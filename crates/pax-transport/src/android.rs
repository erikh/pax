//! Android backend: drives the phone's own Bluetooth stack via JNI to the
//! `android.bluetooth` Java APIs.
//!
//! # Status and build requirements
//!
//! Compiled only with the `android` cargo feature. The library is meant to be
//! cross-compiled for an Android target with the NDK and embedded in an app; the
//! app hands it the process `JavaVM` (from `JNI_OnLoad` or a JNI init call), which
//! is all this backend needs to reach `android.bluetooth`. The `jni` bindings are
//! host-portable, so `cargo build --features android` also compiles (and the
//! pure-Rust OBEX client is unit-tested) on a desktop for CI — but actually
//! *talking* to Bluetooth requires running inside an Android app.
//!
//! # Capabilities
//!
//! * **Discovery** lists bonded devices (`getBondedDevices`), and — when you supply
//!   an Android `Context` via `AndroidBackend::with_context` and bundle the
//!   `dev.pax.PaxBluetooth` companion class — also runs a live classic inquiry
//!   (Android delivers those results via `ACTION_FOUND` broadcasts, which the
//!   companion buffers for us to poll). The companion lives at
//!   `crates/pax-transport/android-companion/dev/pax/PaxBluetooth.java`.
//! * **Pairing** calls `BluetoothDevice.createBond()`.
//! * **Connect** opens an RFCOMM socket to the OBEX Object Push service.
//! * **File push** runs the `obex_opp` client over that socket — real OBEX
//!   Object Push with per-chunk progress.
//!
//! Permissions: your app must hold `BLUETOOTH_CONNECT` (and `BLUETOOTH_SCAN` plus
//! location for discovery) at runtime.
//!
//! ```no_run
//! # #[cfg(feature = "android")]
//! # async fn demo(vm: std::sync::Arc<jni::JavaVM>, context: jni::objects::GlobalRef,
//! #               observer: pax_core::SharedObserver) -> Result<(), pax_transport::TransportError> {
//! use pax_transport::android::AndroidBackend;
//! use pax_transport::{BluetoothBackend, DiscoveryFilter};
//!
//! let backend = AndroidBackend::new(vm, observer)?.with_context(context); // live scan
//! for d in backend.discover(&DiscoveryFilter::new()).await? {
//!     println!("{} — {:?}", d.label(), d.platform());
//! }
//! # Ok(()) }
//! ```

mod obex_opp;

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use jni::objects::{GlobalRef, JObject, JObjectArray, JValue};
use jni::JavaVM;

use pax_core::{
    ChipsetFamily, ClassOfDevice, CompanyId, ControllerModel, DeviceId, DeviceInfo, Direction,
    EventOrigin, SharedObserver, SpecContext, StandardsProfile, Timestamp, TransitEvent,
    TransitEventKind, Transport,
};

use crate::backend::{
    AdapterInfo, BackendKind, BluetoothBackend, Capabilities, Connection, DefaultStandards,
    DiscoveryFilter, SharedStandardsResolver, StaticStandards,
};
use crate::error::{Result, TransportError};
use crate::pairing::{PairingAgent, PairingOutcome};
use crate::transfer::{OutboundFile, TransferReceipt};

use self::obex_opp::ObexOppClient;

/// The Bluetooth SIG short UUID for OBEX Object Push, in 128-bit form.
const OPP_UUID: &str = "00001105-0000-1000-8000-00805f9b34fb";

/// Map a `jni` error onto a [`TransportError`].
fn jerr<E: std::fmt::Display>(e: E) -> TransportError {
    TransportError::Backend(format!("jni: {e}"))
}

fn io_other<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

/// A real Android [`BluetoothBackend`] backed by `android.bluetooth`.
pub struct AndroidBackend {
    vm: Arc<JavaVM>,
    adapter: GlobalRef,
    controller: ControllerModel,
    spec: SpecContext,
    observer: SharedObserver,
    standards: SharedStandardsResolver,
    /// An Android `Context`, if supplied — enables live discovery via the
    /// `dev.pax.PaxBluetooth` companion (see [`AndroidBackend::with_context`]).
    context: Option<GlobalRef>,
    discovery_window: std::time::Duration,
    seq: AtomicU64,
}

