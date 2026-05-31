//! File-transfer primitives: the outbound object and the completion receipt.
//!
//! These describe a *single* OBEX Object Push. The ergonomic, multi-file, retrying
//! workflow lives in `pax-transfer`; this is the raw unit the backend trait moves.

use pax_core::Duration;

/// A file to push to a peer, as an in-memory object.
///
/// The payload is borrowed (`&'a [u8]`) so a large file mapped or read into a
/// buffer is not copied to hand it to the backend. The `name` is what the peer
/// will see; `mime_type` is an optional OBEX `Type` header.
///
/// ```
/// use pax_transport::transfer::OutboundFile;
/// let data = b"hello, device";
/// let file = OutboundFile::new("greeting.txt", data).with_mime("text/plain");
/// assert_eq!(file.len(), 13);
/// assert_eq!(file.mime_type.as_deref(), Some("text/plain"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundFile<'a> {
    /// The object name presented to the peer (typically a filename).
    pub name: String,
    /// An optional MIME type for the OBEX `Type` header.
    pub mime_type: Option<String>,
    /// The file content.
    pub bytes: &'a [u8],
}

impl<'a> OutboundFile<'a> {
    /// Construct an outbound file from a name and borrowed content.
    pub fn new(name: impl Into<String>, bytes: &'a [u8]) -> Self {
        OutboundFile {
            name: name.into(),
            mime_type: None,
            bytes,
        }
    }

    /// Builder: set the MIME type.
    pub fn with_mime(mut self, mime: impl Into<String>) -> Self {
        self.mime_type = Some(mime.into());
        self
    }

    /// The size of the object in bytes.
    pub fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// Whether the object is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// Proof that a transfer completed, with the numbers needed to reason about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferReceipt {
    /// The object name that was pushed.
    pub object_name: String,
    /// Total bytes moved.
    pub bytes: u64,
    /// How many on-air chunks the object was split into.
    pub chunks: u64,
    /// Wall time the transfer took (deterministic in the mock).
    pub duration: Duration,
}

impl TransferReceipt {
    /// Average throughput in bytes per second. Returns `0.0` for a zero-length
    /// duration to avoid dividing by zero.
    ///
    /// ```
    /// use pax_transport::transfer::TransferReceipt;
    /// use pax_core::Duration;
    /// let r = TransferReceipt {
    ///     object_name: "f.bin".into(), bytes: 1_000_000, chunks: 100,
    ///     duration: Duration::from_secs(2),
    /// };
    /// assert_eq!(r.throughput_bytes_per_sec(), 500_000.0);
    /// ```
    pub fn throughput_bytes_per_sec(&self) -> f64 {
        let secs = self.duration.as_secs_f64();
        if secs <= 0.0 {
            0.0
        } else {
            self.bytes as f64 / secs
        }
    }
}
