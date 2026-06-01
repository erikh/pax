//! The observable record of *data in transit*.
//!
//! Every interesting thing a backend does — starting a scan, finishing a pairing,
//! pushing a chunk of a file — is reported as a [`TransitEvent`]. A [`TransitEvent`]
//! is self-describing: it carries not just *what* happened but the full context of
//! *where* (which controller, which spec, which 802 standard, which peer), so that
//! the diagnostics crate can slice a recorded stream along any of those axes
//! without consulting external state.
//!
//! Backends report events by calling [`crate::Observer::on_event`]. The mock
//! backend in `pax-transport` produces a fully deterministic stream, which is what
//! makes the diagnostics layer testable with no hardware.

use core::fmt;
use std::borrow::Cow;

use crate::device::DeviceId;
use crate::hardware::ControllerModel;
use crate::spec::SpecContext;
use crate::standards::StandardsProfile;
use crate::time::{Duration, Timestamp};

/// The fixed context shared by every event on a particular link: which local
/// controller, which peer, under which spec, under which 802 standards.
///
/// This is the bundle of fields diagnostics group by. Keeping it as one struct
/// (rather than four loose fields on every event) makes the split dimensions
/// explicit and keeps [`TransitEvent`] small to clone.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EventOrigin {
    /// The local controller hardware that produced the event.
    pub local: ControllerModel,
    /// The remote peer the event concerns.
    pub peer: DeviceId,
    /// The Bluetooth specification context of the link.
    pub spec: SpecContext,
    /// The IEEE 802 standards context of the link.
    pub standards: StandardsProfile,
}

impl EventOrigin {
    /// Construct an origin from its four axes.
    pub fn new(
        local: ControllerModel,
        peer: DeviceId,
        spec: SpecContext,
        standards: StandardsProfile,
    ) -> Self {
        EventOrigin {
            local,
            peer,
            spec,
            standards,
        }
    }
}

/// The direction of a data chunk relative to the local device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Direction {
    /// Local → peer (e.g. pushing a file out).
    Outbound,
    /// Peer → local (e.g. receiving an OBEX response).
    Inbound,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Direction::Outbound => "out",
            Direction::Inbound => "in",
        })
    }
}

/// A hint at the Secure Simple Pairing association model in use. Surfaced in
/// pairing events so diagnostics can attribute failures to a method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum PairingMethodHint {
    /// "Just Works" — no user interaction, no MITM protection.
    JustWorks,
    /// Legacy fixed PIN code entry.
    PinCode,
    /// Passkey entered on one side.
    PasskeyEntry,
    /// Passkey displayed on one side.
    PasskeyDisplay,
    /// Numeric comparison ("do these match?").
    NumericComparison,
    /// Out-of-band (e.g. NFC) key exchange.
    OutOfBand,
}

impl fmt::Display for PairingMethodHint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PairingMethodHint::JustWorks => "just-works",
            PairingMethodHint::PinCode => "pin-code",
            PairingMethodHint::PasskeyEntry => "passkey-entry",
            PairingMethodHint::PasskeyDisplay => "passkey-display",
            PairingMethodHint::NumericComparison => "numeric-comparison",
            PairingMethodHint::OutOfBand => "oob",
        };
        f.write_str(s)
    }
}

/// What happened. The payload-carrying half of a [`TransitEvent`].
///
/// New variants may be added over time, hence `#[non_exhaustive]`; always include
/// a `_ =>` arm when matching exhaustively in downstream code.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum TransitEventKind {
    /// A discovery/scan was started on the local adapter.
    DiscoveryStarted,
    /// A device was observed during discovery. RSSI in dBm if measured.
    DeviceDiscovered {
        /// Received signal strength, if known.
        rssi: Option<i16>,
    },
    /// Pairing began, using the given association model.
    PairingStarted {
        /// The SSP method hint.
        method: PairingMethodHint,
    },
    /// Pairing/bonding completed successfully.
    PairingCompleted,
    /// Pairing failed.
    PairingFailed {
        /// A human-readable reason.
        reason: String,
    },
    /// A baseband/ACL connection was established.
    Connected,
    /// The connection was torn down.
    Disconnected {
        /// A human-readable reason, if known.
        reason: Option<String>,
    },
    /// An outbound file transfer started.
    TransferStarted {
        /// The object name presented to the peer.
        name: String,
        /// Total size in bytes.
        total_bytes: u64,
    },
    /// Progress on the current transfer.
    TransferProgress {
        /// Bytes transferred so far.
        transferred: u64,
        /// Total bytes for the object.
        total: u64,
    },
    /// The current transfer completed.
    TransferCompleted {
        /// Total bytes moved.
        bytes: u64,
        /// Wall time the transfer took.
        duration: Duration,
    },
    /// A raw block of payload crossed the link in `direction`. This is the
    /// fine-grained "data in transit" event diagnostics use for throughput.
    DataChunk {
        /// Which way the bytes went.
        direction: Direction,
        /// How many bytes were in this chunk.
        bytes: u64,
    },
    /// A non-fatal anomaly worth flagging (e.g. a retransmit, a stall).
    Warning {
        /// A stable short code, e.g. `"stall"` or `"retransmit"`.
        ///
        /// Usually a `&'static str` literal (`"stall".into()` is free); held as a
        /// [`Cow`] so the type can round-trip through `serde` deserialization,
        /// which cannot borrow into a `'static` lifetime.
        code: Cow<'static, str>,
        /// A human-readable detail.
        detail: String,
    },
    /// A fatal error occurred on the link.
    Error {
        /// A human-readable detail.
        detail: String,
    },
}

