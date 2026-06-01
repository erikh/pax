//! The backend abstraction: one async trait that every Bluetooth implementation
//! (mock or real) satisfies, plus the value types its methods exchange.
//!
//! The whole toolkit is written against [`BluetoothBackend`] and never against a
//! concrete backend, which is what lets it be exercised end-to-end with the
//! in-memory [`crate::mock::MockBackend`] and no hardware.

use std::sync::Arc;

use async_trait::async_trait;

use pax_core::{
    BdAddr, ControllerModel, DeviceId, DeviceInfo, Duration, EventOrigin, SpecContext,
    StandardsProfile, Transport, Uuid,
};

/// A stream of GATT characteristic notifications, each `(characteristic UUID, value)`.
/// Returned by [`BluetoothBackend::gatt_subscribe`].
pub type GattNotifications = std::pin::Pin<Box<dyn futures::Stream<Item = (Uuid, Vec<u8>)> + Send>>;

use crate::error::{Result, TransportError};
use crate::pairing::{PairingAgent, PairingOutcome};
use crate::transfer::{OutboundFile, TransferReceipt};

/// Which concrete backend produced a value. Useful in logs and in
/// [`TransportError::Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BackendKind {
    /// The deterministic in-memory mock.
    Mock,
    /// The Linux BlueZ backend (feature `bluez`).
    BlueZ,
    /// The cross-platform btleplug BLE backend (feature `btleplug`).
    Btleplug,
    /// The Android backend (feature `android`).
    Android,
    /// The iOS backend (feature `ios`).
    Ios,
}

impl BackendKind {
    /// A stable, lowercase name (`"mock"`, `"bluez"`, `"btleplug"`, …).
    pub const fn name(self) -> &'static str {
        match self {
            BackendKind::Mock => "mock",
            BackendKind::BlueZ => "bluez",
            BackendKind::Btleplug => "btleplug",
            BackendKind::Android => "android",
            BackendKind::Ios => "ios",
        }
    }
}

/// What a backend can and cannot do. Query this before driving a workflow so you
/// can fail fast (or pick a different backend) when, say, OBEX file push is not
/// supported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Which concrete backend this is.
    pub kind: BackendKind,
    /// Transports the backend can operate over.
    pub transports: Vec<Transport>,
    /// Whether the backend can initiate pairing/bonding.
    pub can_pair: bool,
    /// Whether the backend can push files via OBEX Object Push.
    pub can_push_files: bool,
    /// How many devices the backend can pair **concurrently** without conflict.
    ///
    /// `1` means pairing is serialized — true for any single real controller,
    /// where one D-Bus pairing agent and one baseband handle one bond at a time.
    /// Higher means it is safe to fan out that many pairings at once (the mock).
    /// The batch pairer reads this to choose concurrency *transparently*, so the
    /// same call runs concurrently where possible and sequentially where not.
    pub max_concurrent_pairings: usize,
    /// Whether the backend can accept **inbound** bonds — i.e. become
    /// discoverable + pairable so other devices pair *to* it
    /// ([`BluetoothBackend::accept_pairings`]).
    pub can_accept_pairings: bool,
    /// Whether the backend supports GATT (BLE) read / write / notify
    /// ([`BluetoothBackend::gatt_read`] etc.).
    pub can_gatt: bool,
    /// Whether the backend can **spoof the local adapter's address** —
    /// i.e. present an arbitrary `BD_ADDR` via
    /// [`BluetoothBackend::set_local_address`].
    ///
    /// `true` means the backend has a mechanism for it (the mock simulates it;
    /// BlueZ uses the kernel mgmt socket). A real controller may still reject the
    /// change at run time, but a `false` here means it is *impossible* — query it
    /// before offering a `--spoof`-style option so you never reach a dead end.
    pub can_spoof_address: bool,
}

impl Capabilities {
    /// `true` if the backend advertises support for `transport`.
    pub fn supports(&self, transport: Transport) -> bool {
        self.transports.contains(&transport)
    }
}

/// A description of the local adapter (the controller this backend is driving).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterInfo {
    /// The local controller's own Bluetooth address.
    pub address: BdAddr,
    /// The adapter's friendly name (e.g. the hostname BlueZ advertises).
    pub name: String,
    /// The controller hardware model — the diagnostic "split by hardware" key.
    pub controller: ControllerModel,
    /// The controller's negotiated/native spec context.
    pub spec: SpecContext,
    /// Whether the adapter is currently powered on.
    pub powered: bool,
}

