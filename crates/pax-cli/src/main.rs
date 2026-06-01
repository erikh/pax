//! `pax` — a command-line front-end for the pax Bluetooth toolkit.
//!
//! Every subcommand drives the same `BluetoothBackend` the libraries do, so the
//! CLI works against the deterministic **mock** by default (no hardware, no system
//! deps) and against a real adapter when built with `--features bluez` / `btleplug`.
//!
//! ```text
//! pax scan                       # dump every device in range
//! pax pair AA:BB:CC:DD:EE:FF      # pair with one device
//! pax pair-many A B C             # round-robin pair several
//! pax accept --window 30         # inbound "pairing mode" (be pairable)
//! pax send AA:.. ./file.bin      # OBEX Object Push
//! pax gatt read AA:.. 180a 2a29  # read a GATT characteristic
//! pax doctor                     # run a session, print a diagnostics report
//! pax --backend bluez scan       # pick a backend; --verbose streams events
//! pax spoof 02:00:00:11:22:33    # impersonate a local address (mock/bluez)
//! pax --spoof 02:00:00:11:22:33 scan   # scan under an impersonated address
//! ```
//!
//! ## Impersonating a local address
//!
//! The global `--spoof <ADDR>` flag sets the local adapter's `BD_ADDR` before the
//! command runs, so the scan/pair/GATT that follows presents that identity. It is
//! gated on [`Capabilities::can_spoof_address`](pax_transport::Capabilities): the
//! `bluez` backend (needs root/`CAP_NET_ADMIN`) and the `mock` backend support it;
//! selecting `--backend btleplug --spoof …` fails up front with a clear message.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use clap::{Parser, Subcommand, ValueEnum};
use futures::StreamExt as _;

use pax_core::observe::FanOut;
use pax_core::{
    BdAddr, ChipsetFamily, ClassOfDevice, CompanyId, ControllerModel, DeviceId, Duration, Observer,
    SharedObserver, TransitEvent, Uuid,
};
use pax_diagnostics::{Analyzer, Recorder};
use pax_pairing::agents::AcceptAllAgent;
use pax_pairing::{pair_device, pair_devices, pair_discovered};
use pax_transfer::{upload_file, UploadOptions};
use pax_transport::mock::{MockBackend, MockDevice};
use pax_transport::transfer::OutboundFile;
use pax_transport::{dump_in_range, BluetoothBackend, DiscoveryFilter, InboundPairing};

/// A Bluetooth toolkit on the command line.
#[derive(Parser)]
#[command(name = "pax", version, about)]
struct Cli {
    /// Which backend to drive. Defaults to a real backend if one was compiled in,
    /// else the in-memory demo mock.
    #[arg(long, value_enum, global = true)]
    backend: Option<Backend>,
    /// Stream the in-transit event log to stderr.
    #[arg(long, global = true)]
    verbose: bool,
    /// Impersonate this local Bluetooth address before running the command, so
    /// scans, pairings, and GATT present it as this device's identity. Needs a
    /// backend that supports it (mock or bluez); other backends error up front.
    #[arg(long, value_name = "ADDR", global = true)]
    spoof: Option<String>,
    /// The subcommand to run.
    #[command(subcommand)]
    command: Command,
}

/// Which Bluetooth backend the CLI drives.
#[derive(Copy, Clone, ValueEnum)]
enum Backend {
    /// The deterministic in-memory mock (scripted demo devices).
    Mock,
    /// Linux BlueZ (requires `--features bluez`).
    Bluez,
    /// Cross-platform BLE via btleplug (requires `--features btleplug`).
    Btleplug,
}

