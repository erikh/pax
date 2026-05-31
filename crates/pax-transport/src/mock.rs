//! A deterministic, in-memory [`BluetoothBackend`] for testing without hardware.
//!
//! The mock is the reason the whole toolkit can be tested in a connection-free
//! environment. You script a set of [`MockDevice`]s — each with its own spec,
//! controller-visible properties, pairing behavior, and transfer characteristics —
//! and then drive the exact same [`BluetoothBackend`] API your production code
//! uses. Every operation emits the same [`TransitEvent`]s a
//! real backend would, against a **virtual clock** that advances by fixed,
//! configurable steps, so a diagnostics report computed from a mock run is
//! identical on every machine and every run.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use pax_transport::{
//!     BluetoothBackend, DiscoveryFilter,
//!     mock::{MockBackend, MockDevice},
//! };
//! use pax_transport::transfer::OutboundFile;
//! use pax_core::{DeviceId, Observer, SharedObserver, NoopObserver};
//!
//! # async fn run() -> Result<(), pax_transport::TransportError> {
//! let phone: DeviceId = "11:22:33:44:55:66".parse().unwrap();
//! let backend = MockBackend::builder()
//!     .observer(Arc::new(NoopObserver) as SharedObserver)
//!     .device(MockDevice::new(phone, "Test Phone").with_rssi(-55))
//!     .build();
//!
//! // Discover, connect, push a file — all in memory, deterministically.
//! let found = backend.discover(&DiscoveryFilter::new()).await?;
//! assert_eq!(found.len(), 1);
//!
//! let conn = backend.connect(phone).await?;
//! let data = b"the quick brown fox";
//! let receipt = backend.push_file(&conn, OutboundFile::new("note.txt", data)).await?;
//! assert_eq!(receipt.bytes, data.len() as u64);
//! # Ok(())
//! # }
//! # // drive the async example on a tiny executor so the doctest actually runs.
//! # futures_lite_block_on(run()).unwrap();
//! # fn futures_lite_block_on<F: std::future::Future>(f: F) -> F::Output {
//! #     // minimal no-dep block_on for the doctest
//! #     use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
//! #     fn noop(_: *const ()) {}
//! #     fn clone(_: *const ()) -> RawWaker { RawWaker::new(std::ptr::null(), &VT) }
//! #     static VT: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
//! #     let w = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VT)) };
//! #     let mut cx = Context::from_waker(&w);
//! #     let mut f = Box::pin(f);
//! #     loop { if let Poll::Ready(v) = f.as_mut().poll(&mut cx) { return v; } }
//! # }
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;

use pax_core::{
    BdAddr, ControllerModel, DeviceId, DeviceInfo, Direction, Duration, EventOrigin, NoopObserver,
    PairingMethodHint, SharedObserver, SpecContext, StandardsProfile, Timestamp, TransitEvent,
    TransitEventKind, Transport,
};
use std::sync::Arc;

use crate::backend::{
    AdapterInfo, BackendKind, BluetoothBackend, Capabilities, Connection, DiscoveryFilter,
};
use crate::error::{Result, TransportError};
use crate::pairing::{PairingAgent, PairingOutcome, PairingRequest, PairingResponse};
use crate::transfer::{OutboundFile, TransferReceipt};

/// The deterministic passkey the mock uses for passkey/numeric-comparison flows.
pub const MOCK_PASSKEY: u32 = 123_456;

/// A scripted fake device the [`MockBackend`] knows about.
///
/// Construct with [`MockDevice::new`] and tune with the builder methods. The
/// defaults model a healthy Bluetooth 5.2 BR/EDR phone that pairs via "just
/// works" and transfers in 1 KiB chunks at 1 ms/chunk on the virtual clock.
#[derive(Clone, Debug)]
pub struct MockDevice {
    /// The advertised/known properties of the device.
    pub info: DeviceInfo,
    /// The spec the device negotiates.
    pub spec: SpecContext,
    /// The IEEE 802 standards context attributed to its links.
    pub standards: StandardsProfile,
    /// The association model the device will demand during pairing.
    pub pairing_method: PairingMethodHint,
    /// If set, pairing reaches the agent then fails with this reason.
    pub pairing_failure: Option<String>,
    /// Bytes per simulated on-air chunk during a transfer.
    pub chunk_size: u64,
    /// Virtual time charged per chunk (drives simulated throughput).
    pub per_chunk_latency: Duration,
    /// If set, a transfer aborts once it would exceed this many bytes.
    pub transfer_failure_after: Option<u64>,
}