/// A live (or, in the mock, simulated) connection to a peer.
///
/// A `Connection` is a lightweight handle: it carries the context needed to label
/// events ([`Connection::origin`]) and is passed back to the backend for
/// per-connection operations like [`BluetoothBackend::push_file`]. Backends track
/// the real link state internally and key it by [`Connection::token`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    /// The peer this connection is to.
    pub peer: DeviceId,
    /// The local controller driving the link (for event labeling).
    pub local: ControllerModel,
    /// The negotiated spec context of the link.
    pub spec: SpecContext,
    /// The IEEE 802 standards context of the link.
    pub standards: StandardsProfile,
    /// A backend-internal handle identifying this link. Opaque to callers.
    token: u64,
}

impl Connection {
    /// Construct a connection handle. Normally only backends call this.
    pub fn new(
        peer: DeviceId,
        local: ControllerModel,
        spec: SpecContext,
        standards: StandardsProfile,
        token: u64,
    ) -> Self {
        Connection {
            peer,
            local,
            spec,
            standards,
            token,
        }
    }

    /// The opaque backend handle for this link.
    pub fn token(&self) -> u64 {
        self.token
    }

    /// Build an [`EventOrigin`] for events emitted on this connection.
    pub fn origin(&self) -> EventOrigin {
        EventOrigin::new(self.local.clone(), self.peer, self.spec, self.standards)
    }
}

/// Constraints applied to a discovery scan. All fields are optional; an empty
/// filter (via [`DiscoveryFilter::default`]) matches every device.
#[derive(Clone, Debug, Default)]
pub struct DiscoveryFilter {
    /// Only report devices reachable over this transport.
    pub transport: Option<Transport>,
    /// Only report devices whose name contains this substring (case-insensitive).
    pub name_contains: Option<String>,
    /// Only report devices at or above this RSSI (dBm).
    pub min_rssi: Option<i16>,
    /// Stop after collecting this many devices.
    pub limit: Option<usize>,
}

impl DiscoveryFilter {
    /// An empty filter that matches everything.
    pub fn new() -> Self {
        DiscoveryFilter::default()
    }

    /// Builder: restrict to a transport.
    pub fn transport(mut self, t: Transport) -> Self {
        self.transport = Some(t);
        self
    }

    /// Builder: restrict by name substring.
    pub fn name_contains(mut self, s: impl Into<String>) -> Self {
        self.name_contains = Some(s.into());
        self
    }

    /// Builder: restrict by minimum RSSI.
    pub fn min_rssi(mut self, dbm: i16) -> Self {
        self.min_rssi = Some(dbm);
        self
    }

    /// Builder: cap the number of results.
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Whether `info` satisfies this filter. The `transport` axis is checked by
    /// the backend (which knows each device's transport); the rest are checked
    /// here against the [`DeviceInfo`].
    pub fn accepts(&self, info: &DeviceInfo) -> bool {
        if let Some(min) = self.min_rssi {
            match info.rssi {
                Some(r) if r >= min => {}
                _ => return false,
            }
        }
        if let Some(sub) = &self.name_contains {
            let needle = sub.to_lowercase();
            match &info.name {
                Some(n) if n.to_lowercase().contains(&needle) => {}
                _ => return false,
            }
        }
        true
    }
}

/// Settings for an inbound "pairing mode" run ([`BluetoothBackend::accept_pairings`]).
///
/// The adapter becomes discoverable + pairable and accepts bonds from any device
/// that initiates pairing, until `window` elapses or `max_devices` have bonded.
#[derive(Clone, Copy, Debug)]
pub struct InboundPairing {
    /// How long to stay discoverable + pairable accepting bonds.
    pub window: Duration,
    /// Stop early once this many devices have bonded, if set.
    pub max_devices: Option<usize>,
}

impl InboundPairing {
    /// Accept inbound bonds for `window`, with no device cap.
    pub fn for_window(window: Duration) -> Self {
        InboundPairing {
            window,
            max_devices: None,
        }
    }

    /// Builder: stop after `n` devices have bonded.
    pub fn max_devices(mut self, n: usize) -> Self {
        self.max_devices = Some(n);
        self
    }
}

impl Default for InboundPairing {
    /// A 30-second window with no device cap.
    fn default() -> Self {
        InboundPairing::for_window(Duration::from_secs(30))
    }
}

