//! Real Linux Bluetooth backend, built on [BlueZ](https://www.bluez.org/) via the
//! [`bluer`](https://docs.rs/bluer) crate.
//!
//! # Status and build requirements
//!
//! This module is compiled **only** with the `bluez` cargo feature, which is off
//! by default. It requires, at build time, the D-Bus development headers
//! (`libdbus-1-dev` / `dbus-devel`) and, at run time, a running `bluetoothd` (plus
//! `obexd` for file push) and a Bluetooth adapter. It builds and lints in CI; its
//! *runtime* behavior is exercised by the `PAX_HW_TESTS`-gated tests in
//! `tests/hardware.rs` and the README's manual smoke test, since CI has no adapter.
//! The deterministic [`MockBackend`](crate::mock::MockBackend) mirrors its event
//! behavior so the pairing/transfer/diagnostics layers are validated without it.
//!
//! # What it does
//!
//! * `BlueZBackend::connect_default` binds the default adapter and **auto-detects**
//!   the controller's manufacturer/family (BlueZ modalias) and Bluetooth version
//!   (kernel mgmt socket via `pax-hci`), so diagnostics key on the real hardware.
//! * `discover` runs a timed inquiry and reports devices.
//! * `pair` registers a real BlueZ D-Bus agent whose callbacks are **bridged to the
//!   caller's `PairingAgent`** — PIN, passkey, and numeric comparison prompts reach
//!   your policy, not the system agent.
//! * `push_file` performs an OBEX Object Push via `obexd` (see the `obex`
//!   submodule), emitting **per-packet progress** from the transfer's
//!   `Transferred` property.
//! * The IEEE 802 profile on every event comes from a `StandardsResolver` (default
//!   `DefaultStandards`); see `with_standards_resolver` and the optional
//!   `port-auth-nm` `port_auth` resolver.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use pax_core::{
    BdAddr, ChipsetFamily, CompanyId, ControllerModel, DeviceId, DeviceInfo, Direction, Duration,
    EventOrigin, NoopObserver, SharedObserver, SpecContext, StandardsProfile, Timestamp,
    TransitEvent, TransitEventKind, Transport,
};

use crate::backend::{
    AdapterInfo, BackendKind, BluetoothBackend, Capabilities, Connection, DefaultStandards,
    DiscoveryFilter, InboundPairing, SharedStandardsResolver, StaticStandards,
};
use crate::error::{Result, TransportError};
use crate::pairing::{PairingAgent, PairingOutcome, PairingRequest, PairingResponse};
use crate::transfer::{OutboundFile, TransferReceipt};

/// OBEX Object Push over BlueZ's `obexd` (D-Bus session bus). `bluer` itself has
/// no OBEX support, so this is implemented directly with `zbus`.
mod obex;

/// A best-effort 802.1X / PAN port-auth resolver backed by NetworkManager.
#[cfg(feature = "port-auth-nm")]
pub mod port_auth;

/// A precise 802.1X / EAP port-auth resolver backed by wpa_supplicant.
#[cfg(feature = "port-auth-wpa")]
pub mod wpa;

/// The future type a `bluer` agent callback returns: a boxed, pinned, `Send`
/// future yielding a `bluer` agent request result.
type ReqFuture<T> = std::pin::Pin<
    Box<dyn std::future::Future<Output = std::result::Result<T, bluer::agent::ReqError>> + Send>,
>;

/// Shared cell recording which SSP method the peer invoked (for outbound `pair`).
type MethodSeen = Arc<std::sync::Mutex<Option<pax_core::PairingMethodHint>>>;
/// Shared set of devices that authorized a bond (for inbound `accept_pairings`).
type BondedSet = Arc<std::sync::Mutex<std::collections::HashSet<DeviceId>>>;

/// Convert a [`bluer::Address`] into a core [`BdAddr`].
fn from_bluer_addr(a: bluer::Address) -> BdAddr {
    BdAddr::new(a.0)
}

/// Convert a core [`BdAddr`] into a [`bluer::Address`].
fn to_bluer_addr(a: BdAddr) -> bluer::Address {
    bluer::Address::new(a.octets())
}