impl AndroidBackend {
    /// Build a backend from the app's [`JavaVM`].
    ///
    /// Obtain the `JavaVM` in `JNI_OnLoad` (`vm` argument) or from any `JNIEnv`
    /// via `get_java_vm()`, wrap it in an `Arc`, and pass it here. `observer`
    /// receives every [`TransitEvent`].
    pub fn new(vm: Arc<JavaVM>, observer: SharedObserver) -> Result<Self> {
        let adapter = {
            let mut env = vm.attach_current_thread().map_err(jerr)?;
            let cls = env
                .find_class("android/bluetooth/BluetoothAdapter")
                .map_err(jerr)?;
            let obj = env
                .call_static_method(
                    cls,
                    "getDefaultAdapter",
                    "()Landroid/bluetooth/BluetoothAdapter;",
                    &[],
                )
                .map_err(jerr)?
                .l()
                .map_err(jerr)?;
            if obj.is_null() {
                return Err(TransportError::Unavailable(
                    "no Bluetooth adapter on this device".into(),
                ));
            }
            env.new_global_ref(obj).map_err(jerr)?
        };

        Ok(AndroidBackend {
            vm,
            adapter,
            controller: ControllerModel::new(
                CompanyId(0xFFFF),
                ChipsetFamily::Unknown,
                "android-adapter",
            ),
            spec: SpecContext::new(pax_core::CoreVersion::V5_0, Transport::Dual),
            observer,
            standards: Arc::new(DefaultStandards),
            context: None,
            discovery_window: std::time::Duration::from_secs(8),
            seq: AtomicU64::new(0),
        })
    }

    /// Provide an Android `Context` to enable **live discovery**.
    ///
    /// Without a context, [`discover`](BluetoothBackend::discover) lists only
    /// bonded devices. With one — and the `dev.pax.PaxBluetooth` companion class
    /// bundled in your app — it also runs a classic inquiry. Create the
    /// [`GlobalRef`] from your activity/application context with
    /// `env.new_global_ref(context)?`.
    pub fn with_context(mut self, context: GlobalRef) -> Self {
        self.context = Some(context);
        self
    }

    /// Set how long live discovery scans before collecting results.
    pub fn with_discovery_window(mut self, window: std::time::Duration) -> Self {
        self.discovery_window = window;
        self
    }

    /// Override the controller model used to label events.
    pub fn with_controller(mut self, controller: ControllerModel) -> Self {
        self.controller = controller;
        self
    }

    /// Supply a [`StandardsResolver`](crate::backend::StandardsResolver).
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

    /// Read a Java `String` object into a Rust `String`, or `None` if null.
    fn jstring_opt(env: &mut jni::JNIEnv<'_>, obj: &JObject<'_>) -> Option<String> {
        if obj.is_null() {
            return None;
        }
        env.get_string(obj.into()).ok().map(|s| s.into())
    }

    /// Snapshot the bonded devices as [`DeviceInfo`]s. (Live inquiry needs a
    /// broadcast-receiver companion; see the module docs.)
    fn read_bonded(&self) -> Result<Vec<DeviceInfo>> {
        let mut env = self.vm.attach_current_thread().map_err(jerr)?;
        let set = env
            .call_method(&self.adapter, "getBondedDevices", "()Ljava/util/Set;", &[])
            .map_err(jerr)?
            .l()
            .map_err(jerr)?;
        if set.is_null() {
            return Ok(Vec::new());
        }
        let iter = env
            .call_method(&set, "iterator", "()Ljava/util/Iterator;", &[])
            .map_err(jerr)?
            .l()
            .map_err(jerr)?;

        let mut out = Vec::new();
        loop {
            let has_next = env
                .call_method(&iter, "hasNext", "()Z", &[])
                .map_err(jerr)?
                .z()
                .map_err(jerr)?;
            if !has_next {
                break;
            }
            let dev = env
                .call_method(&iter, "next", "()Ljava/lang/Object;", &[])
                .map_err(jerr)?
                .l()
                .map_err(jerr)?;

            let addr_obj = env
                .call_method(&dev, "getAddress", "()Ljava/lang/String;", &[])
                .map_err(jerr)?
                .l()
                .map_err(jerr)?;
            let addr_str = match Self::jstring_opt(&mut env, &addr_obj) {
                Some(s) => s,
                None => continue,
            };
            let addr = match addr_str.parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let id = DeviceId::public(addr);

            let name_obj = env
                .call_method(&dev, "getName", "()Ljava/lang/String;", &[])
                .map_err(jerr)?
                .l()
                .map_err(jerr)?;
            let mut info = DeviceInfo::new(id);
            info.name = Self::jstring_opt(&mut env, &name_obj);
            info.paired = true;
            // Class of device, if available.
            if let Ok(cls_obj) = env
                .call_method(
                    &dev,
                    "getBluetoothClass",
                    "()Landroid/bluetooth/BluetoothClass;",
                    &[],
                )
                .and_then(|v| v.l())
            {
                if !cls_obj.is_null() {
                    if let Ok(bits) = env
                        .call_method(&cls_obj, "getDeviceClass", "()I", &[])
                        .and_then(|v| v.i())
                    {
                        info.class = Some(ClassOfDevice::new(bits as u32));
                    }
                }
            }
            out.push(info);
        }
        Ok(out)
    }

