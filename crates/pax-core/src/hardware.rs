//! The *model of hardware providing the Bluetooth features*.
//!
//! One of the explicit requirements of the toolkit is to debug connections
//! *split by the model of the controller hardware*. That makes the controller
//! model a first-class, hashable value: diagnostics group an event stream by
//! [`ControllerModel`] to compare, say, an Apple combo chip against a USB CSR
//! dongle on the same machine.

use core::fmt;

/// A Bluetooth SIG–assigned *Company Identifier* (16-bit).
///
/// These appear in HCI `Read_Local_Version_Information`, in LE advertising
/// "manufacturer specific data", and in the upper bytes of some addresses. Only a
/// small, commonly-encountered subset is named here via [`CompanyId::name`]; the
/// rest stringify as their hex value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CompanyId(pub u16);

impl CompanyId {
    /// Ericsson Technology Licensing — company id `0x0000`.
    pub const ERICSSON: CompanyId = CompanyId(0x0000);
    /// Intel Corp. — company id `0x0002`.
    pub const INTEL: CompanyId = CompanyId(0x0002);
    /// Broadcom Corporation — company id `0x000F`.
    pub const BROADCOM: CompanyId = CompanyId(0x000F);
    /// Qualcomm — company id `0x001D`.
    pub const QUALCOMM: CompanyId = CompanyId(0x001D);
    /// Cypress Semiconductor — company id `0x0131`.
    pub const CYPRESS: CompanyId = CompanyId(0x0131);
    /// Realtek Semiconductor — company id `0x005D`.
    pub const REALTEK: CompanyId = CompanyId(0x005D);
    /// Nordic Semiconductor ASA — company id `0x0059`.
    pub const NORDIC: CompanyId = CompanyId(0x0059);
    /// Apple, Inc. — company id `0x004C`.
    pub const APPLE: CompanyId = CompanyId(0x004C);
    /// Cambridge Silicon Radio (CSR) — company id `0x000A`.
    pub const CSR: CompanyId = CompanyId(0x000A);

    /// The human-readable vendor name, if this id is in the curated table.
    ///
    /// ```
    /// # use pax_core::CompanyId;
    /// assert_eq!(CompanyId::APPLE.name(), Some("Apple, Inc."));
    /// assert_eq!(CompanyId(0xFFFF).name(), None);
    /// ```
    pub const fn name(self) -> Option<&'static str> {
        Some(match self.0 {
            0x0000 => "Ericsson Technology Licensing",
            0x0002 => "Intel Corp.",
            0x000A => "Cambridge Silicon Radio",
            0x000F => "Broadcom Corporation",
            0x001D => "Qualcomm",
            0x004C => "Apple, Inc.",
            0x0059 => "Nordic Semiconductor ASA",
            0x005D => "Realtek Semiconductor",
            0x0131 => "Cypress Semiconductor",
            _ => return None,
        })
    }
}

impl fmt::Debug for CompanyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(n) => write!(f, "CompanyId(0x{:04X} {n})", self.0),
            None => write!(f, "CompanyId(0x{:04X})", self.0),
        }
    }
}

impl fmt::Display for CompanyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(n) => f.write_str(n),
            None => write!(f, "0x{:04X}", self.0),
        }
    }
}

/// A broad family of Bluetooth controller silicon.
///
/// This is intentionally coarse — it is the bucket diagnostics use when the exact
/// model string is noisy but the vendor lineage is what matters for behavior
/// (firmware quirks, supported features, coexistence behavior).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum ChipsetFamily {
    /// Apple-designed combo Wi-Fi/Bluetooth silicon (e.g. on Apple Silicon Macs).
    AppleCombo,
    /// Broadcom/Cypress `BCM`-series controllers.
    Broadcom,
    /// Intel `AX`/`Wireless-AC` combo controllers.
    Intel,
    /// Qualcomm/Atheros controllers.
    Qualcomm,
    /// Realtek `RTL`-series controllers.
    Realtek,
    /// Nordic `nRF`-series (common on dev kits and peripherals).
    Nordic,
    /// Cambridge Silicon Radio / classic CSR dongles.
    Csr,
    /// A virtual or emulated controller (used by the mock backend).
    Virtual,
    /// Unknown or unclassified silicon.
    #[default]
    Unknown,
}

