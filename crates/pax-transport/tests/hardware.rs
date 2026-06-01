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

    /// Dump detailed info for every device in range (read-only).
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires a real Bluetooth adapter; set PAX_HW_TESTS=1"]
    async fn dump_devices_in_range() {
        if !hw_enabled() {
            return;
        }
        let backend = BlueZBackend::open()
            .await
            .expect("open default adapter")
            .with_discovery_window(std::time::Duration::from_secs(4));
        let report = pax_transport::dump_in_range(&backend, &DiscoveryFilter::new().limit(30))
            .await
            .expect("dump");
        println!("{report}");
        assert!(report.contains("device(s) in range"));
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

    /// Spoof the local controller address and restore it. **Mutating** — unlike
    /// the other bluez tests this reprograms the adapter, so it is gated by an
    /// extra `PAX_HW_SPOOF=1` opt-in on top of `PAX_HW_TESTS`, and it needs root
    /// (`CAP_NET_ADMIN`) plus a controller whose driver supports the change. It
    /// reads the current address, sets a locally-administered test address,
    /// verifies the change, then restores the original.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "MUTATES the adapter address; set PAX_HW_TESTS=1 PAX_HW_SPOOF=1 (needs root)"]
    async fn spoof_local_address_roundtrip() {
        if !hw_enabled() || std::env::var_os("PAX_HW_SPOOF").is_none() {
            return;
        }
        use pax_core::BdAddr;

        let backend = BlueZBackend::open().await.expect("open default adapter");
        assert!(
            backend.capabilities().can_spoof_address,
            "bluez should advertise spoofing support"
        );

        let original = backend.adapter().await.expect("read adapter").address;
        println!("original address: {original}");

        // A locally-administered test address (bit 0x02 of the top octet).
        let test: BdAddr = "02:00:00:13:37:01".parse().unwrap();
        backend
            .set_local_address(test)
            .await
            .expect("set test address (needs root + supported controller)");
        let now = backend.adapter().await.expect("read adapter").address;
        println!("after spoof: {now}");
        assert_eq!(now, test, "controller did not take the spoofed address");

        // Always try to restore the original so the machine is left as we found it.
        backend
            .set_local_address(original)
            .await
            .expect("restore original address");
        assert_eq!(backend.adapter().await.unwrap().address, original);
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
    use pax_core::{DeviceId, Uuid};
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

    /// Connect to a BLE device and read a GATT characteristic. Set
    /// `PAX_HW_GATT_ADDR`, `PAX_HW_GATT_SVC`, `PAX_HW_GATT_CHR` (UUIDs, short or
    /// full); skipped if unset.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "interactive: connects + reads GATT; set PAX_HW_TESTS=1 + PAX_HW_GATT_*"]
    async fn gatt_read_env() {
        if !hw_enabled() {
            return;
        }
        let (addr, svc, chr) = match (
            std::env::var("PAX_HW_GATT_ADDR").ok(),
            std::env::var("PAX_HW_GATT_SVC").ok(),
            std::env::var("PAX_HW_GATT_CHR").ok(),
        ) {
            (Some(a), Some(s), Some(c)) => (a, s, c),
            _ => {
                println!("set PAX_HW_GATT_ADDR/SVC/CHR to run this test");
                return;
            }
        };
        let target: DeviceId = addr.parse().expect("addr");
        let svc: Uuid = svc.parse().expect("service uuid");
        let chr: Uuid = chr.parse().expect("characteristic uuid");

        let backend = BtleplugBackend::open().await.expect("open BLE adapter");
        // Make sure the device is discovered (so btleplug has a handle), then connect.
        let _ = backend.discover(&DiscoveryFilter::new()).await;
        let conn = backend.connect(target).await.expect("connect");
        let value = backend.gatt_read(&conn, svc, chr).await.expect("gatt read");
        println!("{chr} = {value:02X?}");
        let _ = backend.disconnect(&conn).await;
    }
}
