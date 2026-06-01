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
| [`pax-cli`](crates/pax-cli) | The `pax` command-line tool: `scan`, `pair`, `pair-many`, `accept`, `send`, `gatt`, `doctor`. Mock by default; `--features bluez`/`btleplug` for real hardware. | all libs |

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

    pair_device(&backend, phone, std::sync::Arc::new(AcceptAllAgent::new()), PairOptions::default()).await?;

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
let agent = std::sync::Arc::new(AcceptAllAgent::new());
let report = pair_device(backend, id, agent, PairOptions { retries: 3 }).await?;
println!("paired in {} attempt(s)", report.attempts);
# Ok(()) }
```

#### Pairing with many devices

Two one-call helpers fan pairing out — and pick their concurrency **transparently**
from the backend (concurrent on the mock, sequential on a single real controller,
no caller branching):

```rust,ignore
use std::sync::Arc;
use pax_pairing::{agents::AcceptAllAgent, pair_devices, BatchPairOptions};
use pax_transport::InboundPairing;

// Outbound, round-robin: pair a known list. One failure never aborts the rest;
// you get a PairItem per device, in order.
let results = pair_devices(backend, &targets, Arc::new(AcceptAllAgent::new()),
                           BatchPairOptions::default()).await;
for r in &results { println!("{} -> {}", r.id, if r.paired() {"ok"} else {"failed"}); }

// Inbound "broadcast": make THIS adapter discoverable + pairable and accept bonds
// from many devices at once (a hub/kiosk accepting phones). BlueZ + mock only.
let bonded = backend.accept_pairings(Arc::new(AcceptAllAgent::new()),
                                     InboundPairing::for_window(std::time::Duration::from_secs(30).into())).await?;
```

`pair_discovered(backend, filter, agent, opts)` combines discovery with outbound
batch pairing. To exercise either path against real devices, see
[Live / hardware testing](#live--hardware-testing).

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

### Inventory: dump every device in range

One async call scans and returns a detailed, human-readable dump of every nearby
device — address, inferred platform (Android/iPhone/…), signal, Class-of-Device,
bond state, vendor data (by company name), and service UUIDs:

```rust,ignore
let report = pax_transport::dump_in_range(&backend, &DiscoveryFilter::new()).await?;
println!("{report}");
// === 7 device(s) in range ===
//
// Device 11:22:33:44:55:66
//   name:        Erik's iPhone
//   platform:    iPhone/iOS
//   rssi:        -55 dBm
//   class:       0x7A020C (phone)
//   vendor data: Apple, Inc. (0x004C): 10 05
//   service:     0000180a-0000-1000-8000-00805f9b34fb
```

Per-device, [`DeviceInfo::dump`] gives the same; for structured output, the
`DeviceInfo`s are `serde`-serializable (with the `pax-core/serde` feature).

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
> are covered by the hardware-free suite. Every backend builds and lints in CI; the
> Android backend additionally **cross-compiles to the real `aarch64-` and
> `x86_64-linux-android` ABIs** (it is a pure-Rust library, so no NDK is needed) and
> the **Java companion compiles `-Werror` against the Android 34 API**. What still
> needs real hardware is *runtime* behavior — exercised by the `PAX_HW_TESTS`-gated
> tests in [`crates/pax-transport/tests/hardware.rs`](crates/pax-transport/tests/hardware.rs)
> and on-device, not by CI (run them yourself — see
> [Live / hardware testing](#live--hardware-testing)). Caveats: the BlueZ
> controller **version** probe needs
> `CAP_NET_ADMIN`, `obexd` must be running for BlueZ file push, the `port-auth-nm`
> resolver maps a coarse NetworkManager state, and the Android backend must run
> inside an app that supplies a `JavaVM`.

### Building for Android

```bash
rustup target add aarch64-linux-android x86_64-linux-android
# A *library* build needs no NDK (pure Rust, no link step):
cargo build -p pax-transport --target aarch64-linux-android --features android
# Compile the companion against the Android API (any JDK + an android.jar):
javac -cp "$ANDROID_HOME/platforms/android-34/android.jar" \
  crates/pax-transport/android-companion/dev/pax/PaxBluetooth.java
