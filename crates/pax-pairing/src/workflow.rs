//! A small retrying pairing workflow on top of [`BluetoothBackend::pair`].
//!
//! Real pairing is flaky: a device may be slow to enter pairing mode, an inquiry
//! may need repeating. This module adds bounded retries around the raw backend
//! call, while being careful **not** to retry a genuine rejection (if the agent or
//! the user said "no", asking again is wrong).

use std::sync::Arc;

use pax_core::DeviceId;
use pax_transport::pairing::{PairingAgent, PairingOutcome};
use pax_transport::{BluetoothBackend, TransportError};

/// Tunables for [`pair_device`].
#[derive(Clone, Copy, Debug)]
pub struct PairOptions {
    /// How many *extra* attempts to make after the first failure (so `retries: 2`
    /// means up to three attempts total). Only transient failures are retried.
    pub retries: u32,
}

impl Default for PairOptions {
    fn default() -> Self {
        PairOptions { retries: 2 }
    }
}

/// The outcome of a (possibly multi-attempt) pairing run.
#[derive(Clone, Debug)]
pub struct PairReport {
    /// The successful outcome.
    pub outcome: PairingOutcome,
    /// How many attempts were made (1 on first-try success).
    pub attempts: u32,
}

/// Pair with `target`, retrying transient failures up to `options.retries` times.
///
/// A [`TransportError::PairingRejected`] is returned immediately and never
/// retried — a refusal is a decision, not a glitch. A
/// [`TransportError::PairingFailed`] (timeout, mismatch) is retried.
///
/// ```
/// use std::sync::Arc;
/// use pax_core::{DeviceId, NoopObserver, SharedObserver};
/// use pax_transport::mock::{MockBackend, MockDevice};
/// use pax_pairing::{agents::AcceptAllAgent, pair_device, PairOptions};
///
/// # async fn run() -> Result<(), pax_transport::TransportError> {
/// let id: DeviceId = "AA:BB:CC:DD:EE:FF".parse().unwrap();
/// let backend = MockBackend::builder()
///     .device(MockDevice::new(id, "Speaker"))
///     .build();
///
/// let agent = Arc::new(AcceptAllAgent::new());
/// let report = pair_device(&backend, id, agent, PairOptions::default()).await?;
/// assert!(report.outcome.paired);
/// assert_eq!(report.attempts, 1);
/// # Ok(()) }
/// ```
pub async fn pair_device(
    backend: &dyn BluetoothBackend,
    target: DeviceId,
    agent: Arc<dyn PairingAgent>,
    options: PairOptions,
) -> Result<PairReport, TransportError> {
    let max_attempts = options.retries.saturating_add(1);
    let mut last_err = None;

    for attempt in 1..=max_attempts {
        match backend.pair(target, agent.clone()).await {
            Ok(outcome) => {
                return Ok(PairReport {
                    outcome,
                    attempts: attempt,
                })
            }
            // A rejection is final — do not retry.
            Err(e @ TransportError::PairingRejected(_)) => return Err(e),
            // Device-not-found is also final.
            Err(e @ TransportError::DeviceNotFound(_)) => return Err(e),
            // Everything else is treated as transient.
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| {
        TransportError::PairingFailed("exhausted retries with no error recorded".into())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AcceptAllAgent, RejectAllAgent};
    use pax_transport::mock::{MockBackend, MockDevice};

    fn id() -> DeviceId {
        "AA:BB:CC:DD:EE:FF".parse().unwrap()
    }

    #[tokio::test]
    async fn first_try_success() {
        let backend = MockBackend::builder()
            .device(MockDevice::new(id(), "ok"))
            .build();
        let agent = Arc::new(AcceptAllAgent::new());
        let report = pair_device(&backend, id(), agent, PairOptions::default())
            .await
            .unwrap();
        assert_eq!(report.attempts, 1);
        assert!(report.outcome.bonded);
    }

    #[tokio::test]
    async fn rejection_is_not_retried() {
        let backend = MockBackend::builder()
            .device(MockDevice::new(id(), "no"))
            .build();
        let agent = Arc::new(RejectAllAgent);
        let err = pair_device(&backend, id(), agent, PairOptions { retries: 5 })
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::PairingRejected(_)));
    }

    #[tokio::test]
    async fn transient_failure_is_retried_then_surfaced() {
        let backend = MockBackend::builder()
            .device(MockDevice::new(id(), "flaky").failing_pairing("device busy"))
            .build();
        let agent = Arc::new(AcceptAllAgent::new());
        let err = pair_device(&backend, id(), agent, PairOptions { retries: 2 })
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::PairingFailed(_)));
    }
}
