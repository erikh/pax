//! Print the local controller's manufacturer and HCI version, read from the
//! kernel management socket. Needs CAP_NET_ADMIN (run with sudo) to succeed.
//!
//!   cargo build -p pax-hci --example probe && sudo ./target/debug/examples/probe

fn main() {
    let index: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    match pax_hci::read_controller_info(index) {
        Some(info) => {
            let company = pax_core_company_name(info.manufacturer);
            println!(
                "hci{index}: manufacturer = 0x{:04X} ({company}), HCI version = {} (= {})",
                info.manufacturer,
                info.hci_version,
                core_version_name(info.hci_version),
            );
        }
        None => println!(
            "hci{index}: read failed — need CAP_NET_ADMIN? try: sudo {} {index}",
            std::env::args().next().unwrap_or_default()
        ),
    }
}

// Tiny inline lookups so the example needs no extra deps.
fn pax_core_company_name(id: u16) -> &'static str {
    match id {
        0x0000 => "Ericsson",
        0x0002 => "Intel",
        0x000A => "CSR",
        0x000F => "Broadcom",
        0x001D => "Qualcomm",
        0x004C => "Apple",
        0x005D => "Realtek",
        _ => "unknown",
    }
}

fn core_version_name(hci: u8) -> &'static str {
    match hci {
        6 => "Bluetooth 4.0",
        7 => "Bluetooth 4.1",
        8 => "Bluetooth 4.2",
        9 => "Bluetooth 5.0",
        10 => "Bluetooth 5.1",
        11 => "Bluetooth 5.2",
        12 => "Bluetooth 5.3",
        13 => "Bluetooth 5.4",
        14 => "Bluetooth 6.0",
        _ => "see HCI table",
    }
}