```

Producing the final app `.so` (a `cdylib`) *does* need the NDK linker. Note the
NDK ships **x86_64-host binaries only**, so on an ARM-Linux host (e.g. Asahi) you
build the `.so` from an x86_64 machine / CI, or run the NDK under emulation — the
library cross-compile above still works natively on ARM.

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

### Hardware-free (CI / `cargo test`)

The whole toolkit is testable with **no Bluetooth adapter, no D-Bus, and no peer
device** — the mock backend runs the exact code paths the real backends do:

```bash
cargo test                                            # whole workspace, no hardware
cargo clippy --workspace --all-targets
cargo build  -p pax-transport --features all-backends # real backends compile + lint
cargo doc --workspace --no-deps
```

The mock backend's virtual clock is deterministic, so diagnostic numbers
(throughput, span, chunk counts) are reproducible across machines and runs — which
is what makes them assertable in tests. See
[`crates/pax-diagnostics/tests/end_to_end.rs`](crates/pax-diagnostics/tests/end_to_end.rs)
for a full discover → pair → upload → analyze run with two controllers and two
specs.

### Live / hardware testing

What the hardware-free suite **cannot** cover is *runtime* behavior against a real
controller and a real peer (see *[What's verified, and what isn't](#running-on-phones-android--ios)*
above). There are two ways to drive live hardware, both behind the same backend
cargo features (`--features bluez` / `btleplug`):

1. **The gated test binary** — `PAX_HW_TESTS`-guarded smoke tests in
   [`crates/pax-transport/tests/hardware.rs`](crates/pax-transport/tests/hardware.rs).
2. **The `pax` CLI** — drives a real adapter interactively (see
   [Command line: `pax`](#command-line-pax)).

#### Prerequisites

| Backend | Build needs | Run needs |
|---------|-------------|-----------|
| `bluez` | dbus dev headers (`dbus-devel` / `libdbus-1-dev`) | `bluetoothd` running; `obexd` for file push; **`sudo`** (`CAP_NET_ADMIN`) for the controller *version* probe |
| `btleplug` | platform BLE toolchain | the platform BLE stack + scan/connect permissions |

#### The gated test suite

These tests are `#[ignore]`d **and** guarded by the `PAX_HW_TESTS` env var, so they
compile in CI but only *run* when you opt in with both `PAX_HW_TESTS=1` and
`-- --ignored`. Add `--nocapture` to see the `println!` output (device lists,
adapter info, bonded ids):

```bash
# bluez, read-only (open the adapter, scan, dump devices in range) — safe on a daily driver:
PAX_HW_TESTS=1 cargo test -p pax-transport --features bluez --test hardware -- --ignored --nocapture

# btleplug, read-only (BLE scan):
PAX_HW_TESTS=1 cargo test -p pax-transport --features btleplug --test hardware -- --ignored --nocapture
```

Every gated test, what it exercises, and the env vars that drive it:

| Test | Feature | Kind | Extra env vars |
|------|---------|------|----------------|
| `adapter_reports_real_controller` | bluez | read-only | — |
| `discovery_runs` | bluez | read-only | — |
| `dump_devices_in_range` | bluez | read-only | — |
| `inbound_pairing_mode` | bluez | **interactive** — makes the adapter discoverable + pairable; pair a phone *to* it | `PAX_HW_PAIR_WINDOW` (seconds, default 20) |
| `outbound_batch_pairing` | bluez | **interactive** — pairs the listed devices | `PAX_HW_PAIR_TARGETS=AA:BB:..,CC:DD:..` (skipped if unset) |
| `ble_discovery_runs` | btleplug | read-only | — |
| `gatt_read_env` | btleplug | **interactive** — connect + read one GATT characteristic | `PAX_HW_GATT_ADDR`, `PAX_HW_GATT_SVC`, `PAX_HW_GATT_CHR` (skipped if unset) |

