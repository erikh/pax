//! OBEX Object Push via BlueZ's `obexd`, spoken directly over the D-Bus **session**
//! bus with `zbus` (because `bluer` 0.17 has no OBEX module).
//!
//! The flow mirrors the `obexd` D-Bus API:
//! 1. `org.bluez.obex.Client1.CreateSession(dest, {"Target": "opp"})` → a session.
//! 2. `org.bluez.obex.ObjectPush1.SendFile(path)` on that session → a transfer
//!    object plus its initial properties (notably `Size`).
//! 3. Watch `org.freedesktop.DBus.Properties.PropertiesChanged` on the transfer for
//!    `Transferred` (progress) and `Status` (`complete` / `error`).
//! 4. `Client1.RemoveSession` to clean up.
//!
//! Runtime requirements: a running `obexd` reachable on the caller's session bus,
//! and a device already paired/connected. This code is exercised by hardware-gated
//! integration tests (see `PAX_HW_TESTS`); the default suite uses the mock backend.

use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

use crate::error::TransportError;

const OBEX_SERVICE: &str = "org.bluez.obex";
const OBEX_ROOT: &str = "/org/bluez/obex";
const CLIENT_IFACE: &str = "org.bluez.obex.Client1";
const PUSH_IFACE: &str = "org.bluez.obex.ObjectPush1";
const TRANSFER_IFACE: &str = "org.bluez.obex.Transfer1";

/// Overall ceiling on a single push, so a wedged transfer cannot hang forever.
const OVERALL_DEADLINE: Duration = Duration::from_secs(300);
/// How long to wait for one `PropertiesChanged` before re-checking `Status`.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

fn zerr(e: zbus::Error) -> TransportError {
    TransportError::Backend(format!("obex: {e}"))
}

/// Pull a `u64` out of a property value regardless of borrowed/owned form.
fn as_u64(v: &Value<'_>) -> Option<u64> {
    match v {
        Value::U64(n) => Some(*n),
        Value::U32(n) => Some(*n as u64),
        _ => None,
    }
}

/// Push `source_path` to `dest` (a `"AA:BB:.."` address string) via Object Push.
///
/// `on_progress` is called with the **absolute** number of bytes transferred each
/// time `obexd` reports progress. Returns the total bytes transferred on success.
pub(crate) async fn object_push(
    dest: &str,
    source_path: &str,
    mut on_progress: impl FnMut(u64),
) -> Result<u64, TransportError> {
    let conn = Connection::session().await.map_err(zerr)?;

    // 1. Create an OPP session to the destination.
    let client = zbus::Proxy::new(&conn, OBEX_SERVICE, OBEX_ROOT, CLIENT_IFACE)
        .await
        .map_err(zerr)?;
    let mut args: HashMap<&str, Value<'_>> = HashMap::new();
    args.insert("Target", Value::from("opp"));
    let session_path: OwnedObjectPath = client
        .call("CreateSession", &(dest, args))
        .await
        .map_err(zerr)?;

    // Run the transfer, then always tear the session down (best-effort) regardless
    // of how it ended — no `Drop` guard, because the cleanup needs to `await`.
    let result = run_transfer(&conn, &session_path, source_path, &mut on_progress).await;
    let _: zbus::Result<()> = client.call("RemoveSession", &(session_path,)).await;
    result
}

/// The transfer half of [`object_push`]: send the file and watch it to completion.
async fn run_transfer(
    conn: &Connection,
    session_path: &OwnedObjectPath,
    source_path: &str,
    on_progress: &mut impl FnMut(u64),
) -> Result<u64, TransportError> {
    // 2. Start the transfer.
    let push = zbus::Proxy::new(conn, OBEX_SERVICE, session_path, PUSH_IFACE)
        .await
        .map_err(zerr)?;
    let (transfer_path, props): (OwnedObjectPath, HashMap<String, OwnedValue>) =
        push.call("SendFile", &(source_path,)).await.map_err(zerr)?;

    let total = props.get("Size").and_then(|v| as_u64(v)).unwrap_or(0);

    // 3. Watch the transfer's properties for progress + terminal status.
    let transfer = zbus::Proxy::new(conn, OBEX_SERVICE, &transfer_path, TRANSFER_IFACE)
        .await
        .map_err(zerr)?;
    let props_proxy = zbus::fdo::PropertiesProxy::builder(conn)
        .destination(OBEX_SERVICE)
        .map_err(zerr)?
        .path(transfer_path.clone())
        .map_err(zerr)?
        .build()
        .await
        .map_err(zerr)?;
    let mut changes = props_proxy
        .receive_properties_changed()
        .await
        .map_err(zerr)?;

    let mut transferred = 0u64;
    let started = tokio::time::Instant::now();

    loop {
        // Terminal-state check first (handles a transfer that finished before or
        // between signals).
        match current_status(&transfer).await {
            Some(s) if s == "complete" => {
                on_progress(total.max(transferred));
                return Ok(total.max(transferred));
            }
            Some(s) if s == "error" => {
                return Err(TransportError::TransferFailed {
                    transferred,
                    reason: "obexd reported transfer error".into(),
                });
            }
            _ => {}
        }

        if started.elapsed() >= OVERALL_DEADLINE {
            return Err(TransportError::Timeout(format!(
                "obex push exceeded {}s",
                OVERALL_DEADLINE.as_secs()
            )));
        }

        // Wait for the next property change, but wake periodically to re-check
        // status so we never block past the deadline.
        let next = tokio::time::timeout(POLL_INTERVAL, changes.next()).await;
        let signal = match next {
            Ok(Some(sig)) => sig,
            Ok(None) => continue,      // stream ended; loop re-checks status
            Err(_elapsed) => continue, // poll timeout; loop re-checks status
        };

        let args = match signal.args() {
            Ok(a) => a,
            Err(_) => continue,
        };
        let changed = args.changed_properties();
        if let Some(v) = changed.get("Transferred").and_then(as_u64) {
            transferred = v;
            on_progress(transferred);
        }
        if let Some(Value::Str(status)) = changed.get("Status") {
            match status.as_str() {
                "complete" => {
                    on_progress(total.max(transferred));
                    return Ok(total.max(transferred));
                }
                "error" => {
                    return Err(TransportError::TransferFailed {
                        transferred,
                        reason: "obexd reported transfer error".into(),
                    });
                }
                _ => {}
            }
        }
    }
}

/// Read the current `Status` property, returning `None` if it cannot be read
/// (e.g. the transfer object already vanished after completing).
async fn current_status(transfer: &zbus::Proxy<'_>) -> Option<String> {
    transfer.get_property::<String>("Status").await.ok()
}