/// The top-level `pax` subcommands.
#[derive(Subcommand)]
enum Command {
    /// Scan and dump detailed info for every device in range.
    #[command(alias = "dump")]
    Scan,
    /// Pair with one device.
    Pair {
        /// Address of the device to pair with.
        addr: String,
    },
    /// Pair with several devices (round-robin, concurrent where the backend allows).
    PairMany {
        /// Addresses of the devices to pair with.
        addrs: Vec<String>,
    },
    /// Discover and pair with every device in range.
    PairAll,
    /// Inbound "pairing mode": become discoverable + pairable and accept bonds.
    Accept {
        /// How many seconds to stay pairable.
        #[arg(long, default_value_t = 30)]
        window: u64,
    },
    /// Send a file to a device via OBEX Object Push.
    Send {
        /// Address of the device to send the file to.
        addr: String,
        /// Path of the file to send.
        file: PathBuf,
    },
    /// GATT (BLE) operations.
    Gatt {
        /// Which GATT operation to perform.
        #[command(subcommand)]
        op: GattOp,
    },
    /// Set (spoof) the local adapter's Bluetooth address and exit.
    ///
    /// On a real adapter (bluez) the controller keeps the address until it is
    /// reset; use `--spoof` on another command to run that command under it.
    Spoof {
        /// The address to present, e.g. `02:00:00:11:22:33`.
        addr: String,
    },
    /// Run a short session and print a diagnostics report.
    Doctor,
}

/// GATT (BLE) operations for the `gatt` subcommand.
#[derive(Subcommand)]
enum GattOp {
    /// Read a characteristic value.
    Read {
        /// Address of the device to connect to.
        addr: String,
        /// Service UUID (short or full form).
        service: String,
        /// Characteristic UUID (short or full form).
        characteristic: String,
    },
    /// Write hex bytes to a characteristic.
    Write {
        /// Address of the device to connect to.
        addr: String,
        /// Service UUID (short or full form).
        service: String,
        /// Characteristic UUID (short or full form).
        characteristic: String,
        /// Hex payload, e.g. `01ff` or `01:ff`.
        hex: String,
    },
    /// Subscribe to notifications and print up to `count` of them.
    Subscribe {
        /// Address of the device to connect to.
        addr: String,
        /// Service UUID (short or full form).
        service: String,
        /// Characteristic UUID (short or full form).
        characteristic: String,
        /// Stop after printing this many notifications.
        #[arg(long, default_value_t = 5)]
        count: usize,
    },
}

/// Parse arguments, open the chosen backend, and dispatch the subcommand.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // A recorder captures the event stream; --verbose also tees it to stderr.
    let recorder = Arc::new(Recorder::new());
    let observer: SharedObserver = if cli.verbose {
        Arc::new(FanOut::new(vec![
            recorder.clone(),
            Arc::new(StderrObserver) as SharedObserver,
        ]))
    } else {
        recorder.clone()
    };

    let choice = cli.backend.unwrap_or_else(default_backend);
    let backend = open_backend(choice, observer).await?;
    let agent = Arc::new(AcceptAllAgent::new());

    // A global `--spoof` applies once, up front, with a capability check — so an
    // unsupported backend fails before the command does any partial work.
    if let Some(s) = cli.spoof.as_deref() {
        spoof_local(&*backend, s).await?;
        if cli.verbose {
            eprintln!("local adapter address set to {s}");
        }
    }

    match cli.command {
        Command::Scan => {
            print!(
                "{}",
                dump_in_range(&*backend, &DiscoveryFilter::new()).await?
            );
        }
        Command::Pair { addr } => {
            let id: DeviceId = addr.parse().context("invalid address")?;
            let report = pair_device(&*backend, id, agent, Default::default()).await?;
            println!("paired {id} in {} attempt(s)", report.attempts);
        }
        Command::PairMany { addrs } => {
            let ids = parse_ids(&addrs)?;
            print_items(pair_devices(&*backend, &ids, agent, Default::default()).await);
        }
        Command::PairAll => {
            let items = pair_discovered(
                &*backend,
                &DiscoveryFilter::new(),
                agent,
                Default::default(),
            )
            .await?;
            print_items(items);
        }
        Command::Accept { window } => {
            let opts = InboundPairing::for_window(Duration::from_secs(window));
            println!("Discoverable + pairable for {window}s — pair devices to this adapter now…");
            let bonded = backend.accept_pairings(agent, opts).await?;
            println!("bonded {} device(s):", bonded.len());
            for id in bonded {
                println!("  {id}");
            }
        }
        Command::Send { addr, file } => {
            let id: DeviceId = addr.parse().context("invalid address")?;
            let bytes =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let name = file
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file.bin");
            let receipt = upload_file(
                &*backend,
                id,
                OutboundFile::new(name, &bytes),
                UploadOptions::default(),
            )
            .await?;
            println!(
                "sent {} ({} bytes) at {:.0} B/s",
                receipt.object_name,
                receipt.bytes,
                receipt.throughput_bytes_per_sec()
            );
        }
        Command::Gatt { op } => gatt(&*backend, op).await?,
        Command::Spoof { addr } => {
            spoof_local(&*backend, &addr).await?;
            let info = backend.adapter().await?;
            println!("local adapter address set to {}", info.address);
        }
        Command::Doctor => {
            // Drive a representative session to populate the event stream.
            let _ = dump_in_range(&*backend, &DiscoveryFilter::new()).await;
            println!("{}", Analyzer::from_recorder(&recorder).report());
        }
    }
    Ok(())
}

