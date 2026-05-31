//! How a remote endpoint is identified and described.

use core::fmt;

use crate::address::{AddressType, BdAddr};

/// A stable identifier for a remote Bluetooth device: its address plus how that
/// address should be interpreted.
///
/// Two devices are "the same" iff their [`DeviceId`]s are equal, so this is the
/// natural key for caches and the grouping key used throughout diagnostics.
///
/// ```
/// use pax_core::{DeviceId, AddressType};
/// let id: DeviceId = "00:1A:7D:DA:71:13".parse().unwrap();
/// assert_eq!(id.addr_type, AddressType::Public); // parsing defaults to public
/// assert_eq!(id.to_string(), "00:1A:7D:DA:71:13");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceId {
    /// The device's Bluetooth address.
    pub addr: BdAddr,
    /// How to interpret [`DeviceId::addr`] on the air.
    pub addr_type: AddressType,
}

impl DeviceId {
    /// A public-address device id.
    pub const fn public(addr: BdAddr) -> Self {
        DeviceId {
            addr,
            addr_type: AddressType::Public,
        }
    }

    /// A random-address (LE private) device id.
    pub const fn random(addr: BdAddr) -> Self {
        DeviceId {
            addr,
            addr_type: AddressType::Random,
        }
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The address alone is the human-facing identity; the type is shown only
        // when it is the non-default `random`.
        match self.addr_type {
            AddressType::Public => write!(f, "{}", self.addr),
            AddressType::Random => write!(f, "{} (random)", self.addr),
        }
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({}, {})", self.addr, self.addr_type)
    }
}

impl core::str::FromStr for DeviceId {
    type Err = crate::error::Error;
    /// Parse a public-address device id from an address string. Use
    /// [`DeviceId::random`] explicitly for random addresses.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(DeviceId::public(s.parse()?))
    }
}

impl From<BdAddr> for DeviceId {
    fn from(addr: BdAddr) -> Self {
        DeviceId::public(addr)
    }
}

/// The major device class from the Class-of-Device field (Classic only).
///
/// This is the coarse "what kind of thing is this" classification advertised by
/// BR/EDR devices. It is decoded from bits 8..=12 of the 24-bit CoD.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum MajorDeviceClass {
    /// Miscellaneous / unspecified.
    Miscellaneous,
    /// Desktop, laptop, server, tablet, etc.
    Computer,
    /// Phone (cellular, cordless, smartphone).
    Phone,
    /// LAN / network access point.
    NetworkAccessPoint,
    /// Headset, speaker, headphones, microphone.
    AudioVideo,
    /// Mouse, keyboard, joystick, remote.
    Peripheral,
    /// Display, printer, scanner, camera.
    Imaging,
    /// Wearable (watch, glasses).
    Wearable,
    /// Toy.
    Toy,
    /// Health / fitness device.
    Health,
    /// A class value outside the known majors.
    Uncategorized,
}

impl MajorDeviceClass {
    /// Decode the major class from the 5-bit field value (bits 8..=12 of the CoD).
    const fn from_field(field: u8) -> MajorDeviceClass {
        match field {
            0x00 => MajorDeviceClass::Miscellaneous,
            0x01 => MajorDeviceClass::Computer,
            0x02 => MajorDeviceClass::Phone,
            0x03 => MajorDeviceClass::NetworkAccessPoint,
            0x04 => MajorDeviceClass::AudioVideo,
            0x05 => MajorDeviceClass::Peripheral,
            0x06 => MajorDeviceClass::Imaging,
            0x07 => MajorDeviceClass::Wearable,
            0x08 => MajorDeviceClass::Toy,
            0x09 => MajorDeviceClass::Health,
            _ => MajorDeviceClass::Uncategorized,
        }
    }
}

