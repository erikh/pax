# pax — a Bluetooth toolkit

`pax` is a Rust workspace for doing real work with Bluetooth: **discover** devices,
**pair** with them, **upload files**, and **debug what happens on the wire while
data is in transit** — sliced by the controller hardware, the Bluetooth
specification, and the IEEE 802 standard involved.

It is built around one idea: *everything is written against a small async trait*,
so the exact code path you ship runs against either real hardware **or** a
deterministic in-memory mock. That makes the whole toolkit testable with **no
Bluetooth adapter, no D-Bus, and no peer device** — `cargo test` just works.

```text
   your application
        │
        ├── pax-pairing      ── pair with retries + ready-made agents
        ├── pax-transfer     ── upload files (OBEX) with progress + retries
        └── pax-diagnostics  ── record & analyze the in-transit event stream
                    │
              pax-transport  ── the BluetoothBackend trait + backends
              ┌─────┼───────────────┬───────────────────┐
         MockBackend         BlueZBackend         BtleplugBackend
        (always on)        (feature "bluez")     (feature "btleplug")
                    │
                pax-core     ── addresses, specs, hardware models, 802 taxonomy, events
```

---

## Why it's organized as separate crates

Each crate is one responsibility, so you depend on exactly what you use:

| Crate | What it gives you | Depends on |
|-------|-------------------|------------|
| [`pax-core`](crates/pax-core) | Pure value types: [`BdAddr`], [`DeviceId`], [`CoreVersion`], [`ControllerModel`], the [`Standard802`] / 802.1X taxonomy, and the [`TransitEvent`] model. No I/O, no async, no system deps. | — |
| [`pax-transport`](crates/pax-transport) | The `BluetoothBackend` async trait, the always-on deterministic `MockBackend`, and the optional real backends (BlueZ, btleplug, **Android**, **iOS**). | `pax-core` |
| [`pax-pairing`](crates/pax-pairing) | Pairing agents (`AcceptAllAgent`, `FixedPinAgent`, `CallbackAgent`, …) and a retrying `pair_device` workflow. | `pax-transport` |
| [`pax-transfer`](crates/pax-transfer) | High-level `upload_file` / `upload_files` with connection lifecycle, retries, and a clean progress callback. | `pax-transport` |
| [`pax-diagnostics`](crates/pax-diagnostics) | A `Recorder` (observer) plus an `Analyzer` that splits the event stream by **hardware model**, **Bluetooth spec**, and **IEEE 802 standard**. | `pax-core` |
| [`pax-hci`](crates/pax-hci) | Linux-only helper that reads the local controller's version/manufacturer from the kernel mgmt socket. The **only** crate that uses `unsafe`, isolated so everything else stays `#![forbid(unsafe_code)]`. | — |

---

## Install

This is a Cargo workspace. Add the crates you need from a path or git dependency:

```toml
[dependencies]
pax-core        = { git = "https://github.com/erikh/pax" }
pax-transport   = { git = "https://github.com/erikh/pax" }
pax-pairing     = { git = "https://github.com/erikh/pax" }
pax-transfer    = { git = "https://github.com/erikh/pax" }
pax-diagnostics = { git = "https://github.com/erikh/pax" }
```