impl MockDevice {
    /// A healthy default device with the given id and friendly name.
    pub fn new(id: DeviceId, name: impl Into<String>) -> Self {
        MockDevice {
            info: DeviceInfo::new(id).with_name(name),
            spec: SpecContext::new(pax_core::CoreVersion::V5_2, Transport::BrEdr),
            standards: StandardsProfile::bredr_default(),
            pairing_method: PairingMethodHint::JustWorks,
            pairing_failure: None,
            chunk_size: 1024,
            per_chunk_latency: Duration::from_millis(1),
            transfer_failure_after: None,
        }
    }

    /// Builder: set the device id directly (and reset the contained `DeviceInfo`).
    pub fn id(&self) -> DeviceId {
        self.info.id
    }

    /// Builder: set RSSI.
    pub fn with_rssi(mut self, dbm: i16) -> Self {
        self.info = self.info.with_rssi(dbm);
        self
    }

    /// Builder: set the spec context.
    pub fn with_spec(mut self, spec: SpecContext) -> Self {
        self.spec = spec;
        self
    }

    /// Builder: set the IEEE 802 standards profile.
    pub fn with_standards(mut self, standards: StandardsProfile) -> Self {
        self.standards = standards;
        self
    }

    /// Builder: choose the pairing association model.
    pub fn with_pairing_method(mut self, method: PairingMethodHint) -> Self {
        self.pairing_method = method;
        self
    }

    /// Builder: make pairing fail (after consulting the agent) with `reason`.
    pub fn failing_pairing(mut self, reason: impl Into<String>) -> Self {
        self.pairing_failure = Some(reason.into());
        self
    }

    /// Builder: set the simulated chunk size and per-chunk latency.
    pub fn with_transfer_profile(mut self, chunk_size: u64, per_chunk: Duration) -> Self {
        self.chunk_size = chunk_size.max(1);
        self.per_chunk_latency = per_chunk;
        self
    }

    /// Builder: abort transfers once they pass `bytes` bytes (to exercise the
    /// failure path in diagnostics).
    pub fn failing_transfer_after(mut self, bytes: u64) -> Self {
        self.transfer_failure_after = Some(bytes);
        self
    }

    fn origin(&self, local: &ControllerModel) -> EventOrigin {
        EventOrigin::new(local.clone(), self.info.id, self.spec, self.standards)
    }
}

/// Internal mutable state, guarded by a single mutex. No `await` is ever held
/// across this lock, so a `std::sync::Mutex` is sufficient and keeps the mock
/// runtime-agnostic.
struct MockState {
    devices: Vec<MockDevice>,
    seq: u64,
    clock_ns: u128,
    connections: HashMap<u64, DeviceId>,
    next_token: u64,
    paired: HashSet<DeviceId>,
}

/// The deterministic in-memory backend. Build one with [`MockBackend::builder`].
pub struct MockBackend {
    name: String,
    adapter_address: BdAddr,
    adapter_name: String,
    controller: ControllerModel,
    spec: SpecContext,
    powered: Mutex<bool>,
    observer: SharedObserver,
    state: Mutex<MockState>,
}

impl MockBackend {
    /// Start configuring a mock backend.
    pub fn builder() -> MockBackendBuilder {
        MockBackendBuilder::new()
    }

    /// The controller model this mock reports (the diagnostic hardware key).
    pub fn controller(&self) -> &ControllerModel {
        &self.controller
    }

    /// Read the current virtual clock without advancing it.
    fn now(&self) -> Timestamp {
        Timestamp::from_nanos(self.state.lock().unwrap().clock_ns)
    }

    /// Emit one event: stamp it with the next seq and the current virtual time,
    /// advance the clock by `dt`, then hand it to the observer (outside the lock).
    fn emit(&self, origin: EventOrigin, kind: TransitEventKind, dt: Duration) {
        let event = {
            let mut st = self.state.lock().unwrap();
            let event = TransitEvent::new(st.seq, Timestamp::from_nanos(st.clock_ns), origin, kind);
            st.seq += 1;
            st.clock_ns += dt.as_nanos();
            event
        };
        self.observer.on_event(&event);
    }

