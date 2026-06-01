//! End-to-end tests for local-address impersonation through the public API,
//! against the deterministic mock backend (no hardware required).
//!
//! These exercise the same `set_local_address` / `apply_spoof` / `_as` helpers the
//! real backends and the CLI use, so the spoofing pipeline is validated in CI.

use pax_core::{BdAddr, DeviceId, Uuid};
use pax_transport::mock::{MockBackend, MockDevice};
use pax_transport::{
    apply_spoof, connect_as, discover_as, dump_in_range_as, BluetoothBackend, DiscoveryFilter,
};

fn sensor() -> DeviceId {
    "AA:00:00:00:00:01".parse().unwrap()
}

fn backend() -> MockBackend {
    let svc = Uuid::from_u16(0x180A);
    let chr = Uuid::from_u16(0x2A29);
    MockBackend::builder()
        .adapter_address(BdAddr::new([0x00, 0x00, 0x00, 0x00, 0x00, 0x01]))
        .device(
            MockDevice::new(sensor(), "Sensor")
                .with_rssi(-50)
                .with_gatt(svc, chr, b"ACME".to_vec()),
        )
        .build()
}

#[tokio::test]
async fn scan_connect_and_gatt_under_a_spoofed_identity() {
    let backend = backend();
    let spoof: BdAddr = "02:00:00:DE:AD:BE".parse().unwrap();

    // Scan under the spoofed identity (dump form), then confirm the adapter took
    // on the impersonated address.
    let report = dump_in_range_as(&backend, Some(spoof), &DiscoveryFilter::new())
        .await
        .unwrap();
    assert!(report.contains("1 device(s) in range"));
    assert_eq!(backend.adapter().await.unwrap().address, spoof);

    // Connect under a *different* spoofed identity and read GATT through it.
    let other: BdAddr = "02:00:00:00:00:42".parse().unwrap();
    let conn = connect_as(&backend, Some(other), sensor()).await.unwrap();
    assert_eq!(backend.adapter().await.unwrap().address, other);
    let value = backend
        .gatt_read(&conn, Uuid::from_u16(0x180A), Uuid::from_u16(0x2A29))
        .await
        .unwrap();
    assert_eq!(value, b"ACME");
}

#[tokio::test]
async fn discover_as_without_spoof_keeps_the_real_address() {
    let backend = backend();
    let original = backend.adapter().await.unwrap().address;

    // None means "do not spoof": the address is unchanged and discovery still runs.
    let found = discover_as(&backend, None, &DiscoveryFilter::new())
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(backend.adapter().await.unwrap().address, original);
}

#[tokio::test]
async fn apply_spoof_is_idempotent_and_repeatable() {
    let backend = backend();
    let a: BdAddr = "02:00:00:00:00:0A".parse().unwrap();
    let b: BdAddr = "02:00:00:00:00:0B".parse().unwrap();

    apply_spoof(&backend, Some(a)).await.unwrap();
    apply_spoof(&backend, Some(a)).await.unwrap(); // same address again: fine
    assert_eq!(backend.adapter().await.unwrap().address, a);

    apply_spoof(&backend, Some(b)).await.unwrap(); // switch identities
    assert_eq!(backend.adapter().await.unwrap().address, b);
}