The **default build needs no system libraries** and gives you the full API against
the mock backend. To talk to real hardware, turn on a backend feature — see
[Backends](#backends-real-hardware).

---

## Quick start (no hardware required)

This program runs as-is — it drives the mock backend through a realistic session
and prints a diagnostic report, all in memory.

```rust
use std::sync::Arc;

use pax_core::{ChipsetFamily, CompanyId, ControllerModel, DeviceId, SharedObserver};
use pax_diagnostics::{Analyzer, Recorder, SplitBy};
use pax_pairing::{agents::AcceptAllAgent, pair_device, PairOptions};
use pax_transfer::{upload_bytes, UploadOptions};
use pax_transport::mock::{MockBackend, MockDevice};
use pax_transport::{BluetoothBackend, DiscoveryFilter};

#[tokio::main]
async fn main() -> Result<(), pax_transport::TransportError> {
    // 1. Capture every in-transit event with a Recorder (it's just an Observer).
    let recorder = Arc::new(Recorder::new());

    // 2. Build a backend. Here it's the mock; swap in a real one (below) unchanged.
    let phone: DeviceId = "11:22:33:44:55:66".parse().unwrap();
    let backend = MockBackend::builder()
        .controller(ControllerModel::new(CompanyId::INTEL, ChipsetFamily::Intel, "AX210"))
        .observer(recorder.clone() as SharedObserver)
        .device(MockDevice::new(phone, "Test Phone").with_rssi(-55))
        .build();

    // 3. Discover, pair, upload — using the high-level workflows.
    let found = backend.discover(&DiscoveryFilter::new()).await?;
    println!("found {} device(s)", found.len());

    pair_device(&backend, phone, &AcceptAllAgent::new(), PairOptions::default()).await?;

    let receipt = upload_bytes(
        &backend, phone, "hello.txt", b"the quick brown fox", UploadOptions::default(),
    ).await?;
    println!("uploaded {} bytes in {:?}", receipt.bytes, receipt.duration);

    // 4. Analyze the recorded stream and print the report.
    let report = Analyzer::from_recorder(&recorder).report();
    println!("{report}");

    // ...or ask for one split dimension directly:
    let by_hw = Analyzer::from_recorder(&recorder).breakdown(SplitBy::Controller);
    println!("controllers seen: {}", by_hw.len());

    Ok(())
}
```

---

## The three things it does

### 1. Pair — `pax-pairing`

A [`PairingAgent`] answers the prompts a device raises (PIN, passkey, "just works"
confirmation). Use a ready-made one or supply a closure, then drive the retrying
workflow:

```rust
use pax_pairing::{agents::{AcceptAllAgent, FixedPinAgent, CallbackAgent}, pair_device, PairOptions};
use pax_transport::pairing::{PairingRequest, PairingResponse};

// Pick a policy:
let _ = AcceptAllAgent::new().with_pin("1234");      // confirms everything (test/trusted only)
let _ = FixedPinAgent::new("0000");                  // sticker-PIN accessories
let _ = CallbackAgent::new(|req| match req {          // bespoke logic
    PairingRequest::ConfirmPasskey { passkey } => PairingResponse::Confirm(passkey == 123_456),
    _ => PairingResponse::Cancel,
});

// pair_device retries transient failures but never re-asks after a real refusal.
# async fn demo(backend: &dyn pax_transport::BluetoothBackend, id: pax_core::DeviceId)
#   -> Result<(), pax_transport::TransportError> {
let report = pair_device(backend, id, &AcceptAllAgent::new(), PairOptions { retries: 3 }).await?;
println!("paired in {} attempt(s)", report.attempts);
# Ok(()) }
```

### 2. Upload files — `pax-transfer`

`upload_file` / `upload_files` manage the connection lifecycle, retry a transfer
that aborts mid-flight, and return a [`TransferReceipt`] (bytes, chunks, duration,
throughput). For a progress bar, attach a `ProgressObserver`:

```rust
use std::sync::{Arc, Mutex};
use pax_core::{SharedObserver, observe::FanOut};
use pax_transfer::progress::{ProgressObserver, ProgressUpdate};
use pax_diagnostics::Recorder;

// Fan the event stream out to BOTH a progress bar and a diagnostics recorder.
let recorder = Arc::new(Recorder::new());
let progress: SharedObserver = Arc::new(ProgressObserver::new(|u: ProgressUpdate| {
    println!("{}: {:.0}%", u.object, u.fraction() * 100.0);
}));
let observer: SharedObserver =
    Arc::new(FanOut::new(vec![recorder.clone() as SharedObserver, progress]));
// ...pass `observer` to MockBackend::builder().observer(observer) (or a real backend).
```

### 3. Debug in transit — `pax-diagnostics`

A [`Recorder`] captures the live [`TransitEvent`] stream; an [`Analyzer`] splits it.
The dimensions are exactly the ones you asked for:

```rust
use pax_diagnostics::{Analyzer, SplitBy};
# fn demo(analyzer: Analyzer) {
analyzer.breakdown(SplitBy::Controller);     // by hardware model  (Intel AX210 vs Apple combo vs …)
analyzer.breakdown(SplitBy::Spec);           // by Bluetooth spec  (5.2/BR-EDR vs 5.0/LE vs …)
analyzer.breakdown(SplitBy::RadioStandard);  // by IEEE 802.15.x radio lineage
analyzer.breakdown(SplitBy::PortAuth);       // by IEEE 802.1X port-auth state
analyzer.report();                           // all of the above + anomalies, as a text report
# }
```

Each bucket reports event count, payload bytes, failures, and throughput. The
report's `Display` renders a terminal-friendly table, and every type implements
`serde` (de)serialization under the `serde` feature for machine consumption.

#### About the IEEE 802 dimension

You asked to split "by specification of Bluetooth and 802.1x in general", so the
802 axis is modeled as a small family rather than a single standard (see
[`pax_core::standards`]):

* **802.15.x** — the WPAN radio lineage Bluetooth's lower layers derive from
  (802.15.1). This is the default `RadioStandard` for a Classic link.
* **802.1X** — port-based network access control (EAP). Meaningful when a Bluetooth
  PAN profile is bridged onto an 802 LAN; tracked as a `PortAuthState`
  (`n/a` → `authenticating` → `authenticated` / `auth-failed`).

A link therefore carries a [`StandardsProfile`] of *both* a radio standard and a
port-auth state, and you can split on either.

---

## Backends (real hardware)

Everything above runs on `MockBackend` by default. The real backends are
**opt-in** behind cargo features, because they need system libraries the mock does
not:

| Backend | Feature | Platform | Build requirement | Run requirement | OBEX file push |
|---------|---------|----------|-------------------|-----------------|----------------|
| `MockBackend` | *(always on)* | any | none | none | yes (simulated) |
| `BlueZBackend` | `bluez` | Linux | D-Bus dev headers (`dbus-devel` / `libdbus-1-dev`) | running `bluetoothd` + `obexd` | **yes** (via obexd, with per-packet progress) |
| `BtleplugBackend` | `btleplug` | Linux/macOS/Windows | platform BLE toolchain | platform BLE stack | no (BLE-only — `Unsupported`) |
| `AndroidBackend` | `android` | Android | NDK; embed in an app that hands it a `JavaVM` | `BLUETOOTH_CONNECT`/`SCAN` perms | **yes** (RFCOMM + OBEX, per-chunk progress) |
| `IosBackend` | `ios` | iOS | macOS + Xcode | — | no (Apple gives apps BLE only — `Unsupported`) |

```bash
# Linux: pair, push files (over obexd), full diagnostics:
sudo dnf install dbus-devel      # Fedora/Asahi   (Debian/Ubuntu: apt install libdbus-1-dev)
cargo build -p pax-transport --features bluez

# Cross-platform BLE scan/connect:
cargo build -p pax-transport --features btleplug

# Everything, plus the NetworkManager 802.1X resolver:
cargo build -p pax-transport --features "all-backends,port-auth-nm"
```

Swapping a real backend in is a one-line change — the trait is identical:

```rust,ignore
// was: let backend = MockBackend::builder()...build();
let backend = pax_transport::bluez::BlueZBackend::connect_default(observer).await?;
// pair_device / upload_file / Recorder all work exactly the same.
```

What the real backends do today:

* **`bluez`** — discover, **pair** (your `PairingAgent` is bridged into a real BlueZ
  D-Bus agent — PIN/passkey/confirmation prompts reach *your* policy), connect, and
  **push files** via `obexd` with **per-packet progress**. On open it
  **auto-detects** the controller's manufacturer/family (BlueZ modalias) and
  Bluetooth version (kernel mgmt socket via [`pax-hci`](crates/pax-hci)), so the
  diagnostic "split by hardware" key is real; override with `.with_controller()` /
  `.with_spec()`.
* **`btleplug`** — cross-platform BLE discover/connect/disconnect. Pairing and OBEX
  push return `Unsupported` (BLE bonding is OS-managed; OBEX is BR/EDR-only).

### Running on phones (Android & iOS)

The same `BluetoothBackend` trait drives the phones' own stacks:

* **`android`** — JNI to `android.bluetooth`. Bonded-device discovery, pairing
  (`createBond`), RFCOMM connect, and **OBEX Object Push** with per-chunk progress
  (a small, dependency-free OBEX client runs over the socket — and is unit-tested on
  any host, no phone required). Cross-compile for an Android target with the NDK and
  embed in an app that hands the backend a `JavaVM`:

  ```rust,ignore
  let vm = std::sync::Arc::new(/* JavaVM from JNI_OnLoad or env.get_java_vm()? */);
  let backend = pax_transport::android::AndroidBackend::new(vm, observer)?
      .with_context(context_global_ref);   // optional: enables live discovery
  // discover / pair / upload_file all work through the usual trait.
  ```

  **Live discovery.** `discover()` lists bonded devices by default. To also run a
  live classic inquiry (Android only delivers those via `ACTION_FOUND` broadcasts),
  bundle the companion class
  [`android-companion/dev/pax/PaxBluetooth.java`](crates/pax-transport/android-companion/dev/pax/PaxBluetooth.java)
  in your app and pass a `Context` via `.with_context(..)`. The Rust side polls the
  companion's buffer — no native callbacks, no `RegisterNatives`.

* **`ios`** — CoreBluetooth via `btleplug` (which speaks CoreBluetooth on Apple
  targets). Discover/connect/GATT only. `pair` and `push_file` return `Unsupported`
  because **Apple gives third-party apps no Classic Bluetooth, OBEX, or programmatic
  pairing** — a platform wall, not a missing feature. To send a file *to* an iPhone,
  use AirDrop or an app-level GATT protocol.

> **What's verified, and what isn't.** The mock-backed layers (core, pairing,
> transfer, diagnostics) and the pure-Rust pieces (peer detection, the OBEX client)
> are covered by the hardware-free suite. Every backend — desktop *and* mobile —
> **builds and lints cleanly in CI** (the `jni` and btleplug wrappers compile on any
> host), but their *runtime* needs a real adapter/phone, so end-to-end behavior is
> exercised by the `PAX_HW_TESTS`-gated tests in
> [`crates/pax-transport/tests/hardware.rs`](crates/pax-transport/tests/hardware.rs)
> and on-device, not by CI. Caveats: the BlueZ controller **version** probe needs
> `CAP_NET_ADMIN`, `obexd` must be running for BlueZ file push, the `port-auth-nm`
> resolver maps a coarse NetworkManager state, and the Android backend must run
> inside an app that supplies a `JavaVM`.

### Connecting *to* phones: telling Android from iPhone

When the phone is the **peer**, [`DeviceInfo::platform`](crates/pax-core) guesses
its platform from the Class-of-Device, the Apple company id in BLE manufacturer
data, and the name — handy because an iPhone will refuse a Classic file push while
an Android phone accepts one:

```rust,ignore
use pax_core::peer::PeerPlatform;
for d in backend.discover(&DiscoveryFilter::new()).await? {
    match d.platform() {
        PeerPlatform::IPhone  => println!("{} is an iPhone — no OBEX push", d.label()),
        PeerPlatform::Android => println!("{} is Android — OBEX push OK", d.label()),
        other                 => println!("{}: {other}", d.label()),
    }
}
```

### Wiring 802.1X / PAN auth into diagnostics

The IEEE 802 `PortAuth` split needs a source the library can't see on its own.
Supply one with a `StandardsResolver`:

```rust,ignore
use std::sync::Arc;
use pax_core::{PortAuthState, StandardsProfile};
use pax_transport::backend::StandardsFn;

// 1) Supply it yourself from whatever your app already knows:
let backend = backend.with_standards_resolver(Arc::new(StandardsFn::new(|peer| {
    StandardsProfile::bredr_default().with_port_auth(PortAuthState::Authenticated)
})));

// 2) ...or let the (best-effort) NetworkManager resolver track it live:
# #[cfg(feature = "port-auth-nm")]
let nm = pax_transport::bluez::port_auth::NetworkManagerStandards::connect().await?;
nm.clone().spawn_auto_refresh(std::time::Duration::from_secs(5));
let backend = backend.with_standards_resolver(nm);
```

---

## Testing

```bash
cargo test                                            # whole workspace, no hardware
cargo clippy --workspace --all-targets
cargo build  -p pax-transport --features all-backends # real backends compile + lint
cargo doc --workspace --no-deps

# On a real Linux host with an adapter (read-only smoke tests):
PAX_HW_TESTS=1 cargo test -p pax-transport --features bluez --test hardware -- --ignored --nocapture
```

The mock backend's virtual clock is deterministic, so diagnostic numbers
(throughput, span, chunk counts) are reproducible across machines and runs — which
is what makes them assertable in tests. See
[`crates/pax-diagnostics/tests/end_to_end.rs`](crates/pax-diagnostics/tests/end_to_end.rs)
for a full discover → pair → upload → analyze run with two controllers and two
specs.

### Manual smoke test (real device)

Pairing and file push are interactive, so verify them by hand against a phone:

```rust,ignore
let backend = pax_transport::bluez::BlueZBackend::open().await?;       // real adapter
let id = /* discover or hard-code your phone's address */;
pax_pairing::pair_device(&backend, id, std::sync::Arc::new(
    pax_pairing::agents::AcceptAllAgent::new()), Default::default()).await?;  // confirm on the phone
pax_transfer::upload_bytes(&backend, id, "hello.txt", b"hi from pax", Default::default()).await?;
```

---

## Design notes for consumers

* **One trait, swappable backends.** Hold a backend as
  `std::sync::Arc<dyn BluetoothBackend>` and your code is backend-agnostic.
* **The library never reads the wall clock.** Timestamps are supplied by the
  producer (deterministic in the mock, `Timestamp::now()` at the I/O boundary in
  real backends). This keeps analysis reproducible.
* **Observability is a sink, not a return value.** Backends emit `TransitEvent`s to
  an `Observer`; compose `FanOut` to log, show progress, and record at once.
* **`#![forbid(unsafe_code)]` everywhere except [`pax-hci`](crates/pax-hci)** — the
  one crate that needs a raw socket, deliberately isolated and ~150 lines so it is
  trivial to audit on its own.
* **Errors are typed and `#[non_exhaustive]`.** Match with a `_` arm.

## Roadmap

* GATT read/write/notify on the `btleplug` backend.
* A precise EAP-state source for `port-auth-nm` (wpa_supplicant) instead of the
  coarse NetworkManager device state.
* A `pax-cli` binary built on these crates (the libraries are designed to be
  dropped into a more prescriptive CLI later).

## License

Licensed under the GNU Affero General Public License v3.0 only
([AGPL-3.0-only](LICENSE)).
