//! Real cross-platform Bluetooth **Low Energy** backend, built on
//! [`btleplug`](https://docs.rs/btleplug).
//!
//! # Status and scope
//!
//! Compiled only with the `btleplug` cargo feature (off by default). It works on
//! Linux (via BlueZ/D-Bus), macOS (CoreBluetooth), and Windows (WinRT), but it is
//! **LE-only**: it can scan, connect, disconnect, and exchange GATT data, but it
//! cannot perform Classic OBEX Object Push. [`BluetoothBackend::push_file`]
//! therefore returns [`TransportError::Unsupported`] — use the [`bluez`](crate::bluez)
//! backend for file transfer.
//!
//! Like the `bluez` backend, this is the hardware-integration layer and is not run
//! by the workspace's default (hardware-free) test suite; validate it on a real
//! host with `cargo test --features btleplug` and a manual smoke test. It is
//! written against the documented `btleplug` 0.11 API.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use pax_core::{
    AddressType, BdAddr, ChipsetFamily, CompanyId, ControllerModel, DeviceId, DeviceInfo,
    EventOrigin, NoopObserver, SharedObserver, SpecContext, StandardsProfile, Timestamp,
    TransitEvent, TransitEventKind, Transport, Uuid,
};

use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt as _;

use crate::backend::{
    AdapterInfo, BackendKind, BluetoothBackend, Capabilities, Connection, DefaultStandards,
    DiscoveryFilter, GattNotifications, SharedStandardsResolver, StaticStandards,
};
use crate::error::{Result, TransportError};
use crate::pairing::{PairingAgent, PairingOutcome};
use crate::transfer::{OutboundFile, TransferReceipt};

fn map_err(e: btleplug::Error) -> TransportError {
    TransportError::Backend(e.to_string())
}

/// Convert a core [`Uuid`] to a `btleplug`/`uuid` UUID (both big-endian 16 bytes).
fn to_btle_uuid(u: Uuid) -> uuid::Uuid {
    uuid::Uuid::from_bytes(*u.as_bytes())
}

/// Convert a `uuid` UUID to a core [`Uuid`].
fn from_btle_uuid(u: uuid::Uuid) -> Uuid {
    Uuid::from_bytes(*u.as_bytes())
}

/// Convert a `btleplug` address to a core [`BdAddr`].
fn from_btle_addr(a: btleplug::api::BDAddr) -> BdAddr {
    BdAddr::new(a.into_inner())
}

/// A cross-platform BLE-only [`BluetoothBackend`].
pub struct BtleplugBackend {
    adapter: Adapter,
    controller: ControllerModel,
    spec: SpecContext,
    observer: SharedObserver,
    standards: SharedStandardsResolver,
    seq: AtomicU64,
    /// How long [`Self::discover`] scans before collecting results.
    discovery_window: std::time::Duration,
}

impl BtleplugBackend {
    /// Open the first available BLE adapter.
    pub async fn connect_first(observer: SharedObserver) -> Result<Self> {
        let manager = Manager::new().await.map_err(map_err)?;
        let adapters = manager.adapters().await.map_err(map_err)?;
        let adapter = adapters
            .into_iter()
            .next()
            .ok_or_else(|| TransportError::Unavailable("no BLE adapter found".into()))?;

        Ok(BtleplugBackend {
            controller: ControllerModel::new(
                CompanyId(0xFFFF),
                ChipsetFamily::Unknown,
                "btleplug-adapter",
            ),
            spec: SpecContext::new(pax_core::CoreVersion::V5_0, Transport::Le),
            observer,
            standards: Arc::new(DefaultStandards),
            seq: AtomicU64::new(0),
            discovery_window: std::time::Duration::from_secs(5),
            adapter,
        })
    }

    /// Open the first BLE adapter with a no-op observer.
    pub async fn open() -> Result<Self> {
        Self::connect_first(Arc::new(NoopObserver) as SharedObserver).await
    }

    /// Override the controller model used to label events.
    pub fn with_controller(mut self, controller: ControllerModel) -> Self {
        self.controller = controller;
        self
    }

    /// Set how long [`Self::discover`] scans before collecting results.
    pub fn with_discovery_window(mut self, window: std::time::Duration) -> Self {
        self.discovery_window = window;
        self
    }

