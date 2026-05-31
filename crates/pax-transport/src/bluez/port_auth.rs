//! A best-effort [`StandardsResolver`](crate::backend::StandardsResolver) that
//! reads 802.1X / PAN authentication state from **NetworkManager** over the D-Bus
//! system bus.
//!
//! A Bluetooth library cannot see 802.1X state on its own — that belongs to the
//! network stack. When a Bluetooth device is bridged onto the network as a PAN
//! (NetworkManager models it as a Bluetooth-type device), NetworkManager's device
//! *state* is a reasonable proxy for "is the bridged port authenticated":
//! `NEED_AUTH` → authenticating, `ACTIVATED` → authenticated, `FAILED` → failed.
//!
//! This is **best-effort and Linux-only** (feature `port-auth-nm`). It maps a
//! coarse device state, not a precise EAP exchange; for exact control supply your
//! own [`StandardsFn`](crate::backend::StandardsFn) instead. Because
//! [`resolve`](crate::backend::StandardsResolver::resolve) must be synchronous,
//! this resolver keeps a cache that you refresh out-of-band (manually, or via
//! `NetworkManagerStandards::spawn_auto_refresh`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pax_core::{BdAddr, DeviceId, PortAuthState, StandardsProfile};
use zbus::zvariant::OwnedObjectPath;
use zbus::Connection;

use crate::backend::StandardsResolver;
use crate::error::TransportError;

const NM_SERVICE: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const NM_DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";

/// NetworkManager `NM_DEVICE_TYPE_BT`.
const NM_DEVICE_TYPE_BT: u32 = 5;
/// NetworkManager `NM_DEVICE_STATE_NEED_AUTH`.
const NM_STATE_NEED_AUTH: u32 = 60;
/// NetworkManager `NM_DEVICE_STATE_ACTIVATED`.
const NM_STATE_ACTIVATED: u32 = 100;
/// NetworkManager `NM_DEVICE_STATE_FAILED`.
const NM_STATE_FAILED: u32 = 120;

fn zerr(e: zbus::Error) -> TransportError {
    TransportError::Backend(format!("networkmanager: {e}"))
}

fn map_state(nm_state: u32) -> PortAuthState {
    match nm_state {
        NM_STATE_NEED_AUTH => PortAuthState::Authenticating,
        NM_STATE_ACTIVATED => PortAuthState::Authenticated,
        NM_STATE_FAILED => PortAuthState::Failed,
        _ => PortAuthState::Unauthenticated,
    }
}

/// A [`StandardsResolver`] backed by a cache of NetworkManager Bluetooth-device
/// port-auth states, keyed by device address.
pub struct NetworkManagerStandards {
    conn: Connection,
    cache: Mutex<HashMap<DeviceId, PortAuthState>>,
}

impl NetworkManagerStandards {
    /// Connect to the system bus. Fails only if the bus itself is unreachable;
    /// missing NetworkManager surfaces later as an empty cache (everything
    /// resolves to [`PortAuthState::NotApplicable`]).
    pub async fn connect() -> Result<Arc<Self>, TransportError> {
        let conn = Connection::system().await.map_err(zerr)?;
        Ok(Arc::new(NetworkManagerStandards {
            conn,
            cache: Mutex::new(HashMap::new()),
        }))
    }

    /// Refresh the cache from NetworkManager: enumerate devices, keep the
    /// Bluetooth ones, and record each one's port-auth state by address.
    pub async fn refresh(&self) -> Result<(), TransportError> {
        let nm = zbus::Proxy::new(&self.conn, NM_SERVICE, NM_PATH, NM_IFACE)
            .await
            .map_err(zerr)?;
        let devices: Vec<OwnedObjectPath> = nm.call("GetDevices", &()).await.map_err(zerr)?;

        let mut fresh: HashMap<DeviceId, PortAuthState> = HashMap::new();
        for dev_path in devices {
            let dev =
                match zbus::Proxy::new(&self.conn, NM_SERVICE, &dev_path, NM_DEVICE_IFACE).await {
                    Ok(d) => d,
                    Err(_) => continue,
                };
            let dtype: u32 = dev.get_property("DeviceType").await.unwrap_or(0);
            if dtype != NM_DEVICE_TYPE_BT {
                continue;
            }
            let hw: String = match dev.get_property("HwAddress").await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let state: u32 = dev.get_property("State").await.unwrap_or(0);
            if let Ok(addr) = hw.parse::<BdAddr>() {
                fresh.insert(DeviceId::public(addr), map_state(state));
            }
        }

        *self.cache.lock().unwrap() = fresh;
        Ok(())
    }

    /// Spawn a background task that calls [`Self::refresh`] every `interval`.
    /// Refresh errors are ignored (best-effort). The task ends when the last
    /// `Arc` to `self` is dropped.
    pub fn spawn_auto_refresh(self: Arc<Self>, interval: Duration) {
        tokio::spawn(async move {
            loop {
                let _ = self.refresh().await;
                tokio::time::sleep(interval).await;
            }
        });
    }
}

impl StandardsResolver for NetworkManagerStandards {
    fn resolve(&self, peer: DeviceId) -> StandardsProfile {
        let port_auth = self
            .cache
            .lock()
            .unwrap()
            .get(&peer)
            .copied()
            .unwrap_or(PortAuthState::NotApplicable);
        StandardsProfile::bredr_default().with_port_auth(port_auth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_mapping() {
        assert_eq!(map_state(NM_STATE_NEED_AUTH), PortAuthState::Authenticating);
        assert_eq!(map_state(NM_STATE_ACTIVATED), PortAuthState::Authenticated);
        assert_eq!(map_state(NM_STATE_FAILED), PortAuthState::Failed);
        assert_eq!(map_state(0), PortAuthState::Unauthenticated);
    }
}