    /// Look up a scripted device by id, cloning it out of the lock.
    fn lookup(&self, id: DeviceId) -> Option<MockDevice> {
        self.state
            .lock()
            .unwrap()
            .devices
            .iter()
            .find(|d| d.info.id == id)
            .cloned()
    }

    /// The origin used for adapter-scoped events (e.g. `DiscoveryStarted`), which
    /// have no single peer.
    fn adapter_origin(&self) -> EventOrigin {
        EventOrigin::new(
            self.controller.clone(),
            DeviceId::default(),
            self.spec,
            StandardsProfile::bredr_default(),
        )
    }

    fn step(&self) -> Duration {
        Duration::from_millis(1)
    }
}

#[async_trait]
impl BluetoothBackend for MockBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            kind: BackendKind::Mock,
            transports: vec![Transport::BrEdr, Transport::Le, Transport::Dual],
            can_pair: true,
            can_push_files: true,
        }
    }

    async fn adapter(&self) -> Result<AdapterInfo> {
        Ok(AdapterInfo {
            address: self.adapter_address,
            name: self.adapter_name.clone(),
            controller: self.controller.clone(),
            spec: self.spec,
            powered: *self.powered.lock().unwrap(),
        })
    }

    async fn set_powered(&self, on: bool) -> Result<()> {
        *self.powered.lock().unwrap() = on;
        Ok(())
    }

    async fn discover(&self, filter: &DiscoveryFilter) -> Result<Vec<DeviceInfo>> {
        self.emit(
            self.adapter_origin(),
            TransitEventKind::DiscoveryStarted,
            self.step(),
        );

        // Snapshot the device list so we are not holding the lock while emitting.
        let devices = self.state.lock().unwrap().devices.clone();

        let mut out = Vec::new();
        for dev in devices {
            // Transport axis: checked here because only the device knows its
            // transport. A `Dual` device matches any requested transport.
            if let Some(t) = filter.transport {
                let d = dev.spec.transport;
                if !(d == t || d == Transport::Dual || t == Transport::Dual) {
                    continue;
                }
            }
            if !filter.accepts(&dev.info) {
                continue;
            }
            if let Some(limit) = filter.limit {
                if out.len() >= limit {
                    break;
                }
            }
            self.emit(
                dev.origin(&self.controller),
                TransitEventKind::DeviceDiscovered {
                    rssi: dev.info.rssi,
                },
                self.step(),
            );
            out.push(dev.info.clone());
        }
        Ok(out)
    }

    async fn pair(&self, target: DeviceId, agent: Arc<dyn PairingAgent>) -> Result<PairingOutcome> {
        let device = self
            .lookup(target)
            .ok_or(TransportError::DeviceNotFound(target))?;
        let origin = device.origin(&self.controller);
        let method = device.pairing_method;

        // Announce the chosen association model before consulting the agent.
        self.emit(
            origin.clone(),
            TransitEventKind::PairingStarted { method },
            self.step(),
        );

        // Translate the method into a concrete prompt and ask the agent. NOTE:
        // the lock is NOT held here — `respond` is async and may block on a human.
        let request = match method {
            PairingMethodHint::JustWorks => PairingRequest::ConfirmJustWorks,
            PairingMethodHint::NumericComparison => PairingRequest::ConfirmPasskey {
                passkey: MOCK_PASSKEY,
            },
            PairingMethodHint::PinCode => PairingRequest::RequestPinCode,
            PairingMethodHint::PasskeyEntry => PairingRequest::RequestPasskey,
            PairingMethodHint::PasskeyDisplay => PairingRequest::DisplayPasskey {
                passkey: MOCK_PASSKEY,
            },
            PairingMethodHint::OutOfBand => PairingRequest::ConfirmJustWorks,
            // `PairingMethodHint` is `#[non_exhaustive]`; treat any future model
            // as a simple confirmation for the mock.
            _ => PairingRequest::ConfirmJustWorks,
        };
        let response = agent.respond(request).await;

        // A rejection from the agent ends pairing before any device-side failure.
        let rejected = matches!(
            response,
            PairingResponse::Confirm(false) | PairingResponse::Cancel
        );
        if rejected {
            let reason = "declined by pairing agent".to_string();
            self.emit(
                origin,
                TransitEventKind::PairingFailed {
                    reason: reason.clone(),
                },
                self.step(),
            );
            return Err(TransportError::PairingRejected(reason));
        }

        if let Some(reason) = device.pairing_failure.clone() {
            self.emit(
                origin,
                TransitEventKind::PairingFailed {
                    reason: reason.clone(),
                },
                self.step(),
            );
            return Err(TransportError::PairingFailed(reason));
        }

        self.state.lock().unwrap().paired.insert(target);
        self.emit(origin, TransitEventKind::PairingCompleted, self.step());
        Ok(PairingOutcome::bonded(method))
    }

    async fn connect(&self, target: DeviceId) -> Result<Connection> {
        let device = self
            .lookup(target)
            .ok_or(TransportError::DeviceNotFound(target))?;
        let token = {
            let mut st = self.state.lock().unwrap();
            let token = st.next_token;
            st.next_token += 1;
            st.connections.insert(token, target);
            token
        };
        self.emit(
            device.origin(&self.controller),
            TransitEventKind::Connected,
            self.step(),
        );
        Ok(Connection::new(
            target,
            self.controller.clone(),
            device.spec,
            device.standards,
            token,
        ))
    }

    async fn disconnect(&self, conn: &Connection) -> Result<()> {
        let removed = self
            .state
            .lock()
            .unwrap()
            .connections
            .remove(&conn.token())
            .is_some();
        if !removed {
            return Err(TransportError::NotConnected(conn.peer));
        }
        self.emit(
            conn.origin(),
            TransitEventKind::Disconnected { reason: None },
            self.step(),
        );
        Ok(())
    }

    async fn push_file(
        &self,
        conn: &Connection,
        file: OutboundFile<'_>,
    ) -> Result<TransferReceipt> {
        // The connection must be live, and we need the device's transfer profile.
        let device = {
            let st = self.state.lock().unwrap();
            if !st.connections.contains_key(&conn.token()) {
                return Err(TransportError::NotConnected(conn.peer));
            }
            st.devices
                .iter()
                .find(|d| d.info.id == conn.peer)
                .cloned()
                .ok_or(TransportError::DeviceNotFound(conn.peer))?
        };

        let origin = conn.origin();
        let total = file.len();
        let start = self.now();

        self.emit(
            origin.clone(),
            TransitEventKind::TransferStarted {
                name: file.name.clone(),
                total_bytes: total,
            },
            self.step(),
        );

        let chunk_size = device.chunk_size.max(1) as usize;
        let mut transferred: u64 = 0;
        let mut chunks: u64 = 0;

        for chunk in file.bytes.chunks(chunk_size) {
            let n = chunk.len() as u64;

            // Simulated mid-flight failure, if scripted.
            if let Some(limit) = device.transfer_failure_after {
                if transferred + n > limit {
                    let reason = format!("simulated link failure at {limit} bytes");
                    self.emit(
                        origin.clone(),
                        TransitEventKind::Error {
                            detail: reason.clone(),
                        },
                        self.step(),
                    );
                    return Err(TransportError::TransferFailed {
                        transferred,
                        reason,
                    });
                }
            }

            self.emit(
                origin.clone(),
                TransitEventKind::DataChunk {
                    direction: Direction::Outbound,
                    bytes: n,
                },
                device.per_chunk_latency,
            );
            transferred += n;
            chunks += 1;
            self.emit(
                origin.clone(),
                TransitEventKind::TransferProgress { transferred, total },
                Duration::ZERO,
            );
        }

        let duration = self.now().saturating_since(start);
        self.emit(
            origin,
            TransitEventKind::TransferCompleted {
                bytes: transferred,
                duration,
            },
            self.step(),
        );

        Ok(TransferReceipt {
            object_name: file.name,
            bytes: transferred,
            chunks,
            duration,
        })
    }
}

