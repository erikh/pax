//! A small, dependency-free Bluetooth UUID type.
//!
//! GATT services and characteristics are identified by 128-bit UUIDs, usually
//! written in the canonical `8-4-4-4-12` hex form. Bluetooth SIG also assigns
//! 16- and 32-bit *short* UUIDs, which expand into the 128-bit space via the
//! Bluetooth Base UUID `0000xxxx-0000-1000-8000-00805F9B34FB`.
//!
//! This type avoids pulling the `uuid` crate into the otherwise dependency-light
//! `pax-core`; the real backends convert to/from their own UUID types at the edge.

use core::fmt;
use core::str::FromStr;

use crate::error::Error;

/// The Bluetooth Base UUID, into which 16-/32-bit short UUIDs expand.
const BASE: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B, 0x34, 0xFB,
];

/// A 128-bit UUID (big-endian byte order, i.e. the order it prints in).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// Construct from 16 big-endian bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Uuid(bytes)
    }

    /// The 16 big-endian bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Expand a 16-bit Bluetooth SIG short UUID into the Base UUID.
    ///
    /// ```
    /// use pax_core::Uuid;
    /// // 0x180A == Device Information service.
    /// assert_eq!(Uuid::from_u16(0x180A).to_string(), "0000180a-0000-1000-8000-00805f9b34fb");
    /// ```
    pub const fn from_u16(short: u16) -> Self {
        Uuid::from_u32(short as u32)
    }

    /// Expand a 32-bit Bluetooth SIG short UUID into the Base UUID.
    pub const fn from_u32(short: u32) -> Self {
        let mut b = BASE;
        b[0] = (short >> 24) as u8;
        b[1] = (short >> 16) as u8;
        b[2] = (short >> 8) as u8;
        b[3] = short as u8;
        Uuid(b)
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
            b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
        )
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Uuid({self})")
    }
}

impl FromStr for Uuid {
    type Err = Error;

    /// Parse the canonical hyphenated 128-bit form, a bare 32-hex-digit form, or a
    /// 16-/32-bit short hex form (`"180a"`, `"0000180a"`), which expand via the
    /// Bluetooth Base UUID.
    ///
    /// ```
    /// use pax_core::Uuid;
    /// let full: Uuid = "0000180a-0000-1000-8000-00805f9b34fb".parse().unwrap();
    /// assert_eq!("180a".parse::<Uuid>().unwrap(), full);   // short form expands
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || Error::invalid("UUID", format!("cannot parse `{s}`"));
        let stripped: String = s.chars().filter(|c| *c != '-').collect();
        match stripped.len() {
            4 => u16::from_str_radix(&stripped, 16)
                .map(Uuid::from_u16)
                .map_err(|_| bad()),
            8 => u32::from_str_radix(&stripped, 16)
                .map(Uuid::from_u32)
                .map_err(|_| bad()),
            32 => {
                let mut bytes = [0u8; 16];
                for (i, byte) in bytes.iter_mut().enumerate() {
                    *byte =
                        u8::from_str_radix(&stripped[i * 2..i * 2 + 2], 16).map_err(|_| bad())?;
                }
                Ok(Uuid(bytes))
            }
            _ => Err(bad()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_uuid_expands_via_base() {
        let dis = Uuid::from_u16(0x180A);
        assert_eq!(dis.to_string(), "0000180a-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn roundtrip_full_form() {
        let s = "12345678-9abc-def0-1234-56789abcdef0";
        let u: Uuid = s.parse().unwrap();
        assert_eq!(u.to_string(), s);
    }

    #[test]
    fn short_and_full_are_equal() {
        assert_eq!(
            "180a".parse::<Uuid>().unwrap(),
            "0000180a-0000-1000-8000-00805f9b34fb"
                .parse::<Uuid>()
                .unwrap()
        );
        assert_eq!("0000180A".parse::<Uuid>().unwrap(), Uuid::from_u16(0x180A));
    }

    #[test]
    fn rejects_garbage() {
        assert!("nope".parse::<Uuid>().is_err());
        assert!("12345".parse::<Uuid>().is_err()); // 5 hex digits
        assert!("zzzz".parse::<Uuid>().is_err());
    }
}
