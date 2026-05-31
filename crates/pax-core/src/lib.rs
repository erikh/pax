//! # pax-core
//!
//! Foundational, dependency-light domain types shared by every crate in the
//! [`pax`](https://github.com/erikh/pax) Bluetooth toolkit. This crate contains
//! **no I/O and no async** — it is a pure vocabulary of values you can construct,
//! compare, hash, print, and (optionally) serialize. Everything here compiles on
//! any platform with no Bluetooth hardware present, which is what lets the rest
//! of the toolkit be tested in a connection-free environment.
//!
//! ## What lives here
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`address`] | [`BdAddr`] — the 48-bit Bluetooth device address (`BD_ADDR`) and its [`AddressType`]. |
//! | [`device`]  | [`DeviceId`], [`DeviceInfo`], and [`ClassOfDevice`] — how a remote endpoint is identified and described. |
//! | [`hardware`]| [`ControllerModel`], [`ChipsetFamily`], and [`CompanyId`] — *the model of hardware providing the Bluetooth features*, used to split diagnostics by controller. |
//! | [`spec`]    | [`CoreVersion`], [`Transport`], and [`SpecContext`] — the Bluetooth specification a link negotiated. |
//! | [`standards`] | [`Standard802`], [`StandardsLayer`], [`StandardsProfile`] — the IEEE 802 taxonomy (802.15.x radio and 802.1X access control) used as a diagnostic split dimension. |
//! | [`event`]   | [`TransitEvent`] and friends — the observable record of *data in transit*. |
//! | [`observe`] | [`Observer`] — the sink interface backends call to report [`TransitEvent`]s. |
//! | [`time`]    | [`Timestamp`] and [`Duration`] — a clock-free time vocabulary that keeps tests deterministic. |
//!
//! ## A note on time and determinism
//!
//! The library code in this workspace never reads the wall clock on its own. A
//! [`Timestamp`] is always supplied by the caller (the mock
//! backend uses a deterministic counter; real backends stamp at the I/O
//! boundary). This is deliberate: it means a diagnostic report computed from a
//! recorded event stream is reproducible byte-for-byte in a test.
//!
//! ## Quick taste
//!
//! ```
//! use pax_core::{BdAddr, CoreVersion, Standard802};
//!
//! let addr: BdAddr = "00:1A:7D:DA:71:13".parse().unwrap();
//! assert_eq!(addr.oui(), [0x00, 0x1A, 0x7D]);
//!
//! // Map a raw HCI version byte to a human-meaningful Bluetooth release.
//! assert_eq!(CoreVersion::from_hci(9), Some(CoreVersion::V5_0));
//! assert!(CoreVersion::V5_0.supports_le());
//!
//! // The "802.1x" diagnostic dimension, generalized over the 802 family.
//! assert_eq!(Standard802::PortAuth8021X.dotted(), "802.1X");
//! assert_eq!(Standard802::Wpan80215_1.dotted(), "802.15.1");
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod address;
pub mod device;
pub mod error;
pub mod event;
pub mod hardware;
pub mod observe;
pub mod peer;
pub mod spec;
pub mod standards;
pub mod time;

// A curated prelude-style re-export of the most frequently used types so that
// downstream code can `use pax_core::{BdAddr, DeviceId, ...}` directly.
pub use address::{AddressType, BdAddr};
pub use device::{ClassOfDevice, DeviceId, DeviceInfo, MajorDeviceClass};
pub use error::{Error, Result};
pub use event::{Direction, EventOrigin, PairingMethodHint, TransitEvent, TransitEventKind};
pub use hardware::{ChipsetFamily, CompanyId, ControllerModel};
pub use observe::{NoopObserver, Observer, SharedObserver};
pub use peer::{detect_platform, PeerPlatform};
pub use spec::{CoreVersion, SpecContext, Transport};
pub use standards::{PortAuthState, Standard802, StandardsLayer, StandardsProfile};
pub use time::{Duration, Timestamp};
