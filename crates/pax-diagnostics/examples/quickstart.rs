//! The README quick-start, as a runnable example. Drives the mock backend through
//! a full discover → pair → upload session, records the in-transit events, and
//! prints a diagnostic report — with no Bluetooth hardware.
//!
//! Run it with:  `cargo run -p pax-diagnostics --example quickstart`

use std::sync::Arc;

use pax_core::{ChipsetFamily, CompanyId, ControllerModel, DeviceId, SharedObserver};
use pax_diagnostics::{Analyzer, Recorder, SplitBy};
use pax_pairing::{agents::AcceptAllAgent, pair_device, PairOptions};
use pax_transfer::{upload_bytes, UploadOptions};
use pax_transport::mock::{MockBackend, MockDevice};
use pax_transport::{BluetoothBackend, DiscoveryFilter};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), pax_transport::TransportError> {
    // 1. Capture every in-transit event with a Recorder (it's just an Observer).
    let recorder = Arc::new(Recorder::new());

    // 2. Build a backend. Here it's the mock; swap in a real one unchanged.
    let phone: DeviceId = "11:22:33:44:55:66".parse().unwrap();
    let backend = MockBackend::builder()
        .controller(ControllerModel::new(
            CompanyId::INTEL,
            ChipsetFamily::Intel,
            "AX210",
        ))
        .observer(recorder.clone() as SharedObserver)
        .device(MockDevice::new(phone, "Test Phone").with_rssi(-55))
        .build();

    // 3. Discover, pair, upload — using the high-level workflows.
    let found = backend.discover(&DiscoveryFilter::new()).await?;
    println!("found {} device(s)", found.len());

    pair_device(
        &backend,
        phone,
        Arc::new(AcceptAllAgent::new()),
        PairOptions::default(),
    )
    .await?;

    let receipt = upload_bytes(
        &backend,
        phone,
        "hello.txt",
        b"the quick brown fox",
        UploadOptions::default(),
    )
    .await?;
    println!("uploaded {} bytes in {:?}", receipt.bytes, receipt.duration);

    // 4. Analyze the recorded stream and print the report.
    let report = Analyzer::from_recorder(&recorder).report();
    println!("\n{report}");

    // ...or ask for one split dimension directly:
    let by_hw = Analyzer::from_recorder(&recorder).breakdown(SplitBy::Controller);
    println!("controllers seen: {}", by_hw.len());

    Ok(())
}