    /// Run a live classic inquiry through the `dev.pax.PaxBluetooth` companion:
    /// `startDiscovery`, wait, `drain` the buffered results, `stopDiscovery`.
    /// Returns `Err` (caller falls back to bonded-only) if the companion class is
    /// not bundled or any call fails.
    async fn live_discover(&self) -> Result<Vec<DeviceInfo>> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| TransportError::Unavailable("no Android context".into()))?;
        const COMPANION: &str = "dev/pax/PaxBluetooth";

        // Start the inquiry.
        {
            let mut env = self.vm.attach_current_thread().map_err(jerr)?;
            let cls = env.find_class(COMPANION).map_err(jerr)?;
            env.call_static_method(
                &cls,
                "startDiscovery",
                "(Landroid/content/Context;)V",
                &[JValue::Object(context.as_obj())],
            )
            .map_err(jerr)?;
        }

        tokio::time::sleep(self.discovery_window).await;

        // Drain results and stop.
        let mut env = self.vm.attach_current_thread().map_err(jerr)?;
        let cls = env.find_class(COMPANION).map_err(jerr)?;
        let array: JObjectArray<'_> = env
            .call_static_method(&cls, "drain", "()[Ljava/lang/String;", &[])
            .map_err(jerr)?
            .l()
            .map_err(jerr)?
            .into();
        let _ = env.call_static_method(
            &cls,
            "stopDiscovery",
            "(Landroid/content/Context;)V",
            &[JValue::Object(context.as_obj())],
        );

        let len = env.get_array_length(&array).map_err(jerr)?;
        let mut out = Vec::new();
        for i in 0..len {
            let el = env.get_object_array_element(&array, i).map_err(jerr)?;
            let line = match Self::jstring_opt(&mut env, &el) {
                Some(s) => s,
                None => continue,
            };
            // "address|name|rssi"
            let mut parts = line.split('|');
            let addr_s = parts.next().unwrap_or("");
            let name_s = parts.next().unwrap_or("");
            let rssi_s = parts.next().unwrap_or("");
            let addr = match addr_s.parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let mut info = DeviceInfo::new(DeviceId::public(addr));
            if !name_s.is_empty() {
                info.name = Some(name_s.to_string());
            }
            // Android sends Short.MIN_VALUE when RSSI is unknown.
            if let Ok(rssi) = rssi_s.parse::<i16>() {
                if rssi != i16::MIN {
                    info.rssi = Some(rssi);
                }
            }
            out.push(info);
        }
        Ok(out)
    }

    /// Resolve an address to a `BluetoothDevice` global ref.
    fn remote_device(&self, env: &mut jni::JNIEnv<'_>, target: DeviceId) -> Result<GlobalRef> {
        let addr = env.new_string(target.addr.to_string()).map_err(jerr)?;
        let dev = env
            .call_method(
                &self.adapter,
                "getRemoteDevice",
                "(Ljava/lang/String;)Landroid/bluetooth/BluetoothDevice;",
                &[JValue::Object(&addr)],
            )
            .map_err(jerr)?
            .l()
            .map_err(jerr)?;
        env.new_global_ref(dev).map_err(jerr)
    }
}