impl ChipsetFamily {
    /// Best-effort silicon family for a vendor [`CompanyId`].
    ///
    /// Used when auto-detecting the local controller (e.g. from BlueZ's modalias
    /// or an HCI `Read_Local_Version_Information` manufacturer field). Unknown
    /// vendors map to [`ChipsetFamily::Unknown`].
    ///
    /// ```
    /// use pax_core::{ChipsetFamily, CompanyId};
    /// assert_eq!(ChipsetFamily::from_company(CompanyId::INTEL), ChipsetFamily::Intel);
    /// assert_eq!(ChipsetFamily::from_company(CompanyId::APPLE), ChipsetFamily::AppleCombo);
    /// assert_eq!(ChipsetFamily::from_company(CompanyId(0xABCD)), ChipsetFamily::Unknown);
    /// ```
    pub fn from_company(company: CompanyId) -> ChipsetFamily {
        match company {
            CompanyId::APPLE => ChipsetFamily::AppleCombo,
            CompanyId::INTEL => ChipsetFamily::Intel,
            CompanyId::BROADCOM => ChipsetFamily::Broadcom,
            CompanyId::CYPRESS => ChipsetFamily::Broadcom,
            CompanyId::QUALCOMM => ChipsetFamily::Qualcomm,
            CompanyId::REALTEK => ChipsetFamily::Realtek,
            CompanyId::NORDIC => ChipsetFamily::Nordic,
            CompanyId::CSR => ChipsetFamily::Csr,
            _ => ChipsetFamily::Unknown,
        }
    }
}

impl fmt::Display for ChipsetFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ChipsetFamily::AppleCombo => "Apple combo",
            ChipsetFamily::Broadcom => "Broadcom",
            ChipsetFamily::Intel => "Intel",
            ChipsetFamily::Qualcomm => "Qualcomm",
            ChipsetFamily::Realtek => "Realtek",
            ChipsetFamily::Nordic => "Nordic",
            ChipsetFamily::Csr => "CSR",
            ChipsetFamily::Virtual => "virtual",
            ChipsetFamily::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

/// A concrete identification of the local Bluetooth controller.
///
/// Construct one with [`ControllerModel::new`] or the convenience
/// [`ControllerModel::virtual_model`]. Equality and hashing are by all fields, so
/// it can be used directly as a grouping key in diagnostics.
///
/// ```
/// use pax_core::{ControllerModel, ChipsetFamily, CompanyId};
///
/// let m = ControllerModel::new(CompanyId::APPLE, ChipsetFamily::AppleCombo, "BCM_4388");
/// assert_eq!(m.label(), "Apple, Inc. BCM_4388 (Apple combo)");
/// ```
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ControllerModel {
    /// The vendor that manufactured the controller.
    pub manufacturer: CompanyId,
    /// The broad silicon family.
    pub family: ChipsetFamily,
    /// A specific model string (e.g. `"BCM4378"`, `"AX210"`, `"RTL8761B"`).
    pub model: String,
}

impl ControllerModel {
    /// Construct a fully-specified controller model.
    pub fn new(manufacturer: CompanyId, family: ChipsetFamily, model: impl Into<String>) -> Self {
        ControllerModel {
            manufacturer,
            family,
            model: model.into(),
        }
    }

    /// A virtual controller, used by the mock backend and for examples.
    ///
    /// ```
    /// # use pax_core::ControllerModel;
    /// let v = ControllerModel::virtual_model("pax-mock-0");
    /// assert_eq!(v.family.to_string(), "virtual");
    /// ```
    pub fn virtual_model(model: impl Into<String>) -> Self {
        ControllerModel::new(CompanyId(0xFFFF), ChipsetFamily::Virtual, model)
    }

    /// A compact, human-friendly one-line label, e.g.
    /// `"Intel Corp. AX210 (Intel)"`.
    pub fn label(&self) -> String {
        format!("{} {} ({})", self.manufacturer, self.model, self.family)
    }
}

impl fmt::Debug for ControllerModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ControllerModel({})", self.label())
    }
}

impl fmt::Display for ControllerModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_company_names_resolve() {
        assert_eq!(CompanyId::INTEL.name(), Some("Intel Corp."));
        assert_eq!(CompanyId::BROADCOM.to_string(), "Broadcom Corporation");
        assert_eq!(CompanyId(0x1234).to_string(), "0x1234");
    }

    #[test]
    fn model_label_is_stable() {
        let m = ControllerModel::new(CompanyId::REALTEK, ChipsetFamily::Realtek, "RTL8761B");
        assert_eq!(m.label(), "Realtek Semiconductor RTL8761B (Realtek)");
    }

    #[test]
    fn models_are_usable_as_map_keys() {
        use std::collections::HashMap;
        let mut counts: HashMap<ControllerModel, u32> = HashMap::new();
        let a = ControllerModel::virtual_model("mock-0");
        *counts.entry(a.clone()).or_default() += 1;
        *counts.entry(a).or_default() += 1;
        assert_eq!(counts.len(), 1);
    }
}
