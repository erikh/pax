//! Operational errors raised by transport backends.

use pax_core::DeviceId;
use thiserror::Error;

/// Anything that can go wrong while talking to (or pretending to talk to) a
/// Bluetooth controller.
///
/// Core-domain parse failures are folded in via [`TransportError::Core`]. Real
/// backends map their native error types onto [`TransportError::Backend`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// A core-domain validation/parse error (e.g. a malformed address).
    #[error(transparent)]
    Core(#[from] pax_core::Error),

    /// The requested backend could not be initialized (missing hardware, no
    /// D-Bus, feature not compiled in, …).
    #[error("backend unavailable: {0}")]
    Unavailable(String),

    /// No device with the given id is known to the backend.
    #[error("device not found: {0}")]
    DeviceNotFound(DeviceId),

    /// An operation needs a live connection but none exists for this device.
    #[error("not connected: {0}")]
    NotConnected(DeviceId),

    /// The pairing agent (or the peer) declined to pair.
    #[error("pairing rejected: {0}")]
    PairingRejected(String),

    /// Pairing started but failed (timeout, mismatch, auth error).
    #[error("pairing failed: {0}")]
    PairingFailed(String),

    /// A file transfer aborted partway through.
    #[error("transfer failed after {transferred} bytes: {reason}")]
    TransferFailed {
        /// Bytes that had been pushed before the failure.
        transferred: u64,
        /// Why it failed.
        reason: String,
    },

    /// The backend does not support this operation (e.g. OBEX push on a BLE-only
    /// backend).
    #[error("operation `{operation}` not supported by the {backend} backend")]
    Unsupported {
        /// The backend name.
        backend: &'static str,
        /// The unsupported operation.
        operation: &'static str,
    },

    /// A catch-all for native backend errors, preserving their message.
    #[error("backend error: {0}")]
    Backend(String),

    /// An operation exceeded its deadline.
    #[error("timed out: {0}")]
    Timeout(String),
}

/// Result alias for transport operations.
pub type Result<T> = std::result::Result<T, TransportError>;