Run a single interactive test by naming it and supplying its env vars:

```bash
# Inbound "pairing mode": be pairable for 30s, then pair a phone to this laptop.
PAX_HW_TESTS=1 PAX_HW_PAIR_WINDOW=30 \
  cargo test -p pax-transport --features bluez --test hardware \
  inbound_pairing_mode -- --ignored --nocapture

# Outbound batch: pair a known list of devices, round-robin.
PAX_HW_TESTS=1 PAX_HW_PAIR_TARGETS=AA:BB:CC:DD:EE:FF,11:22:33:44:55:66 \
  cargo test -p pax-transport --features bluez --test hardware \
  outbound_batch_pairing -- --ignored --nocapture

# GATT read: connect to a BLE device and read service 180a / characteristic 2a29.
PAX_HW_TESTS=1 PAX_HW_GATT_ADDR=AA:BB:CC:DD:EE:FF PAX_HW_GATT_SVC=180a PAX_HW_GATT_CHR=2a29 \
  cargo test -p pax-transport --features btleplug --test hardware \
  gatt_read_env -- --ignored --nocapture
```

UUIDs accept the short (`180a`) or full 128-bit form.

#### Env-var reference

Single source of truth for the hardware-test env vars:

| Variable | Used by | Meaning |
|----------|---------|---------|
| `PAX_HW_TESTS` | all gated tests | master gate — any value lets the gated tests run (still needs `-- --ignored`) |
| `PAX_HW_PAIR_WINDOW` | `inbound_pairing_mode` | seconds to stay pairable (default 20) |
| `PAX_HW_PAIR_TARGETS` | `outbound_batch_pairing` | comma-separated device addresses to pair |
| `PAX_HW_GATT_ADDR` | `gatt_read_env` | BLE device address to connect to |
| `PAX_HW_GATT_SVC` | `gatt_read_env` | GATT service UUID (short or full) |
| `PAX_HW_GATT_CHR` | `gatt_read_env` | GATT characteristic UUID (short or full) |

#### Driving live hardware with the `pax` CLI

The CLI runs every workflow against a real adapter when built with a backend
feature; `--backend` picks it and `--verbose` streams the in-transit event log to
stderr:

```bash
cargo run -p pax-cli --features bluez -- --backend bluez --verbose scan
cargo run -p pax-cli --features bluez -- --backend bluez pair AA:BB:CC:DD:EE:FF
cargo run -p pax-cli --features bluez -- --backend bluez accept --window 30
cargo run -p pax-cli --features bluez -- --backend bluez send AA:.. ./file.bin
cargo run -p pax-cli --features btleplug -- --backend btleplug gatt read AA:.. 180a 2a29
```

