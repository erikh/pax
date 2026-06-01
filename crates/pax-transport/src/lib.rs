//! # pax-transport
//!
//! The backend abstraction at the heart of the [`pax`](https://github.com/erikh/pax)
//! toolkit. Everything else is written against one async trait,
//! [`BluetoothBackend`], so the same code path runs against the deterministic
//! in-memory [`mock::MockBackend`] in tests and against real hardware in
//! production.
//!
//! ## The shape of the thing
//!
//! ```text
//!            ┌───────────────────────────────────────────────┐
//!            │             BluetoothBackend (trait)           │
//!            │  discover · pair · connect · push_file · …     │
//!            └───────────────────────────────────────────────┘
//!              ▲                 ▲                      ▲
//!   ┌──────────┘        ┌────────┘            ┌─────────┘
//! ┌─┴───────────┐  ┌────┴───────────┐   ┌─────┴────────────────┐
//! │ MockBackend │  │ BlueZBackend   │   │ BtleplugBackend      │
//! │ (always on) │  │ (feature bluez)│   │ (feature btleplug)   │
//! └─────────────┘  └────────────────┘   └──────────────────────┘
//! ```
//!
//! ## Backends and feature flags
//!
//! | Backend | Feature | Requires | Transports | OBEX push |
//! |---------|---------|----------|------------|-----------|
//! | [`mock::MockBackend`] | *(always available)* | nothing | all (simulated) | yes (simulated) |
//! | `BlueZBackend` (module `bluez`) | `bluez` | Linux, D-Bus dev headers, running `bluetoothd` | BR/EDR + LE | yes |
//! | `BtleplugBackend` (module `btleplug`) | `btleplug` | platform BLE stack | LE only | no |
//!
//! The **default build pulls in no backend but the mock and needs no system
//! libraries**, so `cargo build` / `cargo test` work everywhere. Enable a real
//! backend with, e.g., `cargo build --features bluez`.
//!
//! ## Emitting diagnostics
//!
//! Every backend is constructed with an [`Observer`](pax_core::Observer) and emits
//! [`TransitEvent`](pax_core::TransitEvent)s as it works. Pass a
//! `pax_diagnostics::Recorder` (or any `Observer`) to watch a session; see that
//! crate for analysis.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod backend;
pub mod error;
pub mod mock;
pub mod pairing;
pub mod transfer;

/// Real Linux BlueZ backend. Only present with the `bluez` feature.
#[cfg(feature = "bluez")]
pub mod bluez;

/// Real cross-platform BLE backend. Only present with the `btleplug` feature.
#[cfg(feature = "btleplug")]
pub mod btleplug;

/// Android backend (JNI to `android.bluetooth`). Only present with the `android`
/// feature.
#[cfg(feature = "android")]
pub mod android;

/// iOS backend (CoreBluetooth via btleplug, BLE-only). Only present with the
/// `ios` feature.
#[cfg(feature = "ios")]
pub mod ios;

pub use backend::{
    dump_in_range, AdapterInfo, BackendKind, BluetoothBackend, Capabilities, Connection,
    DefaultStandards, DiscoveryFilter, GattNotifications, InboundPairing, SharedStandardsResolver,
    StandardsFn, StandardsResolver, StaticStandards,
};
pub use error::{Result, TransportError};
pub use pairing::{Decision, PairingAgent, PairingOutcome, PairingRequest, PairingResponse};
pub use transfer::{OutboundFile, TransferReceipt};