    /// Supply a [`StandardsResolver`](crate::backend::StandardsResolver) for the
    /// IEEE 802 profile attributed to events. Defaults to [`DefaultStandards`].
    pub fn with_standards_resolver(mut self, resolver: SharedStandardsResolver) -> Self {
        self.standards = resolver;
        self
    }

    /// Attribute one fixed [`StandardsProfile`] to every link.
    pub fn with_standards_override(mut self, profile: StandardsProfile) -> Self {
        self.standards = Arc::new(StaticStandards(profile));
        self
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn emit(&self, peer: DeviceId, kind: TransitEventKind) {
        let origin = EventOrigin::new(
            self.controller.clone(),
            peer,
            self.spec,
            self.standards.resolve(peer),
        );
        let ev = TransitEvent::new(self.next_seq(), Timestamp::now(), origin, kind);
        self.observer.on_event(&ev);
    }

    /// Find a connected peripheral by address.
    async fn find_peripheral(&self, addr: BdAddr) -> Result<Peripheral> {
        for p in self.adapter.peripherals().await.map_err(map_err)? {
            if let Some(props) = p.properties().await.map_err(map_err)? {
                if from_btle_addr(props.address) == addr {
                    return Ok(p);
                }
            }
        }
        Err(TransportError::NotConnected(DeviceId::public(addr)))
    }

    /// Discover services on `peripheral` and resolve a `(service, characteristic)`
    /// pair to a btleplug [`Characteristic`].
    async fn find_characteristic(
        &self,
        peripheral: &Peripheral,
        service: Uuid,
        characteristic: Uuid,
    ) -> Result<Characteristic> {
        peripheral.discover_services().await.map_err(map_err)?;
        let (svc, chr) = (to_btle_uuid(service), to_btle_uuid(characteristic));
        peripheral
            .characteristics()
            .into_iter()
            .find(|c| c.service_uuid == svc && c.uuid == chr)
            .ok_or_else(|| {
                TransportError::Backend(format!("characteristic {characteristic} not found"))
            })
    }
}

#[async_trait]
impl BluetoothBackend for BtleplugBackend {
    fn name(&self) -> &str {
        "btleplug"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            kind: BackendKind::Btleplug,
            transports: vec![Transport::Le],
            can_pair: false,
            can_push_files: false,
            max_concurrent_pairings: 1,
            can_accept_pairings: false,
            can_gatt: true,
        }
    }

    async fn adapter(&self) -> Result<AdapterInfo> {
        // btleplug does not expose the local adapter address portably, so we report
        // the NIL address and the configured controller identity.
        Ok(AdapterInfo {
            address: BdAddr::NIL,
            name: self
                .adapter
                .adapter_info()
                .await
                .unwrap_or_else(|_| "btleplug".into()),
            controller: self.controller.clone(),
            spec: self.spec,
            powered: true,
        })
    }

    async fn set_powered(&self, _on: bool) -> Result<()> {
        // Power management is not portable across btleplug backends.
        Ok(())
    }