impl fmt::Display for MajorDeviceClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            MajorDeviceClass::Miscellaneous => "miscellaneous",
            MajorDeviceClass::Computer => "computer",
            MajorDeviceClass::Phone => "phone",
            MajorDeviceClass::NetworkAccessPoint => "network access point",
            MajorDeviceClass::AudioVideo => "audio/video",
            MajorDeviceClass::Peripheral => "peripheral",
            MajorDeviceClass::Imaging => "imaging",
            MajorDeviceClass::Wearable => "wearable",
            MajorDeviceClass::Toy => "toy",
            MajorDeviceClass::Health => "health",
            MajorDeviceClass::Uncategorized => "uncategorized",
        };
        f.write_str(s)
    }
}

/// The 24-bit Class-of-Device value advertised by Classic devices.
///
/// ```
/// use pax_core::{ClassOfDevice, MajorDeviceClass};
/// // 0x5A020C = computer/laptop with networking+capturing service bits.
/// let cod = ClassOfDevice::new(0x10_01_0C);
/// assert_eq!(cod.major(), MajorDeviceClass::Computer);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassOfDevice(pub u32);

impl ClassOfDevice {
    /// Wrap a raw 24-bit CoD value.
    pub const fn new(raw: u32) -> Self {
        ClassOfDevice(raw & 0x00FF_FFFF)
    }

    /// The decoded major device class (bits 8..=12).
    pub const fn major(self) -> MajorDeviceClass {
        let field = ((self.0 >> 8) & 0b1_1111) as u8;
        MajorDeviceClass::from_field(field)
    }

    /// The raw service-class bits (bits 13..=23), as a bitmask.
    pub const fn service_bits(self) -> u16 {
        ((self.0 >> 13) & 0x07FF) as u16
    }
}

impl fmt::Debug for ClassOfDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClassOfDevice(0x{:06X} {})", self.0, self.major())
    }
}

/// A snapshot of everything known about a discovered or connected device.
///
/// Most fields are optional because what is known depends on how the device was
/// observed (an advertising LE beacon yields different fields than a paired
/// Classic device).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceInfo {
    /// The device identity (always known).
    pub id: DeviceId,
    /// The friendly name, if advertised or resolved.
    pub name: Option<String>,
    /// The Class-of-Device, if this is a Classic device.
    pub class: Option<ClassOfDevice>,
    /// The most recent RSSI in dBm, if measured.
    pub rssi: Option<i16>,
    /// The advertised TX power in dBm, if present.
    pub tx_power: Option<i16>,
    /// Whether the local stack has bonded with this device.
    pub paired: bool,
    /// Whether there is a live connection right now.
    pub connected: bool,
    /// LE "manufacturer specific data" entries, `(company, bytes)`. Used to infer
    /// the peer's platform (e.g. Apple's company id marks Apple devices); see
    /// [`crate::peer::PeerPlatform`].
    pub manufacturer_data: Vec<(crate::hardware::CompanyId, Vec<u8>)>,
    /// Advertised / known service UUIDs, as strings (e.g. `"0000110a-0000-..."`).
    pub services: Vec<String>,
}

impl DeviceInfo {
    /// A minimal `DeviceInfo` carrying only the identity.
    pub fn new(id: DeviceId) -> Self {
        DeviceInfo {
            id,
            ..DeviceInfo::default()
        }
    }

    /// Builder-style setter for the friendly name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Builder-style setter for RSSI.
    pub fn with_rssi(mut self, rssi: i16) -> Self {
        self.rssi = Some(rssi);
        self
    }

    /// Builder-style setter for the Class-of-Device.
    pub fn with_class(mut self, class: ClassOfDevice) -> Self {
        self.class = Some(class);
        self
    }

    /// Builder-style setter for one manufacturer-data entry.
    pub fn with_manufacturer_data(
        mut self,
        company: crate::hardware::CompanyId,
        data: impl Into<Vec<u8>>,
    ) -> Self {
        self.manufacturer_data.push((company, data.into()));
        self
    }

    /// Builder-style setter for one advertised service UUID.
    pub fn with_service(mut self, uuid: impl Into<String>) -> Self {
        self.services.push(uuid.into());
        self
    }

