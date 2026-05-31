//! Ready-made [`PairingAgent`] implementations.
//!
//! A [`PairingAgent`] decides how to answer the prompts a backend raises during
//! Secure Simple Pairing. These cover the common policies; for anything bespoke,
//! implement the trait yourself or use [`CallbackAgent`].

use async_trait::async_trait;

use pax_transport::pairing::{PairingAgent, PairingRequest, PairingResponse};

/// Accepts every prompt: confirms "just works" and numeric comparison, supplies a
/// configurable PIN/passkey.
///
/// **Insecure** — it defeats man-in-the-middle protection by confirming without a
/// human in the loop. Use it for tests, lab automation, and trusted captive
/// environments only.
#[derive(Clone, Debug)]
pub struct AcceptAllAgent {
    pin: String,
    passkey: u32,
}

impl AcceptAllAgent {
    /// An accept-all agent using PIN `"0000"` and passkey `0`.
    pub fn new() -> Self {
        AcceptAllAgent {
            pin: "0000".to_string(),
            passkey: 0,
        }
    }

    /// Set the PIN this agent supplies for legacy PIN prompts.
    pub fn with_pin(mut self, pin: impl Into<String>) -> Self {
        self.pin = pin.into();
        self
    }

    /// Set the passkey this agent supplies for passkey-entry prompts.
    pub fn with_passkey(mut self, passkey: u32) -> Self {
        self.passkey = passkey;
        self
    }
}

impl Default for AcceptAllAgent {
    fn default() -> Self {
        AcceptAllAgent::new()
    }
}

#[async_trait]
impl PairingAgent for AcceptAllAgent {
    async fn respond(&self, request: PairingRequest) -> PairingResponse {
        match request {
            PairingRequest::ConfirmJustWorks | PairingRequest::ConfirmPasskey { .. } => {
                PairingResponse::Confirm(true)
            }
            PairingRequest::RequestPinCode => PairingResponse::Pin(self.pin.clone()),
            PairingRequest::RequestPasskey => PairingResponse::Passkey(self.passkey),
            PairingRequest::DisplayPasskey { .. } => PairingResponse::Acknowledged,
            _ => PairingResponse::Cancel,
        }
    }
}

/// Rejects every prompt. Useful to assert that a workflow handles refusal, or as a
/// safe default that never bonds.
#[derive(Clone, Copy, Debug, Default)]
pub struct RejectAllAgent;

#[async_trait]
impl PairingAgent for RejectAllAgent {
    async fn respond(&self, request: PairingRequest) -> PairingResponse {
        match request {
            PairingRequest::ConfirmJustWorks | PairingRequest::ConfirmPasskey { .. } => {
                PairingResponse::Confirm(false)
            }
            _ => PairingResponse::Cancel,
        }
    }
}

/// Supplies a fixed legacy PIN and confirms simple prompts. Models the common
/// "the accessory's PIN is printed on a sticker" case.
#[derive(Clone, Debug)]
pub struct FixedPinAgent {
    pin: String,
}

impl FixedPinAgent {
    /// A fixed-PIN agent for the given PIN string (e.g. `"1234"`).
    pub fn new(pin: impl Into<String>) -> Self {
        FixedPinAgent { pin: pin.into() }
    }
}

#[async_trait]
impl PairingAgent for FixedPinAgent {
    async fn respond(&self, request: PairingRequest) -> PairingResponse {
        match request {
            PairingRequest::RequestPinCode => PairingResponse::Pin(self.pin.clone()),
            PairingRequest::ConfirmJustWorks | PairingRequest::ConfirmPasskey { .. } => {
                PairingResponse::Confirm(true)
            }
            PairingRequest::DisplayPasskey { .. } => PairingResponse::Acknowledged,
            _ => PairingResponse::Cancel,
        }
    }
}

/// Bridges a synchronous closure into a [`PairingAgent`]. Lets application code
/// supply a policy without writing a trait impl.
///
/// ```
/// use pax_pairing::agents::CallbackAgent;
/// use pax_transport::pairing::{PairingAgent, PairingRequest, PairingResponse};
///
/// // Confirm only numeric-comparison prompts whose passkey is even.
/// let agent = CallbackAgent::new(|req| match req {
///     PairingRequest::ConfirmPasskey { passkey } => PairingResponse::Confirm(passkey % 2 == 0),
///     _ => PairingResponse::Cancel,
/// });
/// # let _ : &dyn PairingAgent = &agent;
/// ```
pub struct CallbackAgent<F> {
    f: F,
}

impl<F> CallbackAgent<F>
where
    F: Fn(PairingRequest) -> PairingResponse + Send + Sync,
{
    /// Wrap a closure as an agent.
    pub fn new(f: F) -> Self {
        CallbackAgent { f }
    }
}

#[async_trait]
impl<F> PairingAgent for CallbackAgent<F>
where
    F: Fn(PairingRequest) -> PairingResponse + Send + Sync,
{
    async fn respond(&self, request: PairingRequest) -> PairingResponse {
        (self.f)(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accept_all_confirms_and_supplies_pin() {
        let agent = AcceptAllAgent::new().with_pin("1234");
        assert_eq!(
            agent.respond(PairingRequest::ConfirmJustWorks).await,
            PairingResponse::Confirm(true)
        );
        assert_eq!(
            agent.respond(PairingRequest::RequestPinCode).await,
            PairingResponse::Pin("1234".into())
        );
    }

    #[tokio::test]
    async fn reject_all_refuses() {
        let agent = RejectAllAgent;
        assert_eq!(
            agent.respond(PairingRequest::ConfirmJustWorks).await,
            PairingResponse::Confirm(false)
        );
    }

    #[tokio::test]
    async fn callback_runs_policy() {
        let agent = CallbackAgent::new(|req| match req {
            PairingRequest::ConfirmPasskey { passkey } => {
                PairingResponse::Confirm(passkey % 2 == 0)
            }
            _ => PairingResponse::Cancel,
        });
        assert_eq!(
            agent
                .respond(PairingRequest::ConfirmPasskey { passkey: 4 })
                .await,
            PairingResponse::Confirm(true)
        );
        assert_eq!(
            agent
                .respond(PairingRequest::ConfirmPasskey { passkey: 5 })
                .await,
            PairingResponse::Confirm(false)
        );
    }
}