impl TransitEventKind {
    /// If this event moved payload bytes, how many (counting only application
    /// data, e.g. [`TransitEventKind::DataChunk`] and
    /// [`TransitEventKind::TransferCompleted`]). Returns `0` for control events.
    pub fn payload_bytes(&self) -> u64 {
        match self {
            TransitEventKind::DataChunk { bytes, .. } => *bytes,
            _ => 0,
        }
    }

    /// `true` if this event represents a failure ([`Self::Error`],
    /// [`Self::PairingFailed`]).
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            TransitEventKind::Error { .. } | TransitEventKind::PairingFailed { .. }
        )
    }

    /// A stable, low-cardinality label for the kind, for grouping/counting.
    pub fn label(&self) -> &'static str {
        match self {
            TransitEventKind::DiscoveryStarted => "discovery-started",
            TransitEventKind::DeviceDiscovered { .. } => "device-discovered",
            TransitEventKind::PairingStarted { .. } => "pairing-started",
            TransitEventKind::PairingCompleted => "pairing-completed",
            TransitEventKind::PairingFailed { .. } => "pairing-failed",
            TransitEventKind::Connected => "connected",
            TransitEventKind::Disconnected { .. } => "disconnected",
            TransitEventKind::TransferStarted { .. } => "transfer-started",
            TransitEventKind::TransferProgress { .. } => "transfer-progress",
            TransitEventKind::TransferCompleted { .. } => "transfer-completed",
            TransitEventKind::DataChunk { .. } => "data-chunk",
            TransitEventKind::Warning { .. } => "warning",
            TransitEventKind::Error { .. } => "error",
        }
    }
}

/// A single, fully-contextualized observation from a link.
///
/// `seq` is a monotonically increasing counter assigned by the producer, giving a
/// total order even when two events share a timestamp. `at` is the producer's
/// timestamp (deterministic in the mock, wall-clock in real backends).
///
/// ```
/// use pax_core::{
///     TransitEvent, TransitEventKind, EventOrigin, ControllerModel, DeviceId,
///     SpecContext, CoreVersion, Transport, StandardsProfile, Timestamp,
/// };
///
/// let origin = EventOrigin::new(
///     ControllerModel::virtual_model("mock-0"),
///     "00:1A:7D:DA:71:13".parse::<DeviceId>().unwrap(),
///     SpecContext::new(CoreVersion::V5_2, Transport::BrEdr),
///     StandardsProfile::bredr_default(),
/// );
/// let ev = TransitEvent::new(0, Timestamp::ZERO, origin, TransitEventKind::Connected);
/// assert_eq!(ev.kind.label(), "connected");
/// assert!(!ev.kind.is_failure());
/// ```
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TransitEvent {
    /// A producer-assigned, monotonically increasing sequence number.
    pub seq: u64,
    /// When the event was produced.
    pub at: Timestamp,
    /// The link context (controller / peer / spec / standards).
    pub origin: EventOrigin,
    /// What happened.
    pub kind: TransitEventKind,
}

impl TransitEvent {
    /// Assemble an event from its parts.
    pub fn new(seq: u64, at: Timestamp, origin: EventOrigin, kind: TransitEventKind) -> Self {
        TransitEvent {
            seq,
            at,
            origin,
            kind,
        }
    }
}

impl fmt::Display for TransitEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "#{:<4} {:?} [{}] {} <{}> {:?}",
            self.seq,
            self.at,
            self.origin.local.label(),
            self.origin.peer,
            self.origin.spec,
            self.kind,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::ControllerModel;
    use crate::spec::{CoreVersion, Transport};

    fn origin() -> EventOrigin {
        EventOrigin::new(
            ControllerModel::virtual_model("mock-0"),
            "00:1A:7D:DA:71:13".parse().unwrap(),
            SpecContext::new(CoreVersion::V5_2, Transport::BrEdr),
            StandardsProfile::bredr_default(),
        )
    }

    #[test]
    fn payload_accounting() {
        let chunk = TransitEvent::new(
            1,
            Timestamp::ZERO,
            origin(),
            TransitEventKind::DataChunk {
                direction: Direction::Outbound,
                bytes: 512,
            },
        );
        assert_eq!(chunk.kind.payload_bytes(), 512);

        let connected =
            TransitEvent::new(0, Timestamp::ZERO, origin(), TransitEventKind::Connected);
        assert_eq!(connected.kind.payload_bytes(), 0);
    }

    #[test]
    fn failure_detection() {
        let err = TransitEvent::new(
            2,
            Timestamp::ZERO,
            origin(),
            TransitEventKind::Error {
                detail: "link lost".into(),
            },
        );
        assert!(err.kind.is_failure());
    }
}
