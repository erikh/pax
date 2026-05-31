//! The IEEE 802 taxonomy — the "802.1x" diagnostic split dimension, generalized.
//!
//! The toolkit debugs connections split not only by Bluetooth spec but by the
//! relevant **IEEE 802 standard**. This module models that as a small family so
//! diagnostics can group an event stream either by the *radio* lineage
//! (802.15.x, which Bluetooth's lower layers derive from) or by *network access
//! control* (802.1X, which governs a Bluetooth PAN once it is bridged onto a
//! LAN). Both axes are captured together in a [`StandardsProfile`].

use core::fmt;

/// A specific IEEE 802 standard relevant to a Bluetooth deployment.
///
/// The variants span the parts of the 802 family that actually touch a Bluetooth
/// link: the WPAN radio standards it derives from, the WLAN it shares a radio
/// with on combo silicon, and the port-based access control applied when a PAN
/// bridges to Ethernet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Standard802 {
    /// **802.15.1** — WPAN. The MAC/PHY the original Bluetooth BR/EDR derived
    /// from. This is the default radio-lineage standard for a Classic link.
    Wpan80215_1,
    /// **802.15.4** — low-rate WPAN (the basis for Zigbee/Thread). Bluetooth
    /// does not use it, but it shares the 2.4 GHz band and is relevant to
    /// coexistence diagnostics on multi-radio hardware.
    Wpan80215_4,
    /// **802.11** — WLAN. Relevant for coexistence on combo Wi-Fi/Bluetooth
    /// silicon, where Wi-Fi traffic can starve a Bluetooth link.
    Wlan80211,
    /// **802.1X** — port-based network access control (EAP). Relevant when a
    /// Bluetooth PAN profile (PANU/NAP) bridges onto an 802 LAN and the bridge
    /// port is guarded by an authenticator.
    PortAuth8021X,
}

impl Standard802 {
    /// The dotted designation, e.g. `"802.15.1"` or `"802.1X"`.
    ///
    /// ```
    /// # use pax_core::Standard802;
    /// assert_eq!(Standard802::Wpan80215_1.dotted(), "802.15.1");
    /// assert_eq!(Standard802::PortAuth8021X.dotted(), "802.1X");
    /// ```
    pub const fn dotted(self) -> &'static str {
        match self {
            Standard802::Wpan80215_1 => "802.15.1",
            Standard802::Wpan80215_4 => "802.15.4",
            Standard802::Wlan80211 => "802.11",
            Standard802::PortAuth8021X => "802.1X",
        }
    }

    /// A short human title.
    pub const fn title(self) -> &'static str {
        match self {
            Standard802::Wpan80215_1 => "WPAN (Bluetooth lineage)",
            Standard802::Wpan80215_4 => "Low-Rate WPAN",
            Standard802::Wlan80211 => "Wireless LAN",
            Standard802::PortAuth8021X => "Port-Based Network Access Control",
        }
    }

    /// Which conceptual [`StandardsLayer`] this standard governs.
    pub const fn layer(self) -> StandardsLayer {
        match self {
            Standard802::Wpan80215_1 => StandardsLayer::Radio,
            Standard802::Wpan80215_4 => StandardsLayer::Coexistence,
            Standard802::Wlan80211 => StandardsLayer::Coexistence,
            Standard802::PortAuth8021X => StandardsLayer::NetworkAccess,
        }
    }
}

impl fmt::Display for Standard802 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IEEE {} ({})", self.dotted(), self.title())
    }
}

/// The conceptual layer an IEEE 802 standard governs. This is the *coarse* split
/// dimension diagnostics offer when you do not care about the exact standard, only
/// whether an issue is in the radio, the MAC, network access, or cross-radio
/// coexistence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StandardsLayer {
    /// Physical radio / PHY concerns.
    Radio,
    /// Medium-access control concerns.
    Mac,
    /// Network admission / authentication concerns (802.1X / EAP).
    NetworkAccess,
    /// Cross-technology coexistence on shared spectrum.
    Coexistence,
}