/// Best-effort detection of the local controller's manufacturer and Bluetooth
/// version. The kernel management socket (via `pax-hci`) is authoritative for
/// both; BlueZ's modalias is a fallback for the manufacturer. Anything that
/// cannot be determined is left for the caller's default / override.
async fn detect_controller(adapter: &bluer::Adapter) -> (CompanyId, Option<pax_core::CoreVersion>) {
    let mut company: Option<CompanyId> = None;
    let mut version: Option<pax_core::CoreVersion> = None;

    // The adapter name is like "hci0"; the trailing number is the HCI index.
    if let Some(index) = adapter
        .name()
        .strip_prefix("hci")
        .and_then(|n| n.parse::<u16>().ok())
    {
        if let Some(info) = pax_hci::read_controller_info(index) {
            company = Some(CompanyId(info.manufacturer));
            version = pax_core::CoreVersion::from_hci(info.hci_version);
        }
    }

    // Fall back to the modalias vendor for the manufacturer if mgmt was
    // unavailable (e.g. no CAP_NET_ADMIN).
    if company.is_none() {
        if let Ok(Some(modalias)) = adapter.modalias().await {
            company = Some(CompanyId(modalias.vendor as u16));
        }
    }

    (company.unwrap_or(CompanyId(0xFFFF)), version)
}

/// Map any `bluer` error onto a [`TransportError`].
fn map_err(e: bluer::Error) -> TransportError {
    TransportError::Backend(e.to_string())
}

/// A real BlueZ-backed [`BluetoothBackend`].
pub struct BlueZBackend {
    session: bluer::Session,
    adapter: bluer::Adapter,
    controller: ControllerModel,
    spec: SpecContext,
    observer: SharedObserver,
    standards: SharedStandardsResolver,
    seq: AtomicU64,
    /// How long [`Self::discover`] scans before returning.
    discovery_window: std::time::Duration,
}

impl BlueZBackend {
    /// Open a session against the host's default adapter.
    ///
    /// `observer` receives every [`TransitEvent`]; pass
    /// `std::sync::Arc::new(pax_core::NoopObserver)` if you do not need
    /// diagnostics.
    pub async fn connect_default(observer: SharedObserver) -> Result<Self> {
        let session = bluer::Session::new().await.map_err(map_err)?;
        let adapter = session.default_adapter().await.map_err(map_err)?;
        adapter.set_powered(true).await.map_err(map_err)?;

        // Best-effort auto-detection of the controller's real identity (the
        // diagnostic "split by hardware" key). Anything we cannot determine falls
        // back to a sensible default; callers can always override with
        // [`Self::with_controller`] / [`Self::with_spec`].
        let alias = adapter.alias().await.map_err(map_err)?;
        let (company, version) = detect_controller(&adapter).await;
        let controller = ControllerModel::new(company, ChipsetFamily::from_company(company), alias);
        let spec = SpecContext::new(
            version.unwrap_or(pax_core::CoreVersion::V5_0),
            Transport::Dual,
        );

        Ok(BlueZBackend {
            session,
            adapter,
            controller,
            spec,
            observer,
            standards: Arc::new(DefaultStandards),
            seq: AtomicU64::new(0),
            discovery_window: std::time::Duration::from_secs(8),
        })
    }

    /// Override the controller model used to label events (the diagnostic
    /// hardware key). Use this when you know the exact chipset.
    pub fn with_controller(mut self, controller: ControllerModel) -> Self {
        self.controller = controller;
        self
    }

    /// Override the spec context attributed to links.
    pub fn with_spec(mut self, spec: SpecContext) -> Self {
        self.spec = spec;
        self
    }

    /// Set how long [`Self::discover`] scans before returning.
    pub fn with_discovery_window(mut self, window: std::time::Duration) -> Self {
        self.discovery_window = window;
        self
    }

    /// Supply a [`StandardsResolver`](crate::backend::StandardsResolver) so events
    /// carry a real IEEE 802 profile (e.g. the 802.1X port-auth state of a bridged
    /// PAN). Defaults to [`DefaultStandards`].
    pub fn with_standards_resolver(mut self, resolver: SharedStandardsResolver) -> Self {
        self.standards = resolver;
        self
    }

