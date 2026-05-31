//! The analyzer: turn a captured event stream into breakdowns and a report.

use std::collections::BTreeMap;
use std::fmt;

use pax_core::TransitEvent;

use crate::metrics::{Metrics, SplitBy};
use crate::recorder::Recorder;

/// A grouping of events along one [`SplitBy`] dimension: one [`Metrics`] per
/// distinct key. The map is ordered by key so reports are deterministic.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Breakdown {
    /// The dimension that was split on.
    pub dimension: SplitBy,
    /// Metrics keyed by bucket label, in sorted order.
    pub buckets: BTreeMap<String, Metrics>,
}

impl Breakdown {
    /// The number of distinct buckets.
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// Whether there are no buckets (i.e. no events).
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// The bucket with the most payload bytes, if any. Handy for "which controller
    /// / spec / standard carried the most traffic".
    pub fn busiest(&self) -> Option<(&String, &Metrics)> {
        self.buckets.iter().max_by_key(|(_, m)| m.bytes)
    }
}

/// A single flagged concern surfaced by the analyzer.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Anomaly {
    /// A stable, low-cardinality code (e.g. `"error"`, `"warning"`, `"pairing-failed"`).
    pub code: String,
    /// A human-readable description including context.
    pub detail: String,
    /// The sequence number of the event that triggered it.
    pub seq: u64,
}

/// Analyzes a captured [`TransitEvent`] stream. Construct from a [`Recorder`] (or
/// a raw `Vec<TransitEvent>`) and ask for totals, per-dimension breakdowns, or a
/// full [`DiagnosticReport`].
#[derive(Clone, Debug)]
pub struct Analyzer {
    events: Vec<TransitEvent>,
}

impl Analyzer {
    /// Build an analyzer from owned events. They are sorted by `seq` so that
    /// "first"/"last" timestamps are well-defined regardless of arrival order.
    pub fn new(mut events: Vec<TransitEvent>) -> Self {
        events.sort_by_key(|e| e.seq);
        Analyzer { events }
    }

    /// Snapshot a [`Recorder`] into an analyzer.
    pub fn from_recorder(recorder: &Recorder) -> Self {
        Analyzer::new(recorder.events())
    }

    /// The number of events under analysis.
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// A reference to the (seq-sorted) events.
    pub fn events(&self) -> &[TransitEvent] {
        &self.events
    }

    /// Summary metrics across the whole stream.
    pub fn total(&self) -> Metrics {
        Metrics::from_events(&self.events)
    }

    /// Group the stream along `dimension`.
    ///
    /// ```
    /// use pax_diagnostics::{Analyzer, SplitBy};
    /// # use pax_core::*;
    /// # fn ev(ctrl: &str, bytes: u64, seq: u64) -> TransitEvent {
    /// #   TransitEvent::new(seq, Timestamp::from_millis(seq),
    /// #     EventOrigin::new(ControllerModel::virtual_model(ctrl), DeviceId::default(),
    /// #       SpecContext::new(CoreVersion::V5_2, Transport::BrEdr), StandardsProfile::bredr_default()),
    /// #     TransitEventKind::DataChunk { direction: Direction::Outbound, bytes })
    /// # }
    /// let analyzer = Analyzer::new(vec![ev("chip-a", 100, 0), ev("chip-b", 300, 1)]);
    /// let by_hw = analyzer.breakdown(SplitBy::Controller);
    /// assert_eq!(by_hw.len(), 2);
    /// assert_eq!(by_hw.busiest().unwrap().1.bytes, 300);
    /// ```
    pub fn breakdown(&self, dimension: SplitBy) -> Breakdown {
        let mut buckets: BTreeMap<String, Metrics> = BTreeMap::new();
        for event in &self.events {
            buckets
                .entry(dimension.key(event))
                .or_default()
                .record(event);
        }
        Breakdown { dimension, buckets }
    }

    /// Every failure and warning in the stream, as [`Anomaly`] records, in `seq`
    /// order.
    pub fn anomalies(&self) -> Vec<Anomaly> {
        use pax_core::TransitEventKind::*;
        let mut out = Vec::new();
        for e in &self.events {
            let peer = e.origin.peer;
            let ctrl = e.origin.local.label();
            match &e.kind {
                Error { detail } => out.push(Anomaly {
                    code: "error".into(),
                    detail: format!("{detail} (peer {peer}, controller {ctrl})"),
                    seq: e.seq,
                }),
                PairingFailed { reason } => out.push(Anomaly {
                    code: "pairing-failed".into(),
                    detail: format!("{reason} (peer {peer})"),
                    seq: e.seq,
                }),
                Warning { code, detail } => out.push(Anomaly {
                    code: (*code).to_string(),
                    detail: format!("{detail} (peer {peer})"),
                    seq: e.seq,
                }),
                _ => {}
            }
        }
        out
    }

    /// Produce the full multi-dimension report, including the three required split
    /// axes (controller hardware, Bluetooth spec, IEEE 802 standards) plus peers
    /// and event kinds, and the anomaly list.
    pub fn report(&self) -> DiagnosticReport {
        DiagnosticReport {
            total: self.total(),
            by_controller: self.breakdown(SplitBy::Controller),
            by_spec: self.breakdown(SplitBy::Spec),
            by_transport: self.breakdown(SplitBy::Transport),
            by_radio_standard: self.breakdown(SplitBy::RadioStandard),
            by_port_auth: self.breakdown(SplitBy::PortAuth),
            by_event_kind: self.breakdown(SplitBy::EventKind),
            by_peer: self.breakdown(SplitBy::Peer),
            anomalies: self.anomalies(),
        }
    }
}

