//! High-level file upload orchestration: connect, push (with retries), disconnect.
//!
//! [`BluetoothBackend::push_file`] is
//! the raw single-object primitive. This module wraps it with the lifecycle and
//! resilience an application actually wants: open a connection, push one or many
//! files, retry a transfer that aborts mid-flight, and tidy up the connection
//! afterward.

use pax_core::DeviceId;
use pax_transport::transfer::{OutboundFile, TransferReceipt};
use pax_transport::{BluetoothBackend, Connection, TransportError};

/// Tunables for an upload.
#[derive(Clone, Copy, Debug)]
pub struct UploadOptions {
    /// Extra attempts after the first failed transfer (so `retries: 1` means up to
    /// two attempts per file). Only mid-flight transfer failures are retried.
    pub retries: u32,
    /// Disconnect from the device once the upload finishes (success or failure).
    pub disconnect_after: bool,
}

impl Default for UploadOptions {
    fn default() -> Self {
        UploadOptions {
            retries: 1,
            disconnect_after: true,
        }
    }
}

/// Push a single file to `target`, managing the connection lifecycle.
///
/// ```
/// use pax_core::DeviceId;
/// use pax_transport::mock::{MockBackend, MockDevice};
/// use pax_transport::transfer::OutboundFile;
/// use pax_transfer::{upload_file, UploadOptions};
///
/// # async fn run() -> Result<(), pax_transport::TransportError> {
/// let id: DeviceId = "AA:BB:CC:DD:EE:FF".parse().unwrap();
/// let backend = MockBackend::builder().device(MockDevice::new(id, "Laptop")).build();
///
/// let data = b"report.pdf contents";
/// let receipt = upload_file(
///     &backend, id, OutboundFile::new("report.pdf", data), UploadOptions::default(),
/// ).await?;
/// assert_eq!(receipt.bytes, data.len() as u64);
/// # Ok(()) }
/// ```
pub async fn upload_file(
    backend: &dyn BluetoothBackend,
    target: DeviceId,
    file: OutboundFile<'_>,
    options: UploadOptions,
) -> Result<TransferReceipt, TransportError> {
    let conn = backend.connect(target).await?;
    let result = push_with_retries(backend, &conn, file, options.retries).await;
    if options.disconnect_after {
        // Best-effort: a disconnect failure should not mask the transfer result.
        let _ = backend.disconnect(&conn).await;
    }
    result
}

/// Push several files over a single connection, in order. Aborts on the first
/// file that fails (after retries), returning the error; files already pushed are
/// reflected by their place in the (discarded) sequence — call [`upload_file`] per
/// file if you need per-file results regardless of failures.
///
/// On success returns one [`TransferReceipt`] per input file, in order.
pub async fn upload_files(
    backend: &dyn BluetoothBackend,
    target: DeviceId,
    files: &[OutboundFile<'_>],
    options: UploadOptions,
) -> Result<Vec<TransferReceipt>, TransportError> {
    let conn = backend.connect(target).await?;
    let mut receipts = Vec::with_capacity(files.len());
    let mut error = None;

    for file in files {
        match push_with_retries(backend, &conn, file.clone(), options.retries).await {
            Ok(r) => receipts.push(r),
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }

    if options.disconnect_after {
        let _ = backend.disconnect(&conn).await;
    }

    match error {
        Some(e) => Err(e),
        None => Ok(receipts),
    }
}

/// Convenience: push owned bytes by name. Mirrors [`upload_file`] but takes the
/// payload directly so callers do not have to construct an [`OutboundFile`].
pub async fn upload_bytes(
    backend: &dyn BluetoothBackend,
    target: DeviceId,
    name: impl Into<String>,
    bytes: &[u8],
    options: UploadOptions,
) -> Result<TransferReceipt, TransportError> {
    let name = name.into();
    upload_file(backend, target, OutboundFile::new(name, bytes), options).await
}

/// Retry the raw push on transient transfer failure, leaving the connection open.
async fn push_with_retries(
    backend: &dyn BluetoothBackend,
    conn: &Connection,
    file: OutboundFile<'_>,
    retries: u32,
) -> Result<TransferReceipt, TransportError> {
    let max_attempts = retries.saturating_add(1);
    let mut last_err = None;

    for _ in 0..max_attempts {
        match backend.push_file(conn, file.clone()).await {
            Ok(receipt) => return Ok(receipt),
            // A mid-flight abort might succeed on retry.
            Err(e @ TransportError::TransferFailed { .. }) => last_err = Some(e),
            // Anything else (not connected, unsupported, …) is not retryable.
            Err(e) => return Err(e),
        }
    }

    Err(last_err.unwrap_or_else(|| TransportError::TransferFailed {
        transferred: 0,
        reason: "no attempts made".into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pax_transport::mock::{MockBackend, MockDevice};

    fn id() -> DeviceId {
        "AA:BB:CC:DD:EE:FF".parse().unwrap()
    }

    #[tokio::test]
    async fn single_file_round_trip() {
        let backend = MockBackend::builder()
            .device(MockDevice::new(id(), "x"))
            .build();
        let data = vec![7u8; 3000];
        let receipt = upload_bytes(&backend, id(), "blob.bin", &data, UploadOptions::default())
            .await
            .unwrap();
        assert_eq!(receipt.bytes, 3000);
        assert_eq!(receipt.object_name, "blob.bin");
    }

    #[tokio::test]
    async fn multiple_files_in_order() {
        let backend = MockBackend::builder()
            .device(MockDevice::new(id(), "x"))
            .build();
        let a = OutboundFile::new("a.txt", b"aaaa");
        let b = OutboundFile::new("b.txt", b"bbbbbbbb");
        let receipts = upload_files(&backend, id(), &[a, b], UploadOptions::default())
            .await
            .unwrap();
        assert_eq!(receipts.len(), 2);
        assert_eq!(receipts[0].object_name, "a.txt");
        assert_eq!(receipts[1].bytes, 8);
    }

    #[tokio::test]
    async fn persistent_failure_is_surfaced() {
        let backend = MockBackend::builder()
            .device(MockDevice::new(id(), "flaky").failing_transfer_after(1024))
            .build();
        let data = vec![0u8; 4096];
        let err = upload_bytes(
            &backend,
            id(),
            "big.bin",
            &data,
            UploadOptions {
                retries: 2,
                disconnect_after: true,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransportError::TransferFailed { .. }));
    }
}