    /// Attribute one fixed [`StandardsProfile`] to every link. Shorthand for a
    /// [`StaticStandards`] resolver.
    pub fn with_standards_override(mut self, profile: StandardsProfile) -> Self {
        self.standards = Arc::new(StaticStandards(profile));
        self
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// The controller's HCI index, parsed from the adapter name (e.g. `hci0` → 0).
    /// Needed to address the kernel mgmt socket for [`Self::set_local_address`].
    fn hci_index(&self) -> Option<u16> {
        self.adapter
            .name()
            .strip_prefix("hci")
            .and_then(|n| n.parse::<u16>().ok())
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

    async fn device_info(&self, addr: bluer::Address) -> Result<DeviceInfo> {
        let dev = self.adapter.device(addr).map_err(map_err)?;
        let id = DeviceId::public(from_bluer_addr(addr));
        let mut info = DeviceInfo::new(id);
        info.name = dev.name().await.map_err(map_err)?;
        info.rssi = dev.rssi().await.map_err(map_err)?;
        info.paired = dev.is_paired().await.map_err(map_err)?;
        info.connected = dev.is_connected().await.map_err(map_err)?;
        if let Some(class) = dev.class().await.map_err(map_err)? {
            info.class = Some(pax_core::ClassOfDevice::new(class));
        }
        if let Ok(Some(uuids)) = dev.uuids().await {
            info.services = uuids.iter().map(|u| u.to_string()).collect();
        }
        Ok(info)
    }

    /// Build a BlueZ D-Bus agent that bridges to the toolkit's `PairingAgent`.
    ///
    /// Returns the agent plus two shared cells the callbacks populate:
    /// `method_seen` (the SSP model the peer invoked — used by outbound `pair`),
    /// and `bonded` (the set of devices that authorized a bond — used by inbound
    /// `accept_pairings`). Each callback records into both, so the same agent
    /// serves both directions.
    fn build_bluer_agent(
        &self,
        agent: Arc<dyn PairingAgent>,
    ) -> (bluer::agent::Agent, MethodSeen, BondedSet) {
        use bluer::agent::{
            Agent, ReqError, RequestAuthorization, RequestConfirmation, RequestPasskey,
            RequestPinCode,
        };
        use pax_core::PairingMethodHint;

        let method_seen: MethodSeen = Arc::new(std::sync::Mutex::new(None::<PairingMethodHint>));
        let bonded: BondedSet = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));

        let (a, m, b) = (agent.clone(), method_seen.clone(), bonded.clone());
        let request_confirmation = Box::new(move |req: RequestConfirmation| {
            let (a, m, b) = (a.clone(), m.clone(), b.clone());
            Box::pin(async move {
                *m.lock().unwrap() = Some(PairingMethodHint::NumericComparison);
                match a
                    .respond(PairingRequest::ConfirmPasskey {
                        passkey: req.passkey,
                    })
                    .await
                {
                    PairingResponse::Confirm(true) => {
                        b.lock()
                            .unwrap()
                            .insert(DeviceId::public(from_bluer_addr(req.device)));
                        Ok(())
                    }
                    PairingResponse::Cancel => Err(ReqError::Canceled),
                    _ => Err(ReqError::Rejected),
                }
            }) as ReqFuture<()>
        });

        let (a, m, b) = (agent.clone(), method_seen.clone(), bonded.clone());
        let request_pin_code = Box::new(move |req: RequestPinCode| {
            let (a, m, b) = (a.clone(), m.clone(), b.clone());
            Box::pin(async move {
                *m.lock().unwrap() = Some(PairingMethodHint::PinCode);
                match a.respond(PairingRequest::RequestPinCode).await {
                    PairingResponse::Pin(pin) => {
                        b.lock()
                            .unwrap()
                            .insert(DeviceId::public(from_bluer_addr(req.device)));
                        Ok(pin)
                    }
                    PairingResponse::Cancel => Err(ReqError::Canceled),
                    _ => Err(ReqError::Rejected),
                }
            }) as ReqFuture<String>
        });

        let (a, m, b) = (agent.clone(), method_seen.clone(), bonded.clone());
        let request_passkey = Box::new(move |req: RequestPasskey| {
            let (a, m, b) = (a.clone(), m.clone(), b.clone());
            Box::pin(async move {
                *m.lock().unwrap() = Some(PairingMethodHint::PasskeyEntry);
                match a.respond(PairingRequest::RequestPasskey).await {
                    PairingResponse::Passkey(p) => {
                        b.lock()
                            .unwrap()
                            .insert(DeviceId::public(from_bluer_addr(req.device)));
                        Ok(p)
                    }
                    PairingResponse::Cancel => Err(ReqError::Canceled),
                    _ => Err(ReqError::Rejected),
                }
            }) as ReqFuture<u32>
        });

        let (a, m, b) = (agent.clone(), method_seen.clone(), bonded.clone());
        let request_authorization = Box::new(move |req: RequestAuthorization| {
            let (a, m, b) = (a.clone(), m.clone(), b.clone());
            Box::pin(async move {
                *m.lock().unwrap() = Some(PairingMethodHint::JustWorks);
                match a.respond(PairingRequest::ConfirmJustWorks).await {
                    PairingResponse::Confirm(true) => {
                        b.lock()
                            .unwrap()
                            .insert(DeviceId::public(from_bluer_addr(req.device)));
                        Ok(())
                    }
                    PairingResponse::Cancel => Err(ReqError::Canceled),
                    _ => Err(ReqError::Rejected),
                }
            }) as ReqFuture<()>
        });

        let bluer_agent = Agent {
            // Become the default agent so BlueZ routes prompts here for the
            // duration; dropping the handle restores the previous agent.
            request_default: true,
            request_pin_code: Some(request_pin_code),
            request_passkey: Some(request_passkey),
            request_confirmation: Some(request_confirmation),
            request_authorization: Some(request_authorization),
            ..Default::default()
        };
        (bluer_agent, method_seen, bonded)
    }
}

