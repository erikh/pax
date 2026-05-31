//! The Bluetooth *specification* a link is operating under.
//!
//! Diagnostics split an event stream by Bluetooth spec so you can compare, e.g.,
//! a legacy BR/EDR 2.1 link against a 5.x LE link on the same controller. The two
//! axes that matter are the [`CoreVersion`] (the released spec) and the
//! [`Transport`] (BR/EDR vs LE), bundled together as a [`SpecContext`].

use core::fmt;

/// A released version of the Bluetooth Core Specification.
///
/// The discriminant order is chronological, so `<`/`>` comparisons mean
/// "older/newer". Each variant maps to the HCI `Version` byte reported by a
/// controller in `Read_Local_Version_Information` — see [`CoreVersion::hci`] and
/// [`CoreVersion::from_hci`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum CoreVersion {
    /// Bluetooth 1.0b (HCI 0).
    V1_0b,
    /// Bluetooth 1.1 (HCI 1).
    V1_1,
    /// Bluetooth 1.2 (HCI 2).
    V1_2,
    /// Bluetooth 2.0 + EDR (HCI 3).
    V2_0,
    /// Bluetooth 2.1 + EDR (HCI 4) — introduced Secure Simple Pairing.
    V2_1,
    /// Bluetooth 3.0 + HS (HCI 5).
    V3_0,
    /// Bluetooth 4.0 (HCI 6) — introduced Bluetooth Low Energy.
    V4_0,
    /// Bluetooth 4.1 (HCI 7).
    V4_1,
    /// Bluetooth 4.2 (HCI 8) — LE Secure Connections, longer packets.
    V4_2,
    /// Bluetooth 5.0 (HCI 9) — 2M PHY, Coded PHY (long range), advertising ext.
    V5_0,
    /// Bluetooth 5.1 (HCI 10) — direction finding.
    V5_1,
    /// Bluetooth 5.2 (HCI 11) — LE Audio / Isochronous Channels.
    V5_2,
    /// Bluetooth 5.3 (HCI 12).
    V5_3,
    /// Bluetooth 5.4 (HCI 13) — PAwR / encrypted advertising data.
    V5_4,
    /// Bluetooth 6.0 (HCI 14) — channel sounding.
    V6_0,
}

impl CoreVersion {
    /// All known versions, oldest first. Handy for tables and tests.
    pub const ALL: [CoreVersion; 15] = [
        CoreVersion::V1_0b,
        CoreVersion::V1_1,
        CoreVersion::V1_2,
        CoreVersion::V2_0,
        CoreVersion::V2_1,
        CoreVersion::V3_0,
        CoreVersion::V4_0,
        CoreVersion::V4_1,
        CoreVersion::V4_2,
        CoreVersion::V5_0,
        CoreVersion::V5_1,
        CoreVersion::V5_2,
        CoreVersion::V5_3,
        CoreVersion::V5_4,
        CoreVersion::V6_0,
    ];

    /// The HCI `Version` byte a controller reports for this release.
    ///
    /// ```
    /// # use pax_core::CoreVersion;
    /// assert_eq!(CoreVersion::V5_0.hci(), 9);
    /// ```
    pub const fn hci(self) -> u8 {
        match self {
            CoreVersion::V1_0b => 0,
            CoreVersion::V1_1 => 1,
            CoreVersion::V1_2 => 2,
            CoreVersion::V2_0 => 3,
            CoreVersion::V2_1 => 4,
            CoreVersion::V3_0 => 5,
            CoreVersion::V4_0 => 6,
            CoreVersion::V4_1 => 7,
            CoreVersion::V4_2 => 8,
            CoreVersion::V5_0 => 9,
            CoreVersion::V5_1 => 10,
            CoreVersion::V5_2 => 11,
            CoreVersion::V5_3 => 12,
            CoreVersion::V5_4 => 13,
            CoreVersion::V6_0 => 14,
        }
    }

