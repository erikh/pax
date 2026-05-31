//! End-to-end test of the *entire* toolkit with no Bluetooth hardware.
//!
//! This is the connection-free proof that the layers compose: it builds a mock
//! backend with two devices on two different simulated controllers and two
//! different specs, runs a realistic session (discover → pair → upload) through the
//! high-level `pax-pairing` / `pax-transfer` APIs, records every in-transit event,
//! and asserts that the diagnostics analyzer splits the stream correctly by
//! hardware model, Bluetooth spec, and the IEEE 802 axes.

use std::sync::Arc;

use pax_core::{
    ChipsetFamily, CompanyId, ControllerModel, CoreVersion, DeviceId, PortAuthState,
    SharedObserver, SpecContext, Standard802, StandardsProfile, Transport,
};
use pax_diagnostics::{Analyzer, Recorder, SplitBy};
use pax_pairing::{agents::AcceptAllAgent, pair_device, PairOptions};
use pax_transfer::{upload_bytes, UploadOptions};
use pax_transport::mock::{MockBackend, MockDevice};
use pax_transport::BluetoothBackend;
use pax_transport::DiscoveryFilter;

fn phone() -> DeviceId {
    "11:22:33:44:55:66".parse().unwrap()
}

fn laptop() -> DeviceId {
    "AA:BB:CC:DD:EE:FF".parse().unwrap()
}

#[tokio::test]
async fn full_session_is_recorded_and_analyzable_without_hardware() {
    // A recorder is just an Observer; attach it to the backend.
    let recorder = Arc::new(Recorder::new());

    // The local controller the mock reports — an Intel AX210, our "hardware model".
    let controller = ControllerModel::new(CompanyId::INTEL, ChipsetFamily::Intel, "AX210");

    // Two scripted peers with deliberately different specs and 802 profiles so the
    // diagnostic split has something to separate.
    let phone_dev = MockDevice::new(phone(), "Test Phone")
        .with_rssi(-55)
        .with_spec(SpecContext::new(CoreVersion::V5_2, Transport::BrEdr))
        .with_standards(StandardsProfile::bredr_default());

    let laptop_dev = MockDevice::new(laptop(), "Test Laptop")
        .with_rssi(-65)
        // A PAN bridged onto an authenticated switch port: 802.1X authenticated.
        .with_spec(SpecContext::new(CoreVersion::V5_0, Transport::Dual))
        .with_standards(
            StandardsProfile::bredr_default()
                .with_radio(Standard802::Wpan80215_1)
                .with_port_auth(PortAuthState::Authenticated),
        );

    let backend = MockBackend::builder()
        .controller(controller)
        .observer(recorder.clone() as SharedObserver)
        .device(phone_dev)
        .device(laptop_dev)
        .build();

    // 1) Discover.
    let found = backend.discover(&DiscoveryFilter::new()).await.unwrap();
    assert_eq!(found.len(), 2);

    // 2) Pair with both, using the high-level retrying workflow.
    let agent: Arc<AcceptAllAgent> = Arc::new(AcceptAllAgent::new());
    let p1 = pair_device(&backend, phone(), agent.clone(), PairOptions::default())
        .await
        .unwrap();
    let p2 = pair_device(&backend, laptop(), agent.clone(), PairOptions::default())
        .await
        .unwrap();
    assert!(p1.outcome.paired && p2.outcome.paired);

    // 3) Upload a file to each, via the high-level transfer API.
    let phone_payload = vec![0xABu8; 8192]; // 8 chunks at 1 KiB
    let laptop_payload = vec![0xCDu8; 2048]; // 2 chunks
    let r1 = upload_bytes(
        &backend,
        phone(),
        "a.bin",
        &phone_payload,
        UploadOptions::default(),
    )
    .await
    .unwrap();
    let r2 = upload_bytes(
        &backend,
        laptop(),
        "b.bin",
        &laptop_payload,
        UploadOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(r1.bytes, 8192);
    assert_eq!(r2.bytes, 2048);

    // 4) Analyze the recorded stream.
    let analyzer = Analyzer::from_recorder(&recorder);
    assert!(analyzer.event_count() > 0);

    let total = analyzer.total();
    // All payload bytes are accounted for across both transfers.
    assert_eq!(total.bytes, 8192 + 2048);
    assert_eq!(total.failures, 0);

    // Split by Bluetooth spec: 5.2/BR-EDR and 5.0/dual-mode are distinct buckets.
    let by_spec = analyzer.breakdown(SplitBy::Spec);
    assert_eq!(by_spec.len(), 2, "two distinct specs expected");

    // Split by 802.1X port-auth: one peer is n/a, the other authenticated.
    let by_auth = analyzer.breakdown(SplitBy::PortAuth);
    assert!(by_auth.buckets.contains_key("n/a"));
    assert!(by_auth.buckets.contains_key("authenticated"));

    // Split by peer: the phone carried more bytes than the laptop.
    let by_peer = analyzer.breakdown(SplitBy::Peer);
    let busiest = by_peer.busiest().unwrap();
    assert_eq!(busiest.1.bytes, 8192);

    // Everything happened on one controller model.
    let by_hw = analyzer.breakdown(SplitBy::Controller);
    assert_eq!(by_hw.len(), 1);
    assert!(by_hw.buckets.keys().next().unwrap().contains("AX210"));

    // The rendered report mentions each required axis (smoke test of Display).
    let report = analyzer.report().to_string();
    for needle in [
        "Controller hardware",
        "Bluetooth specification",
        "802.15.x radio standard",
        "802.1X port-auth state",
    ] {
        assert!(report.contains(needle), "report missing section: {needle}");
    }
}

#[tokio::test]
async fn diagnostics_capture_a_failed_transfer() {
    let recorder = Arc::new(Recorder::new());
    let flaky = MockDevice::new(phone(), "Flaky").failing_transfer_after(1024);
    let backend = MockBackend::builder()
        .observer(recorder.clone() as SharedObserver)
        .device(flaky)
        .build();

    let data = vec![0u8; 4096];
    // retries: 0 so the failure surfaces immediately.
    let err = upload_bytes(
        &backend,
        phone(),
        "doomed.bin",
        &data,
        UploadOptions {
            retries: 0,
            disconnect_after: true,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        pax_transport::TransportError::TransferFailed { .. }
    ));

    let analyzer = Analyzer::from_recorder(&recorder);
    let anomalies = analyzer.anomalies();
    assert!(!anomalies.is_empty(), "expected at least one anomaly");
    assert!(anomalies.iter().any(|a| a.code == "error"));
}
