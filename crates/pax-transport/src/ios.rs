//! iOS backend — Bluetooth Low Energy via Apple's CoreBluetooth.
//!
//! # Why this is a thin layer over `btleplug`
//!
//! CoreBluetooth is the only Bluetooth API Apple exposes to third-party apps, and
//! it is **BLE-only**: there is no Classic Bluetooth, no OBEX file push, and no
//! programmatic pairing for app developers. The [`btleplug`](https://docs.rs/btleplug)
//! crate already implements a robust CoreBluetooth backend for iOS (the same one it
//! uses on macOS), so rather than hand-roll Objective-C bindings — which could not
//! even be compiled or checked off an Apple host — the iOS backend wraps
//! [`BtleplugBackend`](crate::btleplug::BtleplugBackend) and presents it with iOS
//! semantics and an Apple controller identity.
//!
//! # What works on iOS
//!
//! * Discover, connect, disconnect, and (through btleplug) GATT.
//! * [`BluetoothBackend::pair`] and [`BluetoothBackend::push_file`] return
//!   [`TransportError::Unsupported`] — these are platform limitations, not
//!   missing features. To send a file to an iPhone you generally use AirDrop or an
//!   app-level GATT protocol, neither of which is Bluetooth Object Push.
//!
//! Build for an iOS target on macOS with Xcode; `cargo build --features ios` also
//! compiles on other hosts because the wrapper itself is host-portable (the
//! CoreBluetooth specifics live inside btleplug, gated to Apple targets).

use std::sync::Arc;

use async_trait::async_trait;

use pax_core::{ChipsetFamily, CompanyId, ControllerModel, DeviceId, DeviceInfo, SharedObserver};

use crate::backend::{
    AdapterInfo, BackendKind, BluetoothBackend, Capabilities, Connection, DiscoveryFilter,
};
use crate::btleplug::BtleplugBackend;
use crate::error::{Result, TransportError};
use crate::pairing::{PairingAgent, PairingOutcome};
use crate::transfer::{OutboundFile, TransferReceipt};

/// An iOS [`BluetoothBackend`] (BLE-only, via CoreBluetooth through btleplug).
pub struct IosBackend {
    inner: BtleplugBackend,
}

impl IosBackend {
    /// Open the iOS BLE adapter, reporting an Apple controller identity.
    pub async fn open(observer: SharedObserver) -> Result<Self> {
        let inner = BtleplugBackend::connect_first(observer)
            .await?
            .with_controller(ControllerModel::new(
                CompanyId::APPLE,
                ChipsetFamily::AppleCombo,
                "CoreBluetooth",
            ));
        Ok(IosBackend { inner })
    }

    /// Access the underlying btleplug backend (e.g. for its discovery-window
    /// tuning) before wrapping.
    pub fn inner(&self) -> &BtleplugBackend {
        &self.inner
    }
}

#[async_trait]
impl BluetoothBackend for IosBackend {
    fn name(&self) -> &str {
        "ios"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            kind: BackendKind::Ios,
            transports: vec![pax_core::Transport::Le],
            can_pair: false,
            can_push_files: false,
        }
    }

    async fn adapter(&self) -> Result<AdapterInfo> {
        self.inner.adapter().await
    }

    async fn set_powered(&self, on: bool) -> Result<()> {
        self.inner.set_powered(on).await
    }

    async fn discover(&self, filter: &DiscoveryFilter) -> Result<Vec<DeviceInfo>> {
        self.inner.discover(filter).await
    }

    async fn pair(
        &self,
        _target: DeviceId,
        _agent: Arc<dyn PairingAgent>,
    ) -> Result<PairingOutcome> {
        Err(TransportError::Unsupported {
            backend: "ios",
            operation: "pair (CoreBluetooth does not expose programmatic pairing)",
        })
    }

    async fn connect(&self, target: DeviceId) -> Result<Connection> {
        self.inner.connect(target).await
    }

    async fn disconnect(&self, conn: &Connection) -> Result<()> {
        self.inner.disconnect(conn).await
    }

    async fn push_file(
        &self,
        _conn: &Connection,
        _file: OutboundFile<'_>,
    ) -> Result<TransferReceipt> {
        Err(TransportError::Unsupported {
            backend: "ios",
            operation: "push_file (iOS exposes BLE only — no Classic OBEX)",
        })
    }
}