    /// Map an HCI `Version` byte back to a [`CoreVersion`], or `None` if the byte
    /// is not an assigned value.
    ///
    /// ```
    /// # use pax_core::CoreVersion;
    /// assert_eq!(CoreVersion::from_hci(4), Some(CoreVersion::V2_1));
    /// assert_eq!(CoreVersion::from_hci(200), None);
    /// ```
    pub const fn from_hci(byte: u8) -> Option<CoreVersion> {
        Some(match byte {
            0 => CoreVersion::V1_0b,
            1 => CoreVersion::V1_1,
            2 => CoreVersion::V1_2,
            3 => CoreVersion::V2_0,
            4 => CoreVersion::V2_1,
            5 => CoreVersion::V3_0,
            6 => CoreVersion::V4_0,
            7 => CoreVersion::V4_1,
            8 => CoreVersion::V4_2,
            9 => CoreVersion::V5_0,
            10 => CoreVersion::V5_1,
            11 => CoreVersion::V5_2,
            12 => CoreVersion::V5_3,
            13 => CoreVersion::V5_4,
            14 => CoreVersion::V6_0,
            _ => return None,
        })
    }

    /// The marketing name, e.g. `"Bluetooth 5.0"`.
    pub const fn name(self) -> &'static str {
        match self {
            CoreVersion::V1_0b => "Bluetooth 1.0b",
            CoreVersion::V1_1 => "Bluetooth 1.1",
            CoreVersion::V1_2 => "Bluetooth 1.2",
            CoreVersion::V2_0 => "Bluetooth 2.0 + EDR",
            CoreVersion::V2_1 => "Bluetooth 2.1 + EDR",
            CoreVersion::V3_0 => "Bluetooth 3.0 + HS",
            CoreVersion::V4_0 => "Bluetooth 4.0",
            CoreVersion::V4_1 => "Bluetooth 4.1",
            CoreVersion::V4_2 => "Bluetooth 4.2",
            CoreVersion::V5_0 => "Bluetooth 5.0",
            CoreVersion::V5_1 => "Bluetooth 5.1",
            CoreVersion::V5_2 => "Bluetooth 5.2",
            CoreVersion::V5_3 => "Bluetooth 5.3",
            CoreVersion::V5_4 => "Bluetooth 5.4",
            CoreVersion::V6_0 => "Bluetooth 6.0",
        }
    }

    /// `true` if this release defines Bluetooth Low Energy (4.0 and later).
    pub const fn supports_le(self) -> bool {
        (self as u8) >= (CoreVersion::V4_0 as u8)
    }

    /// `true` if this release supports Classic BR/EDR. (Every release does; LE-only
    /// controllers are described via [`Transport`], not the core version.)
    pub const fn supports_bredr(self) -> bool {
        true
    }
}

impl fmt::Display for CoreVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which physical transport a link uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Transport {
    /// Classic Basic Rate / Enhanced Data Rate (used by OBEX file transfer, A2DP).
    #[default]
    BrEdr,
    /// Bluetooth Low Energy (GATT, advertising).
    Le,
    /// A dual-mode endpoint that supports both.
    Dual,
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Transport::BrEdr => "BR/EDR",
            Transport::Le => "LE",
            Transport::Dual => "dual-mode",
        };
        f.write_str(s)
    }
}

/// The full specification context of a link: which release, over which transport.
///
/// This is the value diagnostics group by when splitting "by specification of
/// Bluetooth".
///
/// ```
/// use pax_core::{SpecContext, CoreVersion, Transport};
/// let ctx = SpecContext::new(CoreVersion::V5_2, Transport::Le);
/// assert_eq!(ctx.to_string(), "Bluetooth 5.2 over LE");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpecContext {
    /// The negotiated core version.
    pub version: CoreVersion,
    /// The physical transport in use.
    pub transport: Transport,
}

impl SpecContext {
    /// Construct a spec context.
    pub const fn new(version: CoreVersion, transport: Transport) -> Self {
        SpecContext { version, transport }
    }
}

impl fmt::Display for SpecContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} over {}", self.version, self.transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hci_roundtrips_for_all_versions() {
        for v in CoreVersion::ALL {
            assert_eq!(CoreVersion::from_hci(v.hci()), Some(v), "roundtrip {v}");
        }
    }

    #[test]
    fn le_support_boundary_is_4_0() {
        assert!(!CoreVersion::V2_1.supports_le());
        assert!(CoreVersion::V4_0.supports_le());
        assert!(CoreVersion::V5_4.supports_le());
    }

    #[test]
    fn ordering_is_chronological() {
        assert!(CoreVersion::V4_2 < CoreVersion::V5_0);
        assert!(CoreVersion::V1_0b < CoreVersion::V6_0);
    }
}
