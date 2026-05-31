//! # pax-pairing
//!
//! Ergonomic pairing for the [`pax`](https://github.com/erikh/pax) toolkit: a set
//! of ready-made [`PairingAgent`](pax_transport::pairing::PairingAgent)
//! implementations plus a small retrying [`pair_device`] workflow built on
//! [`pax_transport::BluetoothBackend`].
//!
//! ```
//! use std::sync::Arc;
//! use pax_core::DeviceId;
//! use pax_transport::mock::{MockBackend, MockDevice};
//! use pax_pairing::{agents::AcceptAllAgent, pair_device, PairOptions};
//!
//! # async fn run() -> Result<(), pax_transport::TransportError> {
//! let id: DeviceId = "AA:BB:CC:DD:EE:FF".parse().unwrap();
//! let backend = MockBackend::builder().device(MockDevice::new(id, "Headset")).build();
//! let report = pair_device(&backend, id, Arc::new(AcceptAllAgent::new()), PairOptions::default()).await?;
//! assert!(report.outcome.paired);
//! # Ok(()) }
//! ```
//!
//! See [`agents`] for the available policies and [`workflow`] for the runner.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod agents;
pub mod workflow;

pub use agents::{AcceptAllAgent, CallbackAgent, FixedPinAgent, RejectAllAgent};
pub use workflow::{
    pair_device, pair_devices, pair_discovered, BatchPairOptions, PairItem, PairOptions, PairReport,
};
