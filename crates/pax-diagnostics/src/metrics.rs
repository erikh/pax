//! The split dimensions and the per-bucket metrics.
//!
//! "Debug connections, split by X" reduces to two things: a function that maps an
//! event to a bucket key ([`SplitBy::key`]), and an accumulator that summarizes
//! the events in a bucket ([`Metrics`]). The analyzer in [`crate::analyze`] glues
//! them together.

use pax_core::{Duration, Timestamp, TransitEvent};

/// An axis to split an event stream along. These are exactly the dimensions the
/// toolkit is required to debug by: the controller **hardware model**, the
/// **Bluetooth specification**, and the **IEEE 802 standard** (both the 802.15.x
/// radio lineage and the 802.1X port-auth state).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SplitBy {
    /// Group by the local controller hardware (`ControllerModel::label`).
    Controller,
    /// Group by the full Bluetooth spec context (version + transport).
    Spec,
    /// Group by physical transport (BR/EDR vs LE).
    Transport,
    /// Group by the IEEE 802.15.x radio-lineage standard.
    RadioStandard,
    /// Group by the 802.1X port-auth state.
    PortAuth,
    /// Group by event kind label.
    EventKind,
    /// Group by remote peer.
    Peer,
}

impl SplitBy {
    /// A human title for this dimension, used in report headings.
    pub const fn title(self) -> &'static str {
        match self {
            SplitBy::Controller => "Controller hardware",
            SplitBy::Spec => "Bluetooth specification",
            SplitBy::Transport => "Transport",
            SplitBy::RadioStandard => "IEEE 802.15.x radio standard",
            SplitBy::PortAuth => "IEEE 802.1X port-auth state",
            SplitBy::EventKind => "Event kind",
            SplitBy::Peer => "Peer device",
        }
    }

    /// The bucket key for `event` under this dimension.
    pub fn key(self, event: &TransitEvent) -> String {
        let o = &event.origin;
        match self {
            SplitBy::Controller => o.local.label(),
            SplitBy::Spec => o.spec.to_string(),
            SplitBy::Transport => o.spec.transport.to_string(),
            SplitBy::RadioStandard => o.standards.radio.dotted().to_string(),
            SplitBy::PortAuth => o.standards.port_auth.to_string(),
            SplitBy::EventKind => event.kind.label().to_string(),
            SplitBy::Peer => o.peer.to_string(),
        }
    }
}

/// A summary of a set of events: counts, bytes, failures, and the time span they
/// cover (from which throughput is derived).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Metrics {
    /// Number of events counted.
    pub events: u64,
    /// Total application payload bytes (sum of `DataChunk` sizes).
    pub bytes: u64,
    /// Number of failure events (errors and pairing failures).
    pub failures: u64,
    /// Number of warning events.
    pub warnings: u64,
    /// Timestamp of the earliest event, if any.
    pub first_at: Option<Timestamp>,
    /// Timestamp of the latest event, if any.
    pub last_at: Option<Timestamp>,
}

impl Metrics {
    /// Fold one event into the running summary.
    pub fn record(&mut self, event: &TransitEvent) {
        self.events += 1;
        self.bytes += event.kind.payload_bytes();
        if event.kind.is_failure() {
            self.failures += 1;
        }
        if event.kind.label() == "warning" {
            self.warnings += 1;
        }
        self.first_at = Some(match self.first_at {
            Some(t) => t.min(event.at),
            None => event.at,
        });
        self.last_at = Some(match self.last_at {
            Some(t) => t.max(event.at),
            None => event.at,
        });
    }

    /// Build a summary from an iterator of events.
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a TransitEvent>) -> Metrics {
        let mut m = Metrics::default();
        for e in events {
            m.record(e);
        }
        m
    }

    /// The wall-time span covered by the events (last − first). Zero if fewer than
    /// two distinct timestamps were seen.
    pub fn span(&self) -> Duration {
        match (self.first_at, self.last_at) {
            (Some(a), Some(b)) => b.saturating_since(a),
            _ => Duration::ZERO,
        }
    }

    /// Average payload throughput in bytes per second over [`Metrics::span`].
    /// Returns `0.0` when no time elapsed (so it never divides by zero).
    pub fn throughput_bytes_per_sec(&self) -> f64 {
        let secs = self.span().as_secs_f64();
        if secs <= 0.0 {
            0.0
        } else {
            self.bytes as f64 / secs
        }
    }

    /// `true` if any failure was recorded in this bucket.
    pub fn has_failures(&self) -> bool {
        self.failures > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pax_core::{
        ControllerModel, CoreVersion, DeviceId, Direction, EventOrigin, SpecContext,
        StandardsProfile, TransitEventKind, Transport,
    };

    fn chunk(seq: u64, at_ms: u64, bytes: u64) -> TransitEvent {
        TransitEvent::new(
            seq,
            Timestamp::from_millis(at_ms),
            EventOrigin::new(
                ControllerModel::virtual_model("mock-0"),
                DeviceId::default(),
                SpecContext::new(CoreVersion::V5_2, Transport::BrEdr),
                StandardsProfile::bredr_default(),
            ),
            TransitEventKind::DataChunk {
                direction: Direction::Outbound,
                bytes,
            },
        )
    }

    #[test]
    fn throughput_over_span() {
        // 1000 bytes spread over 1s -> 1000 B/s.
        let events = vec![chunk(0, 0, 500), chunk(1, 1000, 500)];
        let m = Metrics::from_events(&events);
        assert_eq!(m.bytes, 1000);
        assert_eq!(m.span().as_millis(), 1000);
        assert_eq!(m.throughput_bytes_per_sec(), 1000.0);
    }

    #[test]
    fn split_keys() {
        let e = chunk(0, 0, 10);
        assert_eq!(SplitBy::Transport.key(&e), "BR/EDR");
        assert_eq!(SplitBy::RadioStandard.key(&e), "802.15.1");
        assert_eq!(SplitBy::PortAuth.key(&e), "n/a");
        assert_eq!(SplitBy::EventKind.key(&e), "data-chunk");
    }
}