/// A complete, human-readable diagnostic report. Its [`fmt::Display`] renders a
/// text report suitable for a terminal or a future CLI; the fields are public for
/// programmatic use and (with the `serde` feature) serialization.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DiagnosticReport {
    /// Whole-stream totals.
    pub total: Metrics,
    /// Split by controller hardware model.
    pub by_controller: Breakdown,
    /// Split by Bluetooth spec context.
    pub by_spec: Breakdown,
    /// Split by physical transport.
    pub by_transport: Breakdown,
    /// Split by IEEE 802.15.x radio standard.
    pub by_radio_standard: Breakdown,
    /// Split by IEEE 802.1X port-auth state.
    pub by_port_auth: Breakdown,
    /// Split by event kind.
    pub by_event_kind: Breakdown,
    /// Split by peer device.
    pub by_peer: Breakdown,
    /// Flagged failures and warnings.
    pub anomalies: Vec<Anomaly>,
}

fn render_breakdown(f: &mut fmt::Formatter<'_>, breakdown: &Breakdown) -> fmt::Result {
    writeln!(f, "── {} ──", breakdown.dimension.title())?;
    if breakdown.is_empty() {
        return writeln!(f, "  (no events)");
    }
    // Column widths: key is variable, the rest are fixed and right-aligned.
    let key_w = breakdown
        .buckets
        .keys()
        .map(|k| k.len())
        .max()
        .unwrap_or(3)
        .max(3);
    writeln!(
        f,
        "  {:<kw$}  {:>7}  {:>11}  {:>5}  {:>13}",
        "key",
        "events",
        "bytes",
        "fail",
        "throughput",
        kw = key_w
    )?;
    for (key, m) in &breakdown.buckets {
        writeln!(
            f,
            "  {:<kw$}  {:>7}  {:>11}  {:>5}  {:>10.0} B/s",
            key,
            m.events,
            m.bytes,
            m.failures,
            m.throughput_bytes_per_sec(),
            kw = key_w,
        )?;
    }
    Ok(())
}

impl fmt::Display for DiagnosticReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "═══ pax diagnostic report ═══")?;
        writeln!(
            f,
            "total: {} events, {} bytes, {} failures, {} warnings, span {:?}, {:.0} B/s",
            self.total.events,
            self.total.bytes,
            self.total.failures,
            self.total.warnings,
            self.total.span(),
            self.total.throughput_bytes_per_sec(),
        )?;
        writeln!(f)?;
        // The three required axes first, then the supplementary ones.
        render_breakdown(f, &self.by_controller)?;
        writeln!(f)?;
        render_breakdown(f, &self.by_spec)?;
        writeln!(f)?;
        render_breakdown(f, &self.by_radio_standard)?;
        writeln!(f)?;
        render_breakdown(f, &self.by_port_auth)?;
        writeln!(f)?;
        render_breakdown(f, &self.by_transport)?;
        writeln!(f)?;
        render_breakdown(f, &self.by_event_kind)?;
        writeln!(f)?;
        render_breakdown(f, &self.by_peer)?;
        writeln!(f)?;
        writeln!(f, "── Anomalies ──")?;
        if self.anomalies.is_empty() {
            writeln!(f, "  none")?;
        } else {
            for a in &self.anomalies {
                writeln!(f, "  [#{} {}] {}", a.seq, a.code, a.detail)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pax_core::{
        ControllerModel, CoreVersion, DeviceId, Direction, EventOrigin, SpecContext,
        StandardsProfile, Timestamp, TransitEvent, TransitEventKind, Transport,
    };

    fn data(ctrl: &str, bytes: u64, seq: u64, at_ms: u64) -> TransitEvent {
        TransitEvent::new(
            seq,
            Timestamp::from_millis(at_ms),
            EventOrigin::new(
                ControllerModel::virtual_model(ctrl),
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

    fn error(seq: u64) -> TransitEvent {
        TransitEvent::new(
            seq,
            Timestamp::from_millis(seq),
            EventOrigin::new(
                ControllerModel::virtual_model("mock-0"),
                DeviceId::default(),
                SpecContext::new(CoreVersion::V5_2, Transport::BrEdr),
                StandardsProfile::bredr_default(),
            ),
            TransitEventKind::Error {
                detail: "link lost".into(),
            },
        )
    }

    #[test]
    fn breakdown_by_controller_separates_hardware() {
        let analyzer = Analyzer::new(vec![
            data("chip-a", 100, 0, 0),
            data("chip-a", 100, 1, 10),
            data("chip-b", 500, 2, 20),
        ]);
        let bd = analyzer.breakdown(SplitBy::Controller);
        assert_eq!(bd.len(), 2);
        let busiest = bd.busiest().unwrap();
        assert!(busiest.0.contains("chip-b"));
        assert_eq!(busiest.1.bytes, 500);
    }

    #[test]
    fn anomalies_capture_errors() {
        let analyzer = Analyzer::new(vec![data("c", 10, 0, 0), error(1)]);
        let anomalies = analyzer.anomalies();
        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].code, "error");
        assert_eq!(anomalies[0].seq, 1);
    }

    #[test]
    fn report_renders_all_required_axes() {
        let analyzer = Analyzer::new(vec![data("chip-a", 100, 0, 0), data("chip-a", 100, 1, 100)]);
        let text = analyzer.report().to_string();
        assert!(text.contains("Controller hardware"));
        assert!(text.contains("Bluetooth specification"));
        assert!(text.contains("802.15.x radio standard"));
        assert!(text.contains("802.1X port-auth state"));
        assert!(text.contains("Anomalies"));
    }
}