/// Run one GATT subcommand against the backend.
async fn gatt(backend: &dyn BluetoothBackend, op: GattOp) -> anyhow::Result<()> {
    match op {
        GattOp::Read {
            addr,
            service,
            characteristic,
        } => {
            let conn = backend.connect(addr.parse()?).await?;
            let value = backend
                .gatt_read(&conn, service.parse()?, characteristic.parse()?)
                .await?;
            println!("{characteristic} = {value:02X?}");
            let _ = backend.disconnect(&conn).await;
        }
        GattOp::Write {
            addr,
            service,
            characteristic,
            hex,
        } => {
            let conn = backend.connect(addr.parse()?).await?;
            let data = parse_hex(&hex)?;
            backend
                .gatt_write(
                    &conn,
                    service.parse()?,
                    characteristic.parse()?,
                    &data,
                    true,
                )
                .await?;
            println!("wrote {} byte(s)", data.len());
            let _ = backend.disconnect(&conn).await;
        }
        GattOp::Subscribe {
            addr,
            service,
            characteristic,
            count,
        } => {
            let conn = backend.connect(addr.parse()?).await?;
            let mut stream = backend
                .gatt_subscribe(&conn, service.parse()?, characteristic.parse()?)
                .await?;
            let mut seen = 0;
            while let Some((uuid, value)) = stream.next().await {
                println!("{uuid} = {value:02X?}");
                seen += 1;
                if seen >= count {
                    break;
                }
            }
            let _ = backend.disconnect(&conn).await;
        }
    }
    Ok(())
}

/// Parse and apply a local-address spoof, gating on the backend's capability so
/// an impossible request (a backend that can't change its address) fails with a
/// clear message instead of a confusing partial run.
async fn spoof_local(backend: &dyn BluetoothBackend, addr: &str) -> anyhow::Result<()> {
    let addr: BdAddr = addr.parse().context("invalid spoof address")?;
    let caps = backend.capabilities();
    anyhow::ensure!(
        caps.can_spoof_address,
        "the `{}` backend cannot spoof a local address — use --backend bluez (Linux) or --backend mock",
        caps.kind.name()
    );
    backend
        .set_local_address(addr)
        .await
        .with_context(|| format!("setting local address to {addr}"))?;
    Ok(())
}

/// Print one line per batch-pairing result (id, status, attempt count).
fn print_items(items: Vec<pax_pairing::PairItem>) {
    for item in items {
        let status = match &item.outcome {
            Ok(_) => "paired".to_string(),
            Err(e) => format!("failed ({e})"),
        };
        println!("  {} -> {status} ({} attempt(s))", item.id, item.attempts);
    }
}

/// Parse a list of address strings into [`DeviceId`]s.
fn parse_ids(addrs: &[String]) -> anyhow::Result<Vec<DeviceId>> {
    addrs
        .iter()
        .map(|a| a.parse::<DeviceId>().context("invalid address"))
        .collect()
}