See [Command line: `pax`](#command-line-pax) for the full subcommand list.

#### Manual smoke test (real device, from Rust)

Pairing and file push are interactive, so you can also verify them by hand against
a phone from your own binary:

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
  one crate that needs raw `AF_BLUETOOTH` sockets (to read controller info and to
  reprogram the local address), deliberately isolated and small so it is trivial to
  audit on its own.
* **Errors are typed and `#[non_exhaustive]`.** Match with a `_` arm.

## Command line: `pax`

The [`pax-cli`](crates/pax-cli) crate is the `pax` binary. It builds against the
**mock by default** (no hardware, no system deps); add `--features bluez` /
`btleplug` to drive a real adapter.

```bash
cargo run -p pax-cli -- scan                 # dump every device in range
cargo run -p pax-cli -- pair AA:BB:CC:DD:EE:FF
cargo run -p pax-cli -- pair-many A B C       # round-robin, concurrent where possible
cargo run -p pax-cli -- accept --window 30    # inbound "pairing mode"
cargo run -p pax-cli -- send AA:.. ./file.bin # OBEX Object Push
cargo run -p pax-cli -- gatt read AA:.. 180a 2a29
cargo run -p pax-cli -- doctor                # run a session, print a diagnostics report
cargo run -p pax-cli -- spoof 02:00:00:11:22:33      # impersonate a local address
cargo run -p pax-cli -- --spoof 02:00:00:11:22:33 scan  # …or run any command under one
# real hardware + the event stream on stderr:
cargo run -p pax-cli --features bluez -- --backend bluez --verbose scan
```

## GATT (BLE) read / write / notify

The `btleplug` (and iOS) backend exposes GATT on the trait:

```rust,ignore
use pax_core::Uuid;
let conn = backend.connect(id).await?;
let value = backend.gatt_read(&conn, Uuid::from_u16(0x180A), Uuid::from_u16(0x2A29)).await?;
backend.gatt_write(&conn, svc, chr, b"\x01", true).await?;          // with response
let mut notifications = backend.gatt_subscribe(&conn, svc, chr).await?;
while let Some((uuid, bytes)) = futures::StreamExt::next(&mut notifications).await { /* … */ }
```

UUIDs accept the canonical 128-bit form or 16-/32-bit short forms (`Uuid::from_u16`,
or `"180a".parse()`). bluez/android leave GATT `Unsupported` for now.

## Impersonating a local address

The toolkit can present an arbitrary **local** `BD_ADDR` — useful for privacy,
device migration, testing a peer's bonding/allow-list logic, and authorized
security research. It changes *your own* controller's identity; it never touches a
remote device.

It is a **backend capability**, so an impossible request fails fast instead of
silently doing nothing. Query `capabilities().can_spoof_address`, or just use the
capability-checked helpers:

| Backend | Spoofing | How |
|---------|----------|-----|
| `bluez` | ✅ (Linux, needs root / `CAP_NET_ADMIN`, driver-dependent) | kernel mgmt socket via [`pax-hci`](crates/pax-hci) |
| `mock`  | ✅ (simulated) | records the address; great for tests |
| `btleplug` / `android` / `ios` | ❌ (no OS mechanism) | returns `Unsupported` |

```rust,ignore
use pax_transport::{apply_spoof, discover_as, connect_as, BluetoothBackend};
use pax_core::BdAddr;

let alias: BdAddr = "02:00:00:11:22:33".parse()?;

// Set it once (adapter-global), then everything afterward runs under `alias`:
backend.set_local_address(alias).await?;            // raw trait method
// …or apply it inline to a specific operation (capability-checked):
let devices = discover_as(&*backend, Some(alias), &DiscoveryFilter::new()).await?;
let conn    = connect_as(&*backend, Some(alias), target).await?;   // then GATT, etc.
```

Pairing takes it as an option, applied once before the (possibly retried) bond:

```rust,ignore
use pax_pairing::{pair_device, PairOptions};
let report = pair_device(&*backend, target, agent,
                         PairOptions::default().spoofing(alias)).await?;
```

On the command line, `--spoof <ADDR>` runs any command under the address, and the
`spoof` subcommand sets it and exits. Selecting a backend that can't do it errors
up front:

```bash
pax --backend bluez --spoof 02:00:00:11:22:33 pair AA:BB:CC:DD:EE:FF   # pair as a different address
pax --backend bluez spoof 02:00:00:11:22:33                            # set + persist on the adapter
pax --backend btleplug --spoof 02:00:00:11:22:33 scan
#   error: the `btleplug` backend cannot spoof a local address — use --backend bluez (Linux) or --backend mock
```

The change does **not** survive a controller reset/reboot, and `bluetoothd` may
re-assert the address when it next manages the adapter — re-apply as needed.

## Roadmap

The three previously-listed roadmap items — GATT, a precise wpa_supplicant EAP
source ([`port-auth-wpa`](crates/pax-transport)), and the `pax-cli` binary — are
**done**. What remains is on-device validation that needs a phone / macOS / emulator
(see *What's verified, and what isn't* above), plus GATT on the BlueZ and Android
backends.

## License

Licensed under the GNU Affero General Public License v3.0 only
([AGPL-3.0-only](LICENSE)).
