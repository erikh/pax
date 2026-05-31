//! Hardware-gated smoke tests for the real backends.
//!
//! These need an actual Bluetooth adapter, so they are `#[ignore]`d by default and
//! additionally guarded by the `PAX_HW_TESTS` environment variable — they compile
//! in CI (when the feature is on) but never *run* without hardware. To run them on
//! a real Linux host:
//!
//! ```text
//! PAX_HW_TESTS=1 cargo test -p pax-transport --features bluez --test hardware -- --ignored --nocapture
//! PAX_HW_TESTS=1 cargo test -p pax-transport --features btleplug --test hardware -- --ignored --nocapture
//! ```
//!
//! They are deliberately read-only (open the adapter, scan) so they are safe to run
//! against a daily-driver machine. Pairing and file push are interactive and live
//! in the manual smoke-test in the README.

#[allow(dead_code)]
fn hw_enabled() -> bool {
    std::env::var_os("PAX_HW_TESTS").is_some()
}

#[cfg(feature = "bluez")]
mod bluez_hw {
    use super::hw_enabled;
    use std::sync::Arc;

    use pax_core::{DeviceId, Duration};
    use pax_pairing::{agents::AcceptAllAgent, pair_devices, BatchPairOptions};
    use pax_transport::bluez::BlueZBackend;
    use pax_transport::{BluetoothBackend, DiscoveryFilter, InboundPairing};

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires a real Bluetooth adapter; set PAX_HW_TESTS=1"]
    async fn adapter_reports_real_controller() {
        if !hw_enabled() {
            return;
        }
        let backend = BlueZBackend::open().await.expect("open default adapter");
        let info = backend.adapter().await.expect("read adapter info");
        println!(
            "adapter {} — controller {} — spec {}",
            info.name, info.controller, info.spec
        );
        assert!(!info.name.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires a real Bluetooth adapter; set PAX_HW_TESTS=1"]
    async fn discovery_runs() {
        if !hw_enabled() {
            return;
        }
        let backend = BlueZBackend::open()
            .await
            .expect("open default adapter")
            .with_discovery_window(std::time::Duration::from_secs(4));
        let found = backend
            .discover(&DiscoveryFilter::new().limit(20))
            .await
            .expect("discover");
        println!("discovered {} device(s)", found.len());
        for d in &found {
            println!("  {}", d.label());
        }
    }

    /// Inbound "broadcast" pairing mode: make this adapter discoverable + pairable
    /// and accept bonds from devices that pair *to* it. Pair a phone to the laptop
    /// while this runs. Window seconds come from `PAX_HW_PAIR_WINDOW` (default 20).
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "interactive: makes the adapter pairable; set PAX_HW_TESTS=1"]
    async fn inbound_pairing_mode() {
        if !hw_enabled() {
            return;
        }
        let secs: u64 = std::env::var("PAX_HW_PAIR_WINDOW")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);
        let backend = BlueZBackend::open().await.expect("open default adapter");
        println!(
            "** Adapter is now discoverable + pairable for {secs}s — pair a phone to it now **"
        );
        let bonded = backend
            .accept_pairings(
                Arc::new(AcceptAllAgent::new()),
                InboundPairing::for_window(Duration::from_secs(secs)),
            )
            .await
            .expect("accept_pairings");
        println!("bonded {} device(s):", bonded.len());
        for id in &bonded {
            println!("  {id}");
        }
    }

    /// Outbound batch (round-robin) pairing. Set
    /// `PAX_HW_PAIR_TARGETS=AA:BB:..,CC:DD:..` to a comma-separated list of device
    /// addresses to pair; skipped if unset.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "interactive: pairs real devices; set PAX_HW_TESTS=1 + PAX_HW_PAIR_TARGETS"]
    async fn outbound_batch_pairing() {
        if !hw_enabled() {
            return;
        }
        let targets: Vec<DeviceId> = std::env::var("PAX_HW_PAIR_TARGETS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse()
                    .expect("PAX_HW_PAIR_TARGETS must be valid addresses")
            })
            .collect();
        if targets.is_empty() {
            println!("set PAX_HW_PAIR_TARGETS=addr1,addr2 to run this test");
            return;
        }
        let backend = BlueZBackend::open().await.expect("open default adapter");
        let results = pair_devices(
            &backend,
            &targets,
            Arc::new(AcceptAllAgent::new()),
            BatchPairOptions::default(),
        )
        .await;
        for r in &results {
            println!(
                "  {} -> {} ({} attempt(s))",
                r.id,
                if r.paired() { "paired" } else { "failed" },
                r.attempts
            );
        }
        assert_eq!(results.len(), targets.len());
    }
}

#[cfg(feature = "btleplug")]
mod btleplug_hw {
    use super::hw_enabled;
    use pax_transport::btleplug::BtleplugBackend;
    use pax_transport::{BluetoothBackend, DiscoveryFilter};

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires a real BLE adapter; set PAX_HW_TESTS=1"]
    async fn ble_discovery_runs() {
        if !hw_enabled() {
            return;
        }
        let backend = BtleplugBackend::open().await.expect("open BLE adapter");
        let found = backend
            .discover(&DiscoveryFilter::new().limit(20))
            .await
            .expect("scan");
        println!("discovered {} BLE device(s)", found.len());
    }
}