/// Supplies the IEEE 802 standards context ([`StandardsProfile`]) for a link.
///
/// A Bluetooth library has no inherent way to know a link's 802.1X port-auth
/// state — that lives in the network stack the consumer owns. This hook lets the
/// consumer feed it in, so diagnostics can split by `RadioStandard` / `PortAuth`
/// against a real source. The default ([`DefaultStandards`]) reports a plain
/// Classic profile; supply your own via [`StandardsFn`] or [`StaticStandards`],
/// or enable the `port-auth-nm` feature for a NetworkManager-backed resolver.
///
/// `resolve` is synchronous and must be cheap (it is called on the event hot
/// path). A live resolver should cache state refreshed out-of-band rather than
/// doing I/O here.
pub trait StandardsResolver: Send + Sync {
    /// The standards profile to attribute to events involving `peer`.
    fn resolve(&self, peer: DeviceId) -> StandardsProfile;
}

/// A shared, dynamically-dispatched [`StandardsResolver`].
pub type SharedStandardsResolver = Arc<dyn StandardsResolver>;

/// The default resolver: every link gets [`StandardsProfile::bredr_default`]
/// (802.15.1 radio, port-auth not applicable).
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultStandards;

impl StandardsResolver for DefaultStandards {
    fn resolve(&self, _peer: DeviceId) -> StandardsProfile {
        StandardsProfile::bredr_default()
    }
}

/// A resolver that returns one fixed [`StandardsProfile`] for every peer. Handy
/// when you know the whole session runs over, say, an authenticated PAN bridge.
#[derive(Clone, Copy, Debug)]
pub struct StaticStandards(pub StandardsProfile);

impl StandardsResolver for StaticStandards {
    fn resolve(&self, _peer: DeviceId) -> StandardsProfile {
        self.0
    }
}

/// Bridges a closure into a [`StandardsResolver`], so a consumer can map a peer to
/// a profile from their own state without writing a trait impl.
///
/// ```
/// use pax_core::{DeviceId, PortAuthState, StandardsProfile};
/// use pax_transport::backend::{StandardsFn, StandardsResolver};
///
/// let resolver = StandardsFn::new(|_peer: DeviceId| {
///     StandardsProfile::bredr_default().with_port_auth(PortAuthState::Authenticated)
/// });
/// let id: DeviceId = "00:11:22:33:44:55".parse().unwrap();
/// assert_eq!(resolver.resolve(id).port_auth, PortAuthState::Authenticated);
/// ```
pub struct StandardsFn<F>(F);

impl<F> StandardsFn<F>
where
    F: Fn(DeviceId) -> StandardsProfile + Send + Sync,
{
    /// Wrap a closure as a resolver.
    pub fn new(f: F) -> Self {
        StandardsFn(f)
    }
}

impl<F> StandardsResolver for StandardsFn<F>
where
    F: Fn(DeviceId) -> StandardsProfile + Send + Sync,
{
    fn resolve(&self, peer: DeviceId) -> StandardsProfile {
        (self.0)(peer)
    }
}

/// The single trait every Bluetooth backend implements.
///
/// It is deliberately small: discover, pair, connect, disconnect, push a file,
/// and report the adapter. Higher-level ergonomics (retries, multi-file
/// manifests, progress callbacks, agent policies) live in `pax-pairing` and
/// `pax-transfer`, built *on top* of this trait. That keeps each real backend as
/// thin and auditable as possible.
///
/// Backends emit [`pax_core::TransitEvent`]s to the
/// [`Observer`](pax_core::Observer) they were constructed with, so a diagnostics
/// recorder can watch everything that happens regardless of which backend is in
/// use.
///
/// The trait is object-safe (via the `async-trait` crate); hold a backend as
/// `std::sync::Arc<dyn BluetoothBackend>` to pass it around.
#[async_trait]
pub trait BluetoothBackend: Send + Sync {
    /// A short human label for this backend instance.
    fn name(&self) -> &str;

    /// What this backend can do. Cheap and synchronous.
    fn capabilities(&self) -> Capabilities;

    /// Describe the local adapter.
    async fn adapter(&self) -> Result<AdapterInfo>;

    /// Power the adapter on or off.
    async fn set_powered(&self, on: bool) -> Result<()>;