#[async_trait]
impl BluetoothBackend for BlueZBackend {
    fn name(&self) -> &str {
        "bluez"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            kind: BackendKind::BlueZ,
            transports: vec![Transport::BrEdr, Transport::Le, Transport::Dual],
            can_pair: true,
            can_push_files: true,
            // One controller + one default D-Bus agent => pairing is serialized.
            max_concurrent_pairings: 1,
            can_accept_pairings: true,
            // GATT over BlueZ is possible but not implemented in this backend yet.
            can_gatt: false,
            // The kernel mgmt socket (via `pax-hci`) can reprogram the controller
            // address. Needs CAP_NET_ADMIN and a controller whose driver supports
            // it; a run-time rejection surfaces as `TransportError::LocalAddress`.
            can_spoof_address: true,
        }
    }

    async fn adapter(&self) -> Result<AdapterInfo> {
        Ok(AdapterInfo {
            address: from_bluer_addr(self.adapter.address().await.map_err(map_err)?),
            name: self.adapter.alias().await.map_err(map_err)?,
            controller: self.controller.clone(),
            spec: self.spec,
            powered: self.adapter.is_powered().await.map_err(map_err)?,
        })
    }

    async fn set_powered(&self, on: bool) -> Result<()> {
        self.adapter.set_powered(on).await.map_err(map_err)
    }

    async fn set_local_address(&self, addr: BdAddr) -> Result<()> {
        let index = self.hci_index().ok_or_else(|| {
            TransportError::LocalAddress(format!(
                "cannot derive an HCI index from adapter name {:?}",
                self.adapter.name()
            ))
        })?;
        // `pax_hci` power-cycles the controller and reprograms its public address
        // over the kernel mgmt socket. That is blocking socket I/O, so run it off
        // the async reactor.
        let octets = addr.octets();
        tokio::task::spawn_blocking(move || pax_hci::spoof_public_address(index, octets))
            .await
            .map_err(|e| TransportError::LocalAddress(format!("spoof task failed: {e}")))?
            .map_err(|e| TransportError::LocalAddress(e.to_string()))?;
        // The mgmt power-cycle happened underneath BlueZ; make sure bluetoothd sees
        // the adapter powered again so subsequent operations work.
        self.adapter.set_powered(true).await.map_err(map_err)?;
        Ok(())
    }

    async fn discover(&self, filter: &DiscoveryFilter) -> Result<Vec<DeviceInfo>> {
        use futures::StreamExt;

        self.emit(DeviceId::default(), TransitEventKind::DiscoveryStarted);

        let mut changes = self.adapter.discover_devices().await.map_err(map_err)?;
        let deadline = tokio::time::Instant::now() + self.discovery_window;

        let mut out: Vec<DeviceInfo> = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let next = tokio::time::timeout(remaining, changes.next()).await;
            let event = match next {
                Ok(Some(ev)) => ev,
                _ => break, // timed out or stream ended
            };
            if let bluer::AdapterEvent::DeviceAdded(addr) = event {
                let info = self.device_info(addr).await?;
                if let Some(t) = filter.transport {
                    // BlueZ does not cleanly separate transports per device here;
                    // we keep dual-capable devices and let the caller refine.
                    let _ = t;
                }
                if !filter.accepts(&info) {
                    continue;
                }
                self.emit(
                    info.id,
                    TransitEventKind::DeviceDiscovered { rssi: info.rssi },
                );
                out.push(info);
                if let Some(limit) = filter.limit {
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    async fn pair(&self, target: DeviceId, agent: Arc<dyn PairingAgent>) -> Result<PairingOutcome> {
        use pax_core::PairingMethodHint;

        // Bridge the toolkit's `PairingAgent` into a BlueZ D-Bus agent, registered
        // only for the duration of this call (the handle unregisters on drop).
        let (bluer_agent, method_seen, _bonded) = self.build_bluer_agent(agent);
        let _handle = self
            .session
            .register_agent(bluer_agent)
            .await
            .map_err(map_err)?;

        let dev = self
            .adapter
            .device(to_bluer_addr(target.addr))
            .map_err(map_err)?;
        let result = dev.pair().await;

        let method = method_seen
            .lock()
            .unwrap()
            .unwrap_or(PairingMethodHint::JustWorks);
        self.emit(target, TransitEventKind::PairingStarted { method });
        match result {
            Ok(()) => {
                self.emit(target, TransitEventKind::PairingCompleted);
                Ok(PairingOutcome::bonded(method))
            }
            Err(e) => {
                let reason = e.to_string();
                self.emit(
                    target,
                    TransitEventKind::PairingFailed {
                        reason: reason.clone(),
                    },
                );
                Err(TransportError::PairingFailed(reason))
            }
        }
    }

    async fn connect(&self, target: DeviceId) -> Result<Connection> {
        let dev = self
            .adapter
            .device(to_bluer_addr(target.addr))
            .map_err(map_err)?;
        dev.connect().await.map_err(map_err)?;
        self.emit(target, TransitEventKind::Connected);
        // BlueZ tracks the link itself; the token is unused for this backend.
        Ok(Connection::new(
            target,
            self.controller.clone(),
            self.spec,
            self.standards.resolve(target),
            0,
        ))
    }

    async fn disconnect(&self, conn: &Connection) -> Result<()> {
        let dev = self
            .adapter
            .device(to_bluer_addr(conn.peer.addr))
            .map_err(map_err)?;
        dev.disconnect().await.map_err(map_err)?;
        self.emit(conn.peer, TransitEventKind::Disconnected { reason: None });
        Ok(())
    }

    async fn push_file(
        &self,
        conn: &Connection,
        file: OutboundFile<'_>,
    ) -> Result<TransferReceipt> {
        // `bluer` has no OBEX, so we drive BlueZ's `obexd` directly (see `obex`).
        // obexd transfers from a path, so stage the in-memory bytes to a temp file.
        let total = file.len();
        let start = Timestamp::now();
        let staged = stage_temp_file(&file)
            .map_err(|e| TransportError::Backend(format!("staging temp file: {e}")))?;
        let path_str = staged.path.to_string_lossy().into_owned();
        let dest = conn.peer.addr.to_string();

        self.emit(
            conn.peer,
            TransitEventKind::TransferStarted {
                name: file.name.clone(),
                total_bytes: total,
            },
        );

        // Translate obexd's absolute-progress callbacks into per-delta `DataChunk`
        // events plus an absolute `TransferProgress`, counting the chunks seen.
        let peer = conn.peer;
        let mut last = 0u64;
        let mut chunks = 0u64;
        let result = obex::object_push(&dest, &path_str, |transferred| {
            let delta = transferred.saturating_sub(last);
            last = transferred;
            if delta > 0 {
                chunks += 1;
                self.emit(
                    peer,
                    TransitEventKind::DataChunk {
                        direction: Direction::Outbound,
                        bytes: delta,
                    },
                );
            }
            self.emit(
                peer,
                TransitEventKind::TransferProgress { transferred, total },
            );
        })
        .await;

        match result {
            Ok(bytes) => {
                let duration = Timestamp::now().saturating_since(start);
                self.emit(
                    peer,
                    TransitEventKind::TransferCompleted { bytes, duration },
                );
                Ok(TransferReceipt {
                    object_name: file.name,
                    bytes,
                    chunks: chunks.max(1),
                    duration,
                })
            }
            Err(e) => {
                self.emit(
                    peer,
                    TransitEventKind::Error {
                        detail: e.to_string(),
                    },
                );
                Err(e)
            }
        }
    }

    async fn set_discoverable(&self, on: bool, timeout: Option<Duration>) -> Result<()> {
        if let Some(t) = timeout {
            let secs = (t.as_millis() / 1000) as u32;
            self.adapter
                .set_discoverable_timeout(secs)
                .await
                .map_err(map_err)?;
        }
        self.adapter.set_discoverable(on).await.map_err(map_err)
    }

    async fn set_pairable(&self, on: bool, timeout: Option<Duration>) -> Result<()> {
        if let Some(t) = timeout {
            let secs = (t.as_millis() / 1000) as u32;
            self.adapter
                .set_pairable_timeout(secs)
                .await
                .map_err(map_err)?;
        }
        self.adapter.set_pairable(on).await.map_err(map_err)
    }

    async fn accept_pairings(
        &self,
        agent: Arc<dyn PairingAgent>,
        opts: InboundPairing,
    ) -> Result<Vec<DeviceId>> {
        // Register the bridged agent and become discoverable + pairable. The agent
        // callbacks record each device that authorizes a bond into `bonded`.
        let (bluer_agent, _method_seen, bonded) = self.build_bluer_agent(agent);
        let _handle = self
            .session
            .register_agent(bluer_agent)
            .await
            .map_err(map_err)?;

        // Set the BlueZ timeouts too, so the adapter auto-reverts even if we die.
        let secs = ((opts.window.as_millis() / 1000) as u32).max(1);
        let _ = self.adapter.set_pairable_timeout(secs).await;
        let _ = self.adapter.set_discoverable_timeout(secs).await;
        self.adapter.set_pairable(true).await.map_err(map_err)?;
        self.adapter.set_discoverable(true).await.map_err(map_err)?;

        // Hold the window open, stopping early once `max_devices` have bonded.
        let window = std::time::Duration::from_millis(opts.window.as_millis() as u64);
        let deadline = tokio::time::Instant::now() + window;
        loop {
            if let Some(max) = opts.max_devices {
                if bonded.lock().unwrap().len() >= max {
                    break;
                }
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            tokio::time::sleep(remaining.min(std::time::Duration::from_millis(250))).await;
        }

        // Revert visibility (best-effort) and drop the agent handle on return.
        let _ = self.adapter.set_discoverable(false).await;
        let _ = self.adapter.set_pairable(false).await;

        let ids: Vec<DeviceId> = bonded.lock().unwrap().iter().copied().collect();
        for id in &ids {
            self.emit(*id, TransitEventKind::PairingCompleted);
        }
        Ok(ids)
    }
}

/// A temp file that deletes itself on drop. Used to stage in-memory bytes for
/// obexd, which transfers from a path.
struct StagedFile {
    path: std::path::PathBuf,
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn stage_temp_file(file: &OutboundFile<'_>) -> std::io::Result<StagedFile> {
    use std::io::Write;
    let mut dir = std::env::temp_dir();
    // Keep the original name so the peer sees a sensible filename.
    let safe: String = file
        .name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.push(format!("pax-obex-{}-{}", std::process::id(), safe));
    let mut f = std::fs::File::create(&dir)?;
    f.write_all(file.bytes)?;
    f.flush()?;
    Ok(StagedFile { path: dir })
}

/// Convenience constructor mirroring the mock's ergonomics: a BlueZ backend on the
/// default adapter with no observer.
impl BlueZBackend {
    /// Open the default adapter with a no-op observer.
    pub async fn open() -> Result<Self> {
        Self::connect_default(Arc::new(NoopObserver) as SharedObserver).await
    }
}