impl fmt::Display for StandardsLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            StandardsLayer::Radio => "radio/PHY",
            StandardsLayer::Mac => "MAC",
            StandardsLayer::NetworkAccess => "network access",
            StandardsLayer::Coexistence => "coexistence",
        };
        f.write_str(s)
    }
}

/// The 802.1X / EAP authentication state of a PAN bridge port.
///
/// For the common case — plain pairing and file push over BR/EDR — this is
/// [`PortAuthState::NotApplicable`]. It only becomes meaningful when the link is
/// carrying PAN traffic onto an authenticated network port.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PortAuthState {
    /// No 802.1X authenticator is in play (the default for file transfer).
    #[default]
    NotApplicable,
    /// The port is up but the supplicant has not authenticated.
    Unauthenticated,
    /// An EAP exchange is in progress.
    Authenticating,
    /// The supplicant authenticated successfully; the port is authorized.
    Authenticated,
    /// Authentication failed; the port is held unauthorized.
    Failed,
}

impl fmt::Display for PortAuthState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PortAuthState::NotApplicable => "n/a",
            PortAuthState::Unauthenticated => "unauthenticated",
            PortAuthState::Authenticating => "authenticating",
            PortAuthState::Authenticated => "authenticated",
            PortAuthState::Failed => "auth-failed",
        };
        f.write_str(s)
    }
}

/// The complete standards context attached to a link: its radio-lineage standard
/// and, if applicable, its 802.1X port-auth state.
///
/// Diagnostics can split by either field independently. For a plain file push you
/// will see `{ radio: 802.15.1, port_auth: n/a }`; for a PAN bridged onto a
/// guarded switch port you might see `{ radio: 802.15.1, port_auth: authenticated }`.
///
/// ```
/// use pax_core::{StandardsProfile, Standard802, PortAuthState};
/// let p = StandardsProfile::bredr_default();
/// assert_eq!(p.radio, Standard802::Wpan80215_1);
/// assert_eq!(p.port_auth, PortAuthState::NotApplicable);
///
/// let bridged = StandardsProfile::bredr_default().with_port_auth(PortAuthState::Authenticated);
/// assert_eq!(bridged.port_auth, PortAuthState::Authenticated);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StandardsProfile {
    /// The radio-lineage standard for the link (usually 802.15.1 for Classic).
    pub radio: Standard802,
    /// The 802.1X port-auth state, or [`PortAuthState::NotApplicable`].
    pub port_auth: PortAuthState,
}

impl StandardsProfile {
    /// The default profile for a Classic BR/EDR link: 802.15.1 radio, no port auth.
    pub const fn bredr_default() -> Self {
        StandardsProfile {
            radio: Standard802::Wpan80215_1,
            port_auth: PortAuthState::NotApplicable,
        }
    }

    /// Builder-style override of the radio standard.
    pub const fn with_radio(mut self, radio: Standard802) -> Self {
        self.radio = radio;
        self
    }

    /// Builder-style override of the 802.1X port-auth state.
    pub const fn with_port_auth(mut self, state: PortAuthState) -> Self {
        self.port_auth = state;
        self
    }
}

impl Default for StandardsProfile {
    fn default() -> Self {
        StandardsProfile::bredr_default()
    }
}

impl fmt::Display for StandardsProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} / 802.1X:{}", self.radio.dotted(), self.port_auth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_designations() {
        assert_eq!(Standard802::Wpan80215_4.dotted(), "802.15.4");
        assert_eq!(Standard802::Wlan80211.dotted(), "802.11");
    }

    #[test]
    fn layer_mapping() {
        assert_eq!(
            Standard802::PortAuth8021X.layer(),
            StandardsLayer::NetworkAccess
        );
        assert_eq!(Standard802::Wpan80215_1.layer(), StandardsLayer::Radio);
    }

    #[test]
    fn builder_overrides() {
        let p = StandardsProfile::bredr_default()
            .with_radio(Standard802::Wlan80211)
            .with_port_auth(PortAuthState::Failed);
        assert_eq!(p.radio, Standard802::Wlan80211);
        assert_eq!(p.port_auth, PortAuthState::Failed);
    }
}
