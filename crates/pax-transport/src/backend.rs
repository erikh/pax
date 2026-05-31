//! The backend abstraction: one async trait that every Bluetooth implementation
//! (mock or real) satisfies, plus the value types its methods exchange.
//!
//! The whole toolkit is written against [`BluetoothBackend`] and never against a
//! concrete backend, which is what lets it be exercised end-to-end with the
//! in-memory [`crate::mock::MockBackend`] and no hardware.

use std::sync::Arc;

use async_trait::async_trait;

use pax_core::{
    BdAddr, ControllerModel, DeviceId, DeviceInfo, EventOrigin, SpecContext, StandardsProfile,
    Transport,
};

use crate::error::Result;
use crate::pairing::{PairingAgent, PairingOutcome};
use crate::transfer::{OutboundFile, TransferReceipt};

/// Which concrete backend produced a value. Useful in logs and in
/// [`TransportError::Unsupported`](crate::TransportError::Unsupported).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BackendKind {
    /// The deterministic in-memory mock.
    Mock,
    /// The Linux BlueZ backend (feature `bluez`).
    BlueZ,
    /// The cross-platform btleplug BLE backend (feature `btleplug`).
    Btleplug,
}

impl BackendKind {
    /// A stable, lowercase name (`"mock"`, `"bluez"`, `"btleplug"`).
    pub const fn name(self) -> &'static str {
        match self {
            BackendKind::Mock => "mock",
            BackendKind::BlueZ => "bluez",
            BackendKind::Btleplug => "btleplug",
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
}