    async fn discover(&self, filter: &DiscoveryFilter) -> Result<Vec<DeviceInfo>> {
        self.emit(DeviceId::default(), TransitEventKind::DiscoveryStarted);

        self.adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(map_err)?;
        tokio::time::sleep(self.discovery_window).await;
        let peripherals = self.adapter.peripherals().await.map_err(map_err)?;
        let _ = self.adapter.stop_scan().await;

        let mut out = Vec::new();
        for p in peripherals {
            // Reading one peripheral's properties can fail transiently (a device
            // disappeared mid-scan, or a backend/D-Bus quirk on some platforms).
            // Skip that device rather than abandoning the whole scan.
            let props = match p.properties().await {
                Ok(Some(props)) => props,
                Ok(None) => continue,
                Err(_) => continue,
            };
            let addr_type = match props.address_type {
                Some(btleplug::api::AddressType::Public) => AddressType::Public,
                _ => AddressType::Random,
            };
            let id = DeviceId {
                addr: from_btle_addr(props.address),
                addr_type,
            };
            let mut info = DeviceInfo::new(id);
            info.name = props.local_name;
            info.rssi = props.rssi;
            info.tx_power = props.tx_power_level;
            info.connected = p.is_connected().await.unwrap_or(false);
            // Carry manufacturer data so `info.platform()` can tell an iPhone from
            // an Android device (Apple's company id is the key signal).
            info.manufacturer_data = props
                .manufacturer_data
                .into_iter()
                .map(|(company, data)| (CompanyId(company), data))
                .collect();
            info.services = props.services.iter().map(|u| u.to_string()).collect();

            if !filter.accepts(&info) {
                continue;
            }
            self.emit(id, TransitEventKind::DeviceDiscovered { rssi: info.rssi });
            out.push(info);
            if let Some(limit) = filter.limit {
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    async fn pair(
        &self,
        _target: DeviceId,
        _agent: Arc<dyn PairingAgent>,
    ) -> Result<PairingOutcome> {
        // BLE bonding is handled by the OS bonding manager, not exposed portably by
        // btleplug. Connecting to an encrypted characteristic triggers OS-level
        // pairing implicitly.
        Err(TransportError::Unsupported {
            backend: "btleplug",
            operation: "pair",
        })
    }

    async fn connect(&self, target: DeviceId) -> Result<Connection> {
        let peripherals = self.adapter.peripherals().await.map_err(map_err)?;
        for p in peripherals {
            if let Some(props) = p.properties().await.map_err(map_err)? {
                if from_btle_addr(props.address) == target.addr {
                    p.connect().await.map_err(map_err)?;
                    self.emit(target, TransitEventKind::Connected);
                    return Ok(Connection::new(
                        target,
                        self.controller.clone(),
                        self.spec,
                        self.standards.resolve(target),
                        0,
                    ));
                }
            }
        }
        Err(TransportError::DeviceNotFound(target))
    }

    async fn disconnect(&self, conn: &Connection) -> Result<()> {
        let peripherals = self.adapter.peripherals().await.map_err(map_err)?;
        for p in peripherals {
            if let Some(props) = p.properties().await.map_err(map_err)? {
                if from_btle_addr(props.address) == conn.peer.addr {
                    p.disconnect().await.map_err(map_err)?;
                    self.emit(conn.peer, TransitEventKind::Disconnected { reason: None });
                    return Ok(());
                }
            }
        }
        Err(TransportError::NotConnected(conn.peer))
    }

    async fn push_file(
        &self,
        _conn: &Connection,
        _file: OutboundFile<'_>,
    ) -> Result<TransferReceipt> {
        // OBEX Object Push is a Classic (BR/EDR) profile; a BLE-only backend cannot
        // perform it. Use the `bluez` backend for file transfer.
        Err(TransportError::Unsupported {
            backend: "btleplug",
            operation: "push_file (OBEX Object Push is BR/EDR-only)",
        })
    }

    async fn gatt_read(
        &self,
        conn: &Connection,
        service: Uuid,
        characteristic: Uuid,
    ) -> Result<Vec<u8>> {
        let p = self.find_peripheral(conn.peer.addr).await?;
        let chr = self
            .find_characteristic(&p, service, characteristic)
            .await?;
        p.read(&chr).await.map_err(map_err)
    }

    async fn gatt_write(
        &self,
        conn: &Connection,
        service: Uuid,
        characteristic: Uuid,
        data: &[u8],
        with_response: bool,
    ) -> Result<()> {
        let p = self.find_peripheral(conn.peer.addr).await?;
        let chr = self
            .find_characteristic(&p, service, characteristic)
            .await?;
        let kind = if with_response {
            WriteType::WithResponse
        } else {
            WriteType::WithoutResponse
        };
        p.write(&chr, data, kind).await.map_err(map_err)
    }

    async fn gatt_subscribe(
        &self,
        conn: &Connection,
        service: Uuid,
        characteristic: Uuid,
    ) -> Result<GattNotifications> {
        let p = self.find_peripheral(conn.peer.addr).await?;
        let chr = self
            .find_characteristic(&p, service, characteristic)
            .await?;
        p.subscribe(&chr).await.map_err(map_err)?;
        let want = to_btle_uuid(characteristic);
        // btleplug's notifications() stream owns its source, so it outlives `p`.
        let stream = p
            .notifications()
            .await
            .map_err(map_err)?
            .filter_map(move |n| {
                let item = (n.uuid == want).then(|| (from_btle_uuid(n.uuid), n.value));
                async move { item }
            });
        Ok(Box::pin(stream))
    }
}