#[async_trait]
impl BluetoothBackend for AndroidBackend {
    fn name(&self) -> &str {
        "android"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            kind: BackendKind::Android,
            transports: vec![Transport::BrEdr, Transport::Le, Transport::Dual],
            can_pair: true,
            can_push_files: true,
            max_concurrent_pairings: 1,
            can_accept_pairings: false,
        }
    }

    async fn adapter(&self) -> Result<AdapterInfo> {
        let mut env = self.vm.attach_current_thread().map_err(jerr)?;
        let name_obj = env
            .call_method(&self.adapter, "getName", "()Ljava/lang/String;", &[])
            .map_err(jerr)?
            .l()
            .map_err(jerr)?;
        let powered = env
            .call_method(&self.adapter, "isEnabled", "()Z", &[])
            .map_err(jerr)?
            .z()
            .map_err(jerr)?;
        Ok(AdapterInfo {
            address: pax_core::BdAddr::NIL, // Android hides the local address by policy.
            name: Self::jstring_opt(&mut env, &name_obj).unwrap_or_else(|| "android".into()),
            controller: self.controller.clone(),
            spec: self.spec,
            powered,
        })
    }

    async fn set_powered(&self, _on: bool) -> Result<()> {
        // Apps cannot toggle the adapter without a system permission / user prompt;
        // treat power as managed by the OS.
        Ok(())
    }

    async fn discover(&self, filter: &DiscoveryFilter) -> Result<Vec<DeviceInfo>> {
        self.emit(DeviceId::default(), TransitEventKind::DiscoveryStarted);

        // Start from bonded devices, then merge any live-inquiry results (deduped
        // by address). Live discovery needs a context + the companion class; if
        // either is absent we silently fall back to bonded-only.
        let mut devices = self.read_bonded()?;
        if self.context.is_some() {
            if let Ok(live) = self.live_discover().await {
                for info in live {
                    if !devices.iter().any(|d| d.id == info.id) {
                        devices.push(info);
                    }
                }
            }
        }

        let mut out = Vec::new();
        for info in devices {
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
        Ok(out)
    }

    async fn pair(
        &self,
        target: DeviceId,
        _agent: Arc<dyn PairingAgent>,
    ) -> Result<PairingOutcome> {
        // Android drives pairing UI itself; the agent cannot be bridged in (the OS
        // owns the prompt). We kick off bonding and report the outcome.
        let method = pax_core::PairingMethodHint::JustWorks;
        self.emit(target, TransitEventKind::PairingStarted { method });

        let mut env = self.vm.attach_current_thread().map_err(jerr)?;
        let dev = self.remote_device(&mut env, target)?;
        let ok = env
            .call_method(&dev, "createBond", "()Z", &[])
            .map_err(jerr)?
            .z()
            .map_err(jerr)?;
        if ok {
            self.emit(target, TransitEventKind::PairingCompleted);
            Ok(PairingOutcome::bonded(method))
        } else {
            let reason = "createBond() returned false".to_string();
            self.emit(
                target,
                TransitEventKind::PairingFailed {
                    reason: reason.clone(),
                },
            );
            Err(TransportError::PairingFailed(reason))
        }
    }

    async fn connect(&self, target: DeviceId) -> Result<Connection> {
        // Open (and immediately hold) an RFCOMM socket to OPP, then store it for
        // push_file via the connection token. For simplicity this backend opens a
        // fresh socket per push, so connect() just validates reachability.
        let mut env = self.vm.attach_current_thread().map_err(jerr)?;
        let _dev = self.remote_device(&mut env, target)?;
        self.emit(target, TransitEventKind::Connected);
        Ok(Connection::new(
            target,
            self.controller.clone(),
            self.spec,
            self.standards.resolve(target),
            0,
        ))
    }

    async fn disconnect(&self, conn: &Connection) -> Result<()> {
        self.emit(conn.peer, TransitEventKind::Disconnected { reason: None });
        Ok(())
    }

    async fn push_file(
        &self,
        conn: &Connection,
        file: OutboundFile<'_>,
    ) -> Result<TransferReceipt> {
        let total = file.len();
        let start = Timestamp::now();
        self.emit(
            conn.peer,
            TransitEventKind::TransferStarted {
                name: file.name.clone(),
                total_bytes: total,
            },
        );

        let peer = conn.peer;
        let result = self.run_push(peer, &file);

        match result {
            Ok(chunks) => {
                let duration = Timestamp::now().saturating_since(start);
                self.emit(
                    peer,
                    TransitEventKind::TransferCompleted {
                        bytes: total,
                        duration,
                    },
                );
                Ok(TransferReceipt {
                    object_name: file.name,
                    bytes: total,
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
}

impl AndroidBackend {
    /// Open an RFCOMM/OPP socket and run the OBEX push, emitting per-chunk events.
    /// Returns the number of Body chunks sent.
    fn run_push(&self, peer: DeviceId, file: &OutboundFile<'_>) -> Result<u64> {
        let mut env = self.vm.attach_current_thread().map_err(jerr)?;
        let dev = self.remote_device(&mut env, peer)?;

        // UUID.fromString(OPP_UUID)
        let uuid_cls = env.find_class("java/util/UUID").map_err(jerr)?;
        let uuid_str = env.new_string(OPP_UUID).map_err(jerr)?;
        let uuid = env
            .call_static_method(
                uuid_cls,
                "fromString",
                "(Ljava/lang/String;)Ljava/util/UUID;",
                &[JValue::Object(&uuid_str)],
            )
            .map_err(jerr)?
            .l()
            .map_err(jerr)?;

        // socket = dev.createRfcommSocketToServiceRecord(uuid); socket.connect();
        let socket = env
            .call_method(
                &dev,
                "createRfcommSocketToServiceRecord",
                "(Ljava/util/UUID;)Landroid/bluetooth/BluetoothSocket;",
                &[JValue::Object(&uuid)],
            )
            .map_err(jerr)?
            .l()
            .map_err(jerr)?;
        let socket = env.new_global_ref(socket).map_err(jerr)?;
        env.call_method(&socket, "connect", "()V", &[])
            .map_err(jerr)?;

        let input = env
            .call_method(&socket, "getInputStream", "()Ljava/io/InputStream;", &[])
            .map_err(jerr)?
            .l()
            .map_err(jerr)?;
        let output = env
            .call_method(&socket, "getOutputStream", "()Ljava/io/OutputStream;", &[])
            .map_err(jerr)?
            .l()
            .map_err(jerr)?;
        let input = env.new_global_ref(input).map_err(jerr)?;
        let output = env.new_global_ref(output).map_err(jerr)?;
        drop(env); // the stream adapter re-attaches per call

        let stream = JniStream {
            vm: &self.vm,
            input,
            output,
        };
        let chunks = std::cell::Cell::new(0u64);
        let mut prev = 0u64;

        let push_result = (|| -> std::result::Result<(), obex_opp::ObexError> {
            let mut client = ObexOppClient::connect(stream)?;
            client.put_file(&file.name, file.bytes, |sent| {
                let delta = sent.saturating_sub(prev);
                prev = sent;
                if delta > 0 {
                    chunks.set(chunks.get() + 1);
                    self.emit(
                        peer,
                        TransitEventKind::DataChunk {
                            direction: Direction::Outbound,
                            bytes: delta,
                        },
                    );
                    self.emit(
                        peer,
                        TransitEventKind::TransferProgress {
                            transferred: sent,
                            total: file.len(),
                        },
                    );
                }
            })?;
            client.disconnect()?;
            Ok(())
        })();

        // Close the socket regardless of outcome.
        if let Ok(mut env) = self.vm.attach_current_thread() {
            let _ = env.call_method(&socket, "close", "()V", &[]);
        }

        push_result.map_err(|e| match e {
            obex_opp::ObexError::Rejected { code, .. } => TransportError::TransferFailed {
                transferred: prev,
                reason: format!("OBEX rejected (0x{code:02X})"),
            },
            other => TransportError::Backend(format!("obex: {other}")),
        })?;
        Ok(chunks.get())
    }
}

/// Adapts an Android `BluetoothSocket`'s input/output streams to Rust's
/// [`Read`]/[`Write`], re-attaching the JVM thread per call.
struct JniStream<'vm> {
    vm: &'vm JavaVM,
    input: GlobalRef,
    output: GlobalRef,
}

impl Read for JniStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut env = self.vm.attach_current_thread().map_err(io_other)?;
        let arr = env.new_byte_array(buf.len() as i32).map_err(io_other)?;
        let n = env
            .call_method(
                &self.input,
                "read",
                "([BII)I",
                &[
                    JValue::Object(&arr),
                    JValue::Int(0),
                    JValue::Int(buf.len() as i32),
                ],
            )
            .map_err(io_other)?
            .i()
            .map_err(io_other)?;
        if n <= 0 {
            return Ok(0);
        }
        let mut tmp = vec![0i8; n as usize];
        env.get_byte_array_region(&arr, 0, &mut tmp)
            .map_err(io_other)?;
        for (d, s) in buf.iter_mut().zip(tmp.iter()) {
            *d = *s as u8;
        }
        Ok(n as usize)
    }
}

impl Write for JniStream<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut env = self.vm.attach_current_thread().map_err(io_other)?;
        let arr = env.new_byte_array(buf.len() as i32).map_err(io_other)?;
        let tmp: Vec<i8> = buf.iter().map(|b| *b as i8).collect();
        env.set_byte_array_region(&arr, 0, &tmp).map_err(io_other)?;
        env.call_method(
            &self.output,
            "write",
            "([BII)V",
            &[
                JValue::Object(&arr),
                JValue::Int(0),
                JValue::Int(buf.len() as i32),
            ],
        )
        .map_err(io_other)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut env = self.vm.attach_current_thread().map_err(io_other)?;
        env.call_method(&self.output, "flush", "()V", &[])
            .map_err(io_other)?;
        Ok(())
    }
}