    /// Set (spoof) the local adapter's public Bluetooth address, so subsequent
    /// scans, pairings, and connections present `addr` as this device's identity.
    ///
    /// This is an **adapter-global** change: every operation after it runs under
    /// `addr` until it is changed again or the controller resets. Only the local
    /// controller is affected — no remote device is touched.
    ///
    /// Default: [`TransportError::Unsupported`]. Backends that can do it advertise
    /// [`Capabilities::can_spoof_address`] and override this. Prefer the
    /// capability-checked [`apply_spoof`] (or the `_as` helpers) over calling this
    /// directly, so an unsupported backend fails fast with a clear error.
    async fn set_local_address(&self, _addr: BdAddr) -> Result<()> {
        Err(TransportError::Unsupported {
            backend: "",
            operation: "set_local_address",
        })
    }

    /// Scan for nearby devices, returning those that match `filter`.
    ///
    /// Implementations emit a `DiscoveryStarted` event followed by one
    /// `DeviceDiscovered` event per matching device.
    async fn discover(&self, filter: &DiscoveryFilter) -> Result<Vec<DeviceInfo>>;

    /// Pair (and bond) with a device, delegating any user interaction (PIN,
    /// passkey confirmation) to `agent`.
    ///
    /// The agent is taken as an `Arc` (not a borrow) because real backends need to
    /// hand it to an OS pairing agent that outlives the call — e.g. the BlueZ
    /// backend registers a D-Bus agent whose callbacks capture a clone of it.
    async fn pair(&self, target: DeviceId, agent: Arc<dyn PairingAgent>) -> Result<PairingOutcome>;

    /// Open a connection to a device, returning a handle for per-link operations.
    async fn connect(&self, target: DeviceId) -> Result<Connection>;

    /// Tear down a connection.
    async fn disconnect(&self, conn: &Connection) -> Result<()>;

