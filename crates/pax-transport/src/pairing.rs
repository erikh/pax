//! Pairing primitives: the agent callback interface and the outcome type.
//!
//! Pairing on modern Bluetooth (Secure Simple Pairing) may need a decision from a
//! human or a policy: confirm a "just works" bond, compare a 6-digit number, type
//! a PIN. A backend cannot make that decision itself, so it delegates to a
//! [`PairingAgent`]. `pax-transport` defines the interface; `pax-pairing` ships
//! ready-made agents (accept-all, fixed-PIN, numeric-comparison, …) and the
//! higher-level retrying workflow.

use async_trait::async_trait;
use pax_core::PairingMethodHint;

/// A question the backend asks the agent during pairing.
///
/// Each variant corresponds to one Secure Simple Pairing association model.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PairingRequest {
    /// "Just Works": confirm bonding a device with no MITM protection.
    ConfirmJustWorks,
    /// Numeric comparison: confirm that this 6-digit passkey matches the one the
    /// peer is showing.
    ConfirmPasskey {
        /// The passkey both sides should be displaying.
        passkey: u32,
    },
    /// The peer wants a legacy PIN; the agent must supply it.
    RequestPinCode,
    /// The peer wants a passkey typed in; the agent must supply it.
    RequestPasskey,
    /// Display this passkey to the user (no response value needed beyond
    /// acknowledging).
    DisplayPasskey {
        /// The passkey to show.
        passkey: u32,
    },
}

impl PairingRequest {
    /// The [`PairingMethodHint`] this request corresponds to, for event labeling.
    pub fn method(&self) -> PairingMethodHint {
        match self {
            PairingRequest::ConfirmJustWorks => PairingMethodHint::JustWorks,
            PairingRequest::ConfirmPasskey { .. } => PairingMethodHint::NumericComparison,
            PairingRequest::RequestPinCode => PairingMethodHint::PinCode,
            PairingRequest::RequestPasskey => PairingMethodHint::PasskeyEntry,
            PairingRequest::DisplayPasskey { .. } => PairingMethodHint::PasskeyDisplay,
        }
    }
}

/// The agent's answer to a [`PairingRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PairingResponse {
    /// Confirm (or, with `false`, reject) a yes/no request.
    Confirm(bool),
    /// Provide a PIN string.
    Pin(String),
    /// Provide a numeric passkey.
    Passkey(u32),
    /// Acknowledge a display-only request.
    Acknowledged,
    /// Abort the whole pairing.
    Cancel,
}

/// Where a decision came from. Lets a backend tell "the user said no" apart from
/// "the policy said no", which matters for whether a retry makes sense.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// A human made the choice interactively.
    Human,
    /// A non-interactive policy made the choice.
    Policy,
}

/// The interface a backend calls to resolve pairing prompts.
///
/// Implementations must be `Send + Sync` because a backend may invoke them from
/// any task. They are async so an interactive agent can await terminal/GUI input.
///
/// ```
/// use async_trait::async_trait;
/// use pax_transport::pairing::{PairingAgent, PairingRequest, PairingResponse};
///
/// /// An agent that confirms everything (handy for tests; insecure in production).
/// struct AcceptEverything;
///
/// #[async_trait]
/// impl PairingAgent for AcceptEverything {
///     async fn respond(&self, request: PairingRequest) -> PairingResponse {
///         match request {
///             PairingRequest::ConfirmJustWorks
///             | PairingRequest::ConfirmPasskey { .. } => PairingResponse::Confirm(true),
///             PairingRequest::RequestPinCode => PairingResponse::Pin("0000".into()),
///             PairingRequest::RequestPasskey => PairingResponse::Passkey(0),
///             PairingRequest::DisplayPasskey { .. } => PairingResponse::Acknowledged,
///             // `PairingRequest` is `#[non_exhaustive]`: handle future variants.
///             _ => PairingResponse::Cancel,
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait PairingAgent: Send + Sync {
    /// Resolve one pairing prompt.
    async fn respond(&self, request: PairingRequest) -> PairingResponse;
}

/// The result of a pairing attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingOutcome {
    /// Whether the device is now paired.
    pub paired: bool,
    /// Whether a long-term bond (persisted keys) was established.
    pub bonded: bool,
    /// The association model that was used.
    pub method: PairingMethodHint,
}

impl PairingOutcome {
    /// A successful bond via `method`.
    pub fn bonded(method: PairingMethodHint) -> Self {
        PairingOutcome {
            paired: true,
            bonded: true,
            method,
        }
    }
}
