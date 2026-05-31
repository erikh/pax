//! # pax-diagnostics
//!
//! Debug Bluetooth connections *while data is in transit*, split along the
//! dimensions that matter: the **controller hardware model**, the **Bluetooth
//! specification**, and the **IEEE 802 standard** (both the 802.15.x radio lineage
//! and the 802.1X port-auth state).
//!
//! The flow is always the same three steps:
//!
//! 1. Attach a [`Recorder`] (an [`Observer`](pax_core::Observer)) to whatever
//!    backend is doing the work, so it captures the live
//!    [`TransitEvent`](pax_core::TransitEvent) stream.
//! 2. Drive your session (discover / pair / upload) as usual.
//! 3. Hand the captured events to an [`Analyzer`] and ask for a
//!    [`DiagnosticReport`] — or a single [`Breakdown`] along one [`SplitBy`] axis.
//!
//! Because the mock backend (`pax_transport::mock`) emits a deterministic stream,
//! this entire pipeline is exercised with no hardware (see the crate's
//! `tests/end_to_end.rs`).
//!
//! ```
//! use pax_diagnostics::{Analyzer, SplitBy};
//! use pax_core::*;
//!
//! // Two chunks from two different controllers, 1 second apart each.
//! fn chunk(ctrl: &str, bytes: u64, seq: u64, at_ms: u64) -> TransitEvent {
//!     TransitEvent::new(seq, Timestamp::from_millis(at_ms),
//!         EventOrigin::new(ControllerModel::virtual_model(ctrl), DeviceId::default(),
//!             SpecContext::new(CoreVersion::V5_2, Transport::BrEdr), StandardsProfile::bredr_default()),
//!         TransitEventKind::DataChunk { direction: Direction::Outbound, bytes })
//! }
//!
//! let analyzer = Analyzer::new(vec![
//!     chunk("intel-ax210", 1000, 0, 0),
//!     chunk("intel-ax210", 1000, 1, 1000),
//!     chunk("apple-combo", 4000, 2, 0),
//! ]);
//!
//! let by_hw = analyzer.breakdown(SplitBy::Controller);
//! assert_eq!(by_hw.len(), 2);
//! // The Apple controller moved the most bytes.
//! assert!(by_hw.busiest().unwrap().0.contains("apple-combo"));
//!
//! // The full report renders every required axis as a text table.
//! println!("{}", analyzer.report());
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod analyze;
pub mod metrics;
pub mod recorder;

pub use analyze::{Analyzer, Anomaly, Breakdown, DiagnosticReport};
pub use metrics::{Metrics, SplitBy};
pub use recorder::Recorder;
