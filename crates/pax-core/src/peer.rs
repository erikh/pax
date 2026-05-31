//! Inferring what *kind of device* a peer is — in particular telling an **Android**
//! phone from an **iPhone**.
//!
//! When you connect *to* a phone (rather than running on one), it helps to know
//! which platform it is: an iPhone will refuse Classic OBEX file push (Apple
//! exposes only BLE to third parties), whereas an Android phone accepts it. This
//! module makes a best-effort guess from what a device advertises — its
//! Class-of-Device, the company id in its LE manufacturer data, and its name.
//!
//! The guess is a **heuristic**, not a guarantee: Bluetooth has no authenticated
//! "I am an iPhone" field. Treat [`PeerPlatform`] as a hint for UX and for
//! choosing a transfer strategy, not as a security boundary.

use crate::device::{DeviceInfo, MajorDeviceClass};
use crate::hardware::CompanyId;

/// A best-effort classification of a peer device's platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum PeerPlatform {
    /// An Android phone or tablet.
    Android,
    /// An Apple iPhone / iPad / iPod (iOS / iPadOS).
    IPhone,
    /// An Apple Mac (macOS).
    AppleMac,
    /// A Windows PC.
    Windows,
    /// Could not be determined.
    #[default]
    Unknown,
}

impl PeerPlatform {
    /// `true` if this platform can receive a Classic OBEX Object Push (file
    /// upload). iPhones cannot (third-party apps get BLE only); Android can.
    ///
    /// ```
    /// use pax_core::peer::PeerPlatform;
    /// assert!(PeerPlatform::Android.supports_obex_push());
    /// assert!(!PeerPlatform::IPhone.supports_obex_push());
    /// ```
    pub const fn supports_obex_push(self) -> bool {
        matches!(
            self,
            PeerPlatform::Android | PeerPlatform::AppleMac | PeerPlatform::Windows
        )
    }

    /// A short human label.
    pub const fn label(self) -> &'static str {
        match self {
            PeerPlatform::Android => "Android",
            PeerPlatform::IPhone => "iPhone/iOS",
            PeerPlatform::AppleMac => "Mac",
            PeerPlatform::Windows => "Windows",
            PeerPlatform::Unknown => "unknown",
        }
    }
}

impl core::fmt::Display for PeerPlatform {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

/// Whether the device advertises Apple's company id in any manufacturer-data entry.
fn is_apple(info: &DeviceInfo) -> bool {
    info.manufacturer_data
        .iter()
        .any(|(company, _)| *company == CompanyId::APPLE)
}

/// Best-effort platform detection from a [`DeviceInfo`].
///
/// Resolution order, most specific first:
/// 1. The friendly name (e.g. contains "iPhone", "Galaxy", "MacBook").
/// 2. Apple company id in manufacturer data, refined by Class-of-Device.
/// 3. Class-of-Device alone (a non-Apple phone is taken to be Android).
///
/// ```
/// use pax_core::{DeviceId, DeviceInfo, ClassOfDevice, CompanyId};
/// use pax_core::peer::{detect_platform, PeerPlatform};
///
/// let id: DeviceId = "00:1A:7D:DA:71:13".parse().unwrap();
///
/// // An iPhone: Apple company id + a phone Class-of-Device.
/// let iphone = DeviceInfo::new(id)
///     .with_class(ClassOfDevice::new(0x7A_02_0C))      // major 0x02 == phone
///     .with_manufacturer_data(CompanyId::APPLE, [0x10, 0x05]);
/// assert_eq!(detect_platform(&iphone), PeerPlatform::IPhone);
///
/// // A Pixel: phone Class-of-Device, no Apple id.
/// let pixel = DeviceInfo::new(id).with_name("Pixel 8");
/// assert_eq!(detect_platform(&pixel), PeerPlatform::Android);
/// ```
pub fn detect_platform(info: &DeviceInfo) -> PeerPlatform {
    // 1. Name heuristics — strongest signal when present.
    if let Some(name) = &info.name {
        let n = name.to_lowercase();
        if n.contains("iphone") || n.contains("ipad") || n.contains("ipod") {
            return PeerPlatform::IPhone;
        }
        if n.contains("macbook")
            || n.contains("imac")
            || n.contains("mac mini")
            || n.contains("mac studio")
            || n.contains("mac pro")
        {
            return PeerPlatform::AppleMac;
        }
        const ANDROID_HINTS: [&str; 9] = [
            "android", "galaxy", "pixel", "oneplus", "xiaomi", "redmi", "oppo", "vivo", "nexus",
        ];
        if ANDROID_HINTS.iter().any(|h| n.contains(h)) {
            return PeerPlatform::Android;
        }
        if n.contains("surface") || n.contains("windows") {
            return PeerPlatform::Windows;
        }
    }

    // 2. Apple company id, refined by Class-of-Device.
    let major = info.class.map(|c| c.major());
    if is_apple(info) {
        return match major {
            Some(MajorDeviceClass::Computer) => PeerPlatform::AppleMac,
            Some(MajorDeviceClass::Phone) => PeerPlatform::IPhone,
            // Apple, but the Class-of-Device doesn't disambiguate (e.g. a BLE-only
            // advertiser): default to iPhone, the most common Apple peer.
            _ => PeerPlatform::IPhone,
        };
    }

    // 3. Class-of-Device alone. A non-Apple phone is, in practice, Android.
    match major {
        Some(MajorDeviceClass::Phone) => PeerPlatform::Android,
        _ => PeerPlatform::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{ClassOfDevice, DeviceId, DeviceInfo};

    fn id() -> DeviceId {
        "00:1A:7D:DA:71:13".parse().unwrap()
    }

    #[test]
    fn name_beats_everything() {
        let i = DeviceInfo::new(id()).with_name("Erik's iPhone");
        assert_eq!(detect_platform(&i), PeerPlatform::IPhone);
        let a = DeviceInfo::new(id()).with_name("Galaxy S24");
        assert_eq!(detect_platform(&a), PeerPlatform::Android);
        let m = DeviceInfo::new(id()).with_name("Erik's MacBook Pro");
        assert_eq!(detect_platform(&m), PeerPlatform::AppleMac);
    }

    #[test]
    fn apple_id_with_phone_cod_is_iphone() {
        let i = DeviceInfo::new(id())
            .with_class(ClassOfDevice::new(0x7A_02_0C))
            .with_manufacturer_data(CompanyId::APPLE, vec![0x10, 0x05]);
        assert_eq!(detect_platform(&i), PeerPlatform::IPhone);
    }

    #[test]
    fn apple_id_with_computer_cod_is_mac() {
        let m = DeviceInfo::new(id())
            .with_class(ClassOfDevice::new(0x00_01_0C))
            .with_manufacturer_data(CompanyId::APPLE, vec![0x10]);
        assert_eq!(detect_platform(&m), PeerPlatform::AppleMac);
    }

    #[test]
    fn nonapple_phone_is_android() {
        let a = DeviceInfo::new(id()).with_class(ClassOfDevice::new(0x5A_02_0C));
        assert_eq!(detect_platform(&a), PeerPlatform::Android);
    }

    #[test]
    fn unknown_when_no_signal() {
        let u = DeviceInfo::new(id());
        assert_eq!(detect_platform(&u), PeerPlatform::Unknown);
    }

    #[test]
    fn obex_capability_matches_platform() {
        assert!(PeerPlatform::Android.supports_obex_push());
        assert!(!PeerPlatform::IPhone.supports_obex_push());
    }
}
