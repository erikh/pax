//! Bluetooth device addresses (`BD_ADDR`).

use core::fmt;
use core::str::FromStr;

use crate::error::Error;

/// A 48-bit Bluetooth device address, often written `AA:BB:CC:DD:EE:FF`.
///
/// Stored as six octets in *transmission order* (most-significant first, i.e. the
/// same order they appear in the colon-separated text form). The top three octets
/// are the OUI (Organizationally Unique Identifier) assigned to the silicon
/// vendor; see [`BdAddr::oui`].
///
/// # Parsing
///
/// Both colon- and dash-separated forms are accepted, case-insensitively:
///
/// ```
/// use pax_core::BdAddr;
/// let a: BdAddr = "00:1A:7D:DA:71:13".parse().unwrap();
/// let b: BdAddr = "00-1a-7d-da-71-13".parse().unwrap();
/// assert_eq!(a, b);
/// assert_eq!(a.to_string(), "00:1A:7D:DA:71:13"); // canonical: upper, colons
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BdAddr([u8; 6]);

impl BdAddr {
    /// The all-zero address (`00:00:00:00:00:00`), used as a "none" sentinel.
    pub const NIL: BdAddr = BdAddr([0; 6]);

    /// Construct from six octets in transmission order.
    pub const fn new(octets: [u8; 6]) -> Self {
        BdAddr(octets)
    }

    /// The six octets in transmission order.
    pub const fn octets(self) -> [u8; 6] {
        self.0
    }

    /// The OUI (top three octets), identifying the silicon vendor.
    ///
    /// ```
    /// # use pax_core::BdAddr;
    /// let a: BdAddr = "00:1A:7D:DA:71:13".parse().unwrap();
    /// assert_eq!(a.oui(), [0x00, 0x1A, 0x7D]);
    /// ```
    pub const fn oui(self) -> [u8; 3] {
        [self.0[0], self.0[1], self.0[2]]
    }

    /// `true` if this is the all-zero [`BdAddr::NIL`] address.
    pub fn is_nil(self) -> bool {
        self.0 == [0; 6]
    }

    /// `true` if the locally-administered bit is set in the most-significant
    /// octet. For Bluetooth LE this distinguishes random/private addresses from
    /// globally-unique public ones.
    pub const fn is_locally_administered(self) -> bool {
        self.0[0] & 0b0000_0010 != 0
    }
}

impl fmt::Display for BdAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let o = self.0;
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            o[0], o[1], o[2], o[3], o[4], o[5]
        )
    }
}

impl fmt::Debug for BdAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BdAddr({self})")
    }
}

impl FromStr for BdAddr {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let sep = if s.contains(':') {
            ':'
        } else if s.contains('-') {
            '-'
        } else {
            return Err(Error::InvalidAddress(s.to_string()));
        };

        let mut octets = [0u8; 6];
        let mut count = 0usize;
        for part in s.split(sep) {
            if count >= 6 {
                return Err(Error::InvalidAddress(s.to_string()));
            }
            let byte = u8::from_str_radix(part.trim(), 16)
                .map_err(|_| Error::InvalidAddress(s.to_string()))?;
            // Reject inputs like "0:1:..." that are not two hex digits wide; this
            // keeps the canonical form unambiguous.
            if part.trim().len() != 2 {
                return Err(Error::InvalidAddress(s.to_string()));
            }
            octets[count] = byte;
            count += 1;
        }
        if count != 6 {
            return Err(Error::InvalidAddress(s.to_string()));
        }
        Ok(BdAddr(octets))
    }
}

impl From<[u8; 6]> for BdAddr {
    fn from(octets: [u8; 6]) -> Self {
        BdAddr(octets)
    }
}

/// How a [`BdAddr`] should be interpreted on the air.
///
/// Classic (BR/EDR) devices always use a public address. Bluetooth LE devices may
/// use a random/private address that rotates over time for privacy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AddressType {
    /// A globally-unique, vendor-assigned public address (the default).
    #[default]
    Public,
    /// A random address (static, or resolvable/non-resolvable private), used by
    /// Bluetooth LE for privacy.
    Random,
}

impl fmt::Display for AddressType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AddressType::Public => f.write_str("public"),
            AddressType::Random => f.write_str("random"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colon_and_dash_forms_equal() {
        let a: BdAddr = "AA:BB:CC:DD:EE:FF".parse().unwrap();
        let b: BdAddr = "aa-bb-cc-dd-ee-ff".parse().unwrap();
        assert_eq!(a, b);
        assert_eq!(a.octets(), [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn rejects_malformed() {
        assert!("not-an-address".parse::<BdAddr>().is_err());
        assert!("00:1A:7D:DA:71".parse::<BdAddr>().is_err()); // too few
        assert!("00:1A:7D:DA:71:13:99".parse::<BdAddr>().is_err()); // too many
        assert!("0:1A:7D:DA:71:13".parse::<BdAddr>().is_err()); // not 2-wide
        assert!("ZZ:1A:7D:DA:71:13".parse::<BdAddr>().is_err()); // non-hex
    }

    #[test]
    fn locally_administered_bit() {
        // 0x02 in the top octet sets the locally-administered bit.
        let random: BdAddr = "02:00:00:00:00:01".parse().unwrap();
        assert!(random.is_locally_administered());
        let public: BdAddr = "00:1A:7D:DA:71:13".parse().unwrap();
        assert!(!public.is_locally_administered());
    }

    #[test]
    fn roundtrips_through_display() {
        let s = "00:1A:7D:DA:71:13";
        assert_eq!(s.parse::<BdAddr>().unwrap().to_string(), s);
    }
}