/// Parse a hex string (ignoring whitespace and `:` separators) into bytes.
fn parse_hex(s: &str) -> anyhow::Result<Vec<u8>> {
    let s: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':')
        .collect();
    anyhow::ensure!(
        s.len() % 2 == 0,
        "hex payload must have an even number of digits"
    );
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("invalid hex"))
        .collect()
}

/// The default backend: a real one if compiled in, else the demo mock.
fn default_backend() -> Backend {
    if cfg!(feature = "bluez") {
        Backend::Bluez
    } else if cfg!(feature = "btleplug") {
        Backend::Btleplug
    } else {
        Backend::Mock
    }
}

/// Open the backend the user selected (or the compiled-in default).
async fn open_backend(
    choice: Backend,
    observer: SharedObserver,
) -> anyhow::Result<Arc<dyn BluetoothBackend>> {
    match choice {
        Backend::Mock => Ok(Arc::new(demo_mock(observer))),
        Backend::Bluez => open_bluez(observer).await,
        Backend::Btleplug => open_btleplug(observer).await,
    }
}

/// Open the BlueZ backend, or fail with a rebuild hint if it wasn't compiled in.
#[cfg(feature = "bluez")]
async fn open_bluez(observer: SharedObserver) -> anyhow::Result<Arc<dyn BluetoothBackend>> {
    let backend = pax_transport::bluez::BlueZBackend::connect_default(observer).await?;
    Ok(Arc::new(backend) as Arc<dyn BluetoothBackend>)
}
/// Open the BlueZ backend, or fail with a rebuild hint if it wasn't compiled in.
#[cfg(not(feature = "bluez"))]
async fn open_bluez(_observer: SharedObserver) -> anyhow::Result<Arc<dyn BluetoothBackend>> {
    anyhow::bail!("this build has no `bluez` backend — rebuild with `--features bluez`")
}

/// Open the btleplug backend, or fail with a rebuild hint if it wasn't compiled in.
#[cfg(feature = "btleplug")]
async fn open_btleplug(observer: SharedObserver) -> anyhow::Result<Arc<dyn BluetoothBackend>> {
    let backend = pax_transport::btleplug::BtleplugBackend::connect_first(observer).await?;
    Ok(Arc::new(backend) as Arc<dyn BluetoothBackend>)
}
/// Open the btleplug backend, or fail with a rebuild hint if it wasn't compiled in.
#[cfg(not(feature = "btleplug"))]
async fn open_btleplug(_observer: SharedObserver) -> anyhow::Result<Arc<dyn BluetoothBackend>> {
    anyhow::bail!("this build has no `btleplug` backend — rebuild with `--features btleplug`")
}

/// A few scripted devices so the mock backend demos `scan`/`pair`/`gatt` with no
/// hardware.
fn demo_mock(observer: SharedObserver) -> MockBackend {
    let phone: DeviceId = "11:22:33:44:55:66".parse().unwrap();
    let headset: DeviceId = "AA:BB:CC:DD:EE:FF".parse().unwrap();
    MockBackend::builder()
        .observer(observer)
        .controller(ControllerModel::new(
            CompanyId::INTEL,
            ChipsetFamily::Intel,
            "AX210",
        ))
        .device(
            MockDevice::new(phone, "Demo Phone")
                .with_rssi(-55)
                .with_class(ClassOfDevice::new(0x5A_02_0C))
                .with_gatt(
                    Uuid::from_u16(0x180A),
                    Uuid::from_u16(0x2A29),
                    b"ACME".to_vec(),
                )
                .with_notification(
                    Uuid::from_u16(0x180D),
                    Uuid::from_u16(0x2A37),
                    vec![0x06, 72],
                ),
        )
        .device(MockDevice::new(headset, "Demo Headset").with_rssi(-72))
        .build()
}

/// Prints every event to stderr (for `--verbose`).
struct StderrObserver;
impl Observer for StderrObserver {
    /// Write one formatted event line to stderr.
    fn on_event(&self, event: &TransitEvent) {
        eprintln!("{event}");
    }
}