    /// The best human label available: the name if known, else the address.
    pub fn label(&self) -> String {
        match &self.name {
            Some(n) => format!("{n} [{}]", self.id),
            None => self.id.to_string(),
        }
    }

    /// A detailed, multi-line, human-readable dump of everything known about this
    /// device — address, inferred platform, signal, Class-of-Device, bond state,
    /// vendor data (with company names), and service UUIDs. Fields that are unknown
    /// are omitted. Pairs with [`crate::peer::detect_platform`].
    ///
    /// ```
    /// use pax_core::{DeviceId, DeviceInfo, ClassOfDevice, CompanyId};
    /// let id: DeviceId = "11:22:33:44:55:66".parse().unwrap();
    /// let dump = DeviceInfo::new(id)
    ///     .with_name("Pixel 8")
    ///     .with_rssi(-57)
    ///     .with_class(ClassOfDevice::new(0x5A_02_0C))
    ///     .dump();
    /// assert!(dump.contains("11:22:33:44:55:66"));
    /// assert!(dump.contains("Pixel 8"));
    /// assert!(dump.contains("Android"));   // inferred platform
    /// assert!(dump.contains("-57 dBm"));
    /// ```
    pub fn dump(&self) -> String {
        use core::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "Device {}", self.id);

        let mut line = |label: &str, value: &str| {
            let _ = writeln!(s, "  {label:<13}{value}");
        };

        if let Some(name) = &self.name {
            line("name:", name);
        }
        let platform = crate::peer::detect_platform(self);
        if platform != crate::peer::PeerPlatform::Unknown {
            line("platform:", platform.label());
        }
        if let Some(rssi) = self.rssi {
            line("rssi:", &format!("{rssi} dBm"));
        }
        if let Some(tx) = self.tx_power {
            line("tx power:", &format!("{tx} dBm"));
        }
        if let Some(cod) = self.class {
            line("class:", &format!("0x{:06X} ({})", cod.0, cod.major()));
        }
        line("paired:", if self.paired { "yes" } else { "no" });
        line("connected:", if self.connected { "yes" } else { "no" });
        for (company, data) in &self.manufacturer_data {
            let hex: Vec<String> = data.iter().map(|b| format!("{b:02X}")).collect();
            line(
                "vendor data:",
                &format!("{company} (0x{:04X}): {}", company.0, hex.join(" ")),
            );
        }
        for uuid in &self.services {
            line("service:", uuid);
        }
        s
    }

    /// Best-effort guess of the peer's platform from the advertised data.
    /// Convenience for [`crate::peer::detect_platform`].
    pub fn platform(&self) -> crate::peer::PeerPlatform {
        crate::peer::detect_platform(self)
    }
}

// `DeviceId` needs a Default for `DeviceInfo::default()`; the nil address is the
// natural "unset" value.
impl Default for DeviceId {
    fn default() -> Self {
        DeviceId::public(BdAddr::NIL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_device_id_defaults_public() {
        let id: DeviceId = "00:1A:7D:DA:71:13".parse().unwrap();
        assert_eq!(id.addr_type, AddressType::Public);
    }

    #[test]
    fn class_of_device_major_decode() {
        // major field 0x01 == Computer.
        let cod = ClassOfDevice::new(0x00_01_00);
        assert_eq!(cod.major(), MajorDeviceClass::Computer);
        // major field 0x04 == Audio/Video (e.g. a headset).
        let av = ClassOfDevice::new(0x24_04_18);
        assert_eq!(av.major(), MajorDeviceClass::AudioVideo);
    }

    #[test]
    fn device_info_label_prefers_name() {
        let id: DeviceId = "00:1A:7D:DA:71:13".parse().unwrap();
        let with = DeviceInfo::new(id).with_name("Pixel");
        assert_eq!(with.label(), "Pixel [00:1A:7D:DA:71:13]");
        let without = DeviceInfo::new(id);
        assert_eq!(without.label(), "00:1A:7D:DA:71:13");
    }
}