    /// Push a single file to a connected device via OBEX Object Push.
    ///
    /// Implementations emit `TransferStarted`, a stream of `DataChunk` /
    /// `TransferProgress` events, and finally `TransferCompleted` (or `Error` on
    /// failure) to the configured observer.
    async fn push_file(&self, conn: &Connection, file: OutboundFile<'_>)
        -> Result<TransferReceipt>;

    /// Make the local adapter discoverable (visible to scans) or not.
    ///
    /// Default: [`TransportError::Unsupported`]. Override on backends that can
    /// accept inbound bonds (see [`Capabilities::can_accept_pairings`]).
    async fn set_discoverable(&self, _on: bool, _timeout: Option<Duration>) -> Result<()> {
        Err(TransportError::Unsupported {
            backend: "",
            operation: "set_discoverable",
        })
    }

    /// Make the local adapter pairable (will accept incoming bonds) or not.
    ///
    /// Default: [`TransportError::Unsupported`].
    async fn set_pairable(&self, _on: bool, _timeout: Option<Duration>) -> Result<()> {
        Err(TransportError::Unsupported {
            backend: "",
            operation: "set_pairable",
        })
    }

    /// Enter **inbound pairing mode** ("broadcast"): become discoverable + pairable
    /// and accept bonds initiated *by other devices*, using `agent` to answer any
    /// prompts, until `opts.window` elapses or `opts.max_devices` have bonded.
    /// Returns the devices that bonded; restores discoverable/pairable off on exit.
    ///
    /// This is the true one-to-many: one call lets many phones pair to this adapter.
    /// Default: [`TransportError::Unsupported`] (only the mock and BlueZ implement it;
    /// BLE bonding and inbound Classic on mobile are OS-managed).
    async fn accept_pairings(
        &self,
        _agent: Arc<dyn PairingAgent>,
        _opts: InboundPairing,
    ) -> Result<Vec<DeviceId>> {
        Err(TransportError::Unsupported {
            backend: "",
            operation: "accept_pairings",
        })
    }

    /// Read a GATT characteristic value on a connected BLE device.
    ///
    /// Default: [`TransportError::Unsupported`]. Implemented by BLE backends
    /// (see [`Capabilities::can_gatt`]).
    async fn gatt_read(
        &self,
        _conn: &Connection,
        _service: Uuid,
        _characteristic: Uuid,
    ) -> Result<Vec<u8>> {
        Err(TransportError::Unsupported {
            backend: "",
            operation: "gatt_read",
        })
    }

    /// Write `data` to a GATT characteristic. `with_response` selects a
    /// write-with-response (acknowledged) vs. write-without-response.
    ///
    /// Default: [`TransportError::Unsupported`].
    async fn gatt_write(
        &self,
        _conn: &Connection,
        _service: Uuid,
        _characteristic: Uuid,
        _data: &[u8],
        _with_response: bool,
    ) -> Result<()> {
        Err(TransportError::Unsupported {
            backend: "",
            operation: "gatt_write",
        })
    }

    /// Subscribe to notifications/indications on a GATT characteristic, returning a
    /// stream of `(characteristic, value)` updates.
    ///
    /// Default: [`TransportError::Unsupported`].
    async fn gatt_subscribe(
        &self,
        _conn: &Connection,
        _service: Uuid,
        _characteristic: Uuid,
    ) -> Result<GattNotifications> {
        Err(TransportError::Unsupported {
            backend: "",
            operation: "gatt_subscribe",
        })
    }
}

/// Scan for **every device in range** and return a detailed, human-readable dump
/// of each — address, inferred platform, signal, Class-of-Device, bond state,
/// vendor data, and service UUIDs (via [`DeviceInfo::dump`](pax_core::DeviceInfo::dump)).
///
/// This is async — it runs a real discovery on the backend. Pass
/// [`DiscoveryFilter::new`] to dump everything, or a filter to narrow it. For
/// structured output instead of text, iterate [`BluetoothBackend::discover`] and
/// serialize the [`DeviceInfo`]s (they are `serde` with the `pax-core/serde` feature).
///
/// ```
/// use pax_transport::{dump_in_range, DiscoveryFilter, mock::{MockBackend, MockDevice}};
/// use pax_core::DeviceId;
///
/// # async fn run() -> Result<(), pax_transport::TransportError> {
/// let id: DeviceId = "11:22:33:44:55:66".parse().unwrap();
/// let backend = MockBackend::builder()
///     .device(MockDevice::new(id, "Pixel 8").with_rssi(-57))
///     .build();
/// let report = dump_in_range(&backend, &DiscoveryFilter::new()).await?;
/// assert!(report.contains("1 device(s) in range"));
/// assert!(report.contains("Pixel 8"));
/// # Ok(()) }
/// ```
pub async fn dump_in_range(
    backend: &dyn BluetoothBackend,
    filter: &DiscoveryFilter,
) -> Result<String> {
    dump_in_range_as(backend, None, filter).await
}

/// Apply an optional local-address **spoof** before an operation.
///
/// `None` is a no-op. `Some(addr)` first checks
/// [`Capabilities::can_spoof_address`] — returning a clear
/// [`TransportError::Unsupported`] naming the backend if it cannot — and then
/// calls [`BluetoothBackend::set_local_address`]. This is the shared,
/// capability-checked entry point the `_as` scan/connect helpers, the pairing
/// workflow, and the CLI all use, so an unsupported request fails fast and
/// uniformly instead of reaching an impossible state.
///
/// ```
/// use pax_transport::{apply_spoof, BluetoothBackend, mock::MockBackend};
/// use pax_core::BdAddr;
///
/// # async fn run() -> Result<(), pax_transport::TransportError> {
/// let backend = MockBackend::builder().build();
/// apply_spoof(&backend, None).await?;                              // no-op
/// apply_spoof(&backend, "02:00:00:11:22:33".parse::<BdAddr>().ok()).await?; // spoof
/// assert_eq!(backend.adapter().await?.address.to_string(), "02:00:00:11:22:33");
/// # Ok(()) }
/// ```
pub async fn apply_spoof(backend: &dyn BluetoothBackend, spoof: Option<BdAddr>) -> Result<()> {
    let Some(addr) = spoof else {
        return Ok(());
    };
    let caps = backend.capabilities();
    if !caps.can_spoof_address {
        return Err(TransportError::Unsupported {
            backend: caps.kind.name(),
            operation: "set_local_address",
        });
    }
    backend.set_local_address(addr).await
}

/// Scan under an optional spoofed local identity: [`apply_spoof`] then
/// [`discover`](BluetoothBackend::discover). The scan is emitted under `spoof`
/// (if any), so probes carry the impersonated address.
pub async fn discover_as(
    backend: &dyn BluetoothBackend,
    spoof: Option<BdAddr>,
    filter: &DiscoveryFilter,
) -> Result<Vec<DeviceInfo>> {
    apply_spoof(backend, spoof).await?;
    backend.discover(filter).await
}

/// Connect under an optional spoofed local identity: [`apply_spoof`] then
/// [`connect`](BluetoothBackend::connect). Use this for GATT and other
/// per-connection work that should run under the impersonated address.
pub async fn connect_as(
    backend: &dyn BluetoothBackend,
    spoof: Option<BdAddr>,
    target: DeviceId,
) -> Result<Connection> {
    apply_spoof(backend, spoof).await?;
    backend.connect(target).await
}

/// Like [`dump_in_range`], but first applies an optional local-address spoof so
/// the scan runs under the impersonated identity. `None` matches
/// [`dump_in_range`] exactly.
pub async fn dump_in_range_as(
    backend: &dyn BluetoothBackend,
    spoof: Option<BdAddr>,
    filter: &DiscoveryFilter,
) -> Result<String> {
    let devices = discover_as(backend, spoof, filter).await?;
    let mut out = format!("=== {} device(s) in range ===\n", devices.len());
    for device in &devices {
        out.push('\n');
        out.push_str(&device.dump());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pax_core::{AddressType, DeviceInfo};

    #[test]
    fn filter_accepts_by_name_and_rssi() {
        let info = DeviceInfo::new(DeviceId {
            addr: "00:1A:7D:DA:71:13".parse().unwrap(),
            addr_type: AddressType::Public,
        })
        .with_name("Pixel 8")
        .with_rssi(-60);

        assert!(DiscoveryFilter::new().accepts(&info));
        assert!(DiscoveryFilter::new().name_contains("pixel").accepts(&info));
        assert!(!DiscoveryFilter::new()
            .name_contains("galaxy")
            .accepts(&info));
        assert!(DiscoveryFilter::new().min_rssi(-70).accepts(&info));
        assert!(!DiscoveryFilter::new().min_rssi(-50).accepts(&info));
    }

    #[test]
    fn backend_kind_names() {
        assert_eq!(BackendKind::Mock.name(), "mock");
        assert_eq!(BackendKind::BlueZ.name(), "bluez");
    }

    /// A minimal backend that advertises *no* spoofing support and uses the
    /// default [`BluetoothBackend::set_local_address`]. Only the methods the
    /// spoofing helpers touch (`capabilities`, `set_local_address`) are
    /// meaningful; the rest just satisfy the trait.
    struct NoSpoof;

    #[async_trait]
    impl BluetoothBackend for NoSpoof {
        fn name(&self) -> &str {
            "no-spoof"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                kind: BackendKind::Btleplug,
                transports: vec![Transport::Le],
                can_pair: false,
                can_push_files: false,
                max_concurrent_pairings: 1,
                can_accept_pairings: false,
                can_gatt: false,
                can_spoof_address: false,
            }
        }
        async fn adapter(&self) -> Result<AdapterInfo> {
            Err(TransportError::Backend("n/a".into()))
        }
        async fn set_powered(&self, _on: bool) -> Result<()> {
            Ok(())
        }
        async fn discover(&self, _filter: &DiscoveryFilter) -> Result<Vec<DeviceInfo>> {
            Ok(vec![])
        }
        async fn pair(
            &self,
            _target: DeviceId,
            _agent: Arc<dyn PairingAgent>,
        ) -> Result<PairingOutcome> {
            Err(TransportError::Backend("n/a".into()))
        }
        async fn connect(&self, _target: DeviceId) -> Result<Connection> {
            Err(TransportError::Backend("n/a".into()))
        }
        async fn disconnect(&self, _conn: &Connection) -> Result<()> {
            Ok(())
        }
        async fn push_file(
            &self,
            _conn: &Connection,
            _file: OutboundFile<'_>,
        ) -> Result<TransferReceipt> {
            Err(TransportError::Backend("n/a".into()))
        }
    }

    #[tokio::test]
    async fn apply_spoof_none_is_a_noop_even_on_unsupported_backend() {
        // No address requested → never consults the capability, never errors.
        apply_spoof(&NoSpoof, None).await.unwrap();
    }

    #[tokio::test]
    async fn apply_spoof_on_unsupported_backend_fails_fast() {
        let addr: BdAddr = "02:00:00:11:22:33".parse().unwrap();
        let err = apply_spoof(&NoSpoof, Some(addr)).await.unwrap_err();
        match err {
            TransportError::Unsupported { backend, operation } => {
                assert_eq!(backend, "btleplug");
                assert_eq!(operation, "set_local_address");
            }
            other => panic!("expected Unsupported, got {other}"),
        }
    }

    #[tokio::test]
    async fn default_set_local_address_is_unsupported() {
        let addr: BdAddr = "02:00:00:11:22:33".parse().unwrap();
        // Calling the trait method directly (bypassing the capability check) still
        // returns Unsupported from the default impl.
        assert!(matches!(
            NoSpoof.set_local_address(addr).await,
            Err(TransportError::Unsupported { .. })
        ));
    }
}