/// Builder for [`MockBackend`].
pub struct MockBackendBuilder {
    name: String,
    adapter_address: BdAddr,
    adapter_name: String,
    controller: ControllerModel,
    spec: SpecContext,
    observer: Option<SharedObserver>,
    devices: Vec<MockDevice>,
}

impl MockBackendBuilder {
    fn new() -> Self {
        MockBackendBuilder {
            name: "pax-mock".to_string(),
            adapter_address: BdAddr::new([0x00, 0x00, 0x00, 0x00, 0x00, 0x01]),
            adapter_name: "pax-mock-adapter".to_string(),
            controller: ControllerModel::virtual_model("pax-mock-0"),
            spec: SpecContext::new(pax_core::CoreVersion::V5_2, Transport::BrEdr),
            observer: None,
            devices: Vec::new(),
        }
    }

    /// Set the backend instance name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the local adapter address.
    pub fn adapter_address(mut self, addr: BdAddr) -> Self {
        self.adapter_address = addr;
        self
    }

    /// Set the local adapter friendly name.
    pub fn adapter_name(mut self, name: impl Into<String>) -> Self {
        self.adapter_name = name.into();
        self
    }

    /// Set the controller model the mock reports (the diagnostic hardware key).
    pub fn controller(mut self, model: ControllerModel) -> Self {
        self.controller = model;
        self
    }

    /// Set the adapter's spec context.
    pub fn spec(mut self, spec: SpecContext) -> Self {
        self.spec = spec;
        self
    }

    /// Attach an observer to receive every emitted [`TransitEvent`].
    pub fn observer(mut self, observer: SharedObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Add one scripted device.
    pub fn device(mut self, device: MockDevice) -> Self {
        self.devices.push(device);
        self
    }

    /// Add several scripted devices.
    pub fn devices(mut self, devices: impl IntoIterator<Item = MockDevice>) -> Self {
        self.devices.extend(devices);
        self
    }

    /// Finalize into a [`MockBackend`].
    pub fn build(self) -> MockBackend {
        MockBackend {
            name: self.name,
            adapter_address: self.adapter_address,
            adapter_name: self.adapter_name,
            controller: self.controller,
            spec: self.spec,
            powered: Mutex::new(true),
            observer: self.observer.unwrap_or_else(|| Arc::new(NoopObserver)),
            state: Mutex::new(MockState {
                devices: self.devices,
                seq: 0,
                clock_ns: 0,
                connections: HashMap::new(),
                next_token: 1,
                paired: HashSet::new(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pax_core::Observer;
    use std::sync::Mutex as StdMutex;

    /// A recording observer local to these tests.
    #[derive(Default)]
    struct Rec(StdMutex<Vec<TransitEvent>>);
    impl Observer for Rec {
        fn on_event(&self, e: &TransitEvent) {
            self.0.lock().unwrap().push(e.clone());
        }
    }

    fn phone() -> DeviceId {
        "11:22:33:44:55:66".parse().unwrap()
    }

    async fn build_and_push() -> (Arc<Rec>, TransferReceipt) {
        let rec = Arc::new(Rec::default());
        let backend = MockBackend::builder()
            .observer(rec.clone())
            .device(MockDevice::new(phone(), "Phone").with_rssi(-50))
            .build();
        let conn = backend.connect(phone()).await.unwrap();
        let data = vec![0u8; 4096];
        let receipt = backend
            .push_file(&conn, OutboundFile::new("blob.bin", &data))
            .await
            .unwrap();
        (rec, receipt)
    }

    #[tokio::test]
    async fn push_is_chunked_and_deterministic() {
        let (rec, receipt) = build_and_push().await;
        assert_eq!(receipt.bytes, 4096);
        assert_eq!(receipt.chunks, 4); // 4096 / 1024
                                       // 4 chunks at 1ms each == 4ms of virtual transfer time.
        assert_eq!(receipt.duration.as_millis(), 5); // started step + 4 chunks
        let events = rec.0.lock().unwrap();
        assert!(events.iter().any(|e| e.kind.label() == "transfer-started"));
        assert!(events
            .iter()
            .any(|e| e.kind.label() == "transfer-completed"));
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind.label() == "data-chunk")
                .count(),
            4
        );
    }

    #[tokio::test]
    async fn unknown_device_is_an_error() {
        let backend = MockBackend::builder().build();
        let err = backend.connect(phone()).await.unwrap_err();
        assert!(matches!(err, TransportError::DeviceNotFound(_)));
    }

    #[tokio::test]
    async fn scripted_transfer_failure() {
        let rec = Arc::new(Rec::default());
        let backend = MockBackend::builder()
            .observer(rec.clone())
            .device(MockDevice::new(phone(), "Flaky").failing_transfer_after(2048))
            .build();
        let conn = backend.connect(phone()).await.unwrap();
        let data = vec![0u8; 4096];
        let err = backend
            .push_file(&conn, OutboundFile::new("blob.bin", &data))
            .await
            .unwrap_err();
        match err {
            TransportError::TransferFailed { transferred, .. } => assert_eq!(transferred, 2048),
            other => panic!("unexpected: {other}"),
        }
    }
}
