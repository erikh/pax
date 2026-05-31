//! A small retrying pairing workflow on top of [`BluetoothBackend::pair`].
//!
//! Real pairing is flaky: a device may be slow to enter pairing mode, an inquiry
//! may need repeating. This module adds bounded retries around the raw backend
//! call, while being careful **not** to retry a genuine rejection (if the agent or
//! the user said "no", asking again is wrong).

use std::sync::Arc;

use pax_core::DeviceId;
use pax_transport::pairing::{PairingAgent, PairingOutcome};
use pax_transport::{BluetoothBackend, DiscoveryFilter, TransportError};

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

/// Tunables for [`pair_devices`].
#[derive(Clone, Copy, Debug)]
pub struct BatchPairOptions {
    /// How many devices to pair **at once**.
    ///
    /// `None` (the default) reads the backend's
    /// [`Capabilities::max_concurrent_pairings`](pax_transport::Capabilities) — so
    /// the *same call* runs concurrently where the backend can (the mock) and
    /// **transparently downgrades to sequential** where it serializes (a single
    /// real controller). `Some(n)` overrides it.
    pub concurrency: Option<usize>,
    /// How many round-robin passes to make over the still-unpaired devices. A
    /// device that fails transiently in one round is retried in the next.
    pub rounds: u32,
    /// Per-device options (in-attempt retries) passed to [`pair_device`].
    pub per_device: PairOptions,
}

impl Default for BatchPairOptions {
    fn default() -> Self {
        BatchPairOptions {
            concurrency: None,
            rounds: 2,
            per_device: PairOptions::default(),
        }
    }
}

/// The outcome of pairing one device within a [`pair_devices`] batch.
#[derive(Debug)]
pub struct PairItem {
    /// The device this result is for.
    pub id: DeviceId,
    /// The final outcome — `Ok` if it bonded, else the last error seen.
    pub outcome: Result<PairReport, TransportError>,
    /// How many round-robin passes attempted this device.
    pub attempts: u32,
}

impl PairItem {
    /// `true` if the device bonded.
    pub fn paired(&self) -> bool {
        self.outcome.is_ok()
    }
}

/// Pair with **many devices**, round-robin, concurrently where the backend allows.
///
/// Each round attempts every still-unpaired device (with bounded concurrency from
/// [`BatchPairOptions::concurrency`]); successes drop out, transient failures are
/// retried next round, and rejections / device-not-found are final. One device
/// failing never aborts the others — every target gets a [`PairItem`], returned in
/// input order.
///
/// The concurrency is chosen **transparently** from the backend's capabilities, so
/// this one call does the right thing on every backend without the caller branching.
///
/// ```
/// use std::sync::Arc;
/// use pax_core::DeviceId;
/// use pax_transport::mock::{MockBackend, MockDevice};
/// use pax_pairing::{agents::AcceptAllAgent, pair_devices, BatchPairOptions};
///
/// # async fn run() {
/// let ids: Vec<DeviceId> = ["AA:BB:CC:DD:EE:01", "AA:BB:CC:DD:EE:02"]
///     .iter().map(|s| s.parse().unwrap()).collect();
/// let mut b = MockBackend::builder();
/// for id in &ids { b = b.device(MockDevice::new(*id, "dev")); }
/// let backend = b.build();
///
/// let results = pair_devices(&backend, &ids, Arc::new(AcceptAllAgent::new()),
///                            BatchPairOptions::default()).await;
/// assert!(results.iter().all(|r| r.paired()));
/// # }
/// ```
pub async fn pair_devices(
    backend: &dyn BluetoothBackend,
    targets: &[DeviceId],
    agent: Arc<dyn PairingAgent>,
    options: BatchPairOptions,
) -> Vec<PairItem> {
    use futures::stream::StreamExt;
    use std::collections::HashMap;

    // Transparent concurrency: caller override, else the backend's safe limit.
    let concurrency = options
        .concurrency
        .unwrap_or_else(|| backend.capabilities().max_concurrent_pairings)
        .max(1);

    // De-duplicate targets while preserving first-seen order.
    let mut order: Vec<DeviceId> = Vec::new();
    let mut results: HashMap<DeviceId, PairItem> = HashMap::new();
    for &id in targets {
        if results.contains_key(&id) {
            continue;
        }
        order.push(id);
        results.insert(
            id,
            PairItem {
                id,
                outcome: Err(TransportError::PairingFailed("not attempted".into())),
                attempts: 0,
            },
        );
    }

    let mut pending: Vec<DeviceId> = order.clone();
    let rounds = options.rounds.max(1);
    for _round in 0..rounds {
        if pending.is_empty() {
            break;
        }
        let active = concurrency.min(pending.len()).max(1);
        let round_results: Vec<(DeviceId, Result<PairReport, TransportError>)> =
            futures::stream::iter(pending.iter().copied())
                .map(|id| {
                    let agent = agent.clone();
                    async move {
                        (
                            id,
                            pair_device(backend, id, agent, options.per_device).await,
                        )
                    }
                })
                .buffer_unordered(active)
                .collect()
                .await;

        let mut next_pending = Vec::new();
        for (id, outcome) in round_results {
            let item = results.get_mut(&id).expect("known target");
            item.attempts += 1;
            let retryable = matches!(&outcome, Err(e) if !is_final(e));
            item.outcome = outcome;
            if retryable {
                next_pending.push(id);
            }
        }
        pending = next_pending;
    }

    order
        .into_iter()
        .map(|id| results.remove(&id).unwrap())
        .collect()
}

/// Discover devices matching `filter`, then [`pair_devices`] all of them.
pub async fn pair_discovered(
    backend: &dyn BluetoothBackend,
    filter: &DiscoveryFilter,
    agent: Arc<dyn PairingAgent>,
    options: BatchPairOptions,
) -> Result<Vec<PairItem>, TransportError> {
    let found = backend.discover(filter).await?;
    let targets: Vec<DeviceId> = found.iter().map(|d| d.id).collect();
    Ok(pair_devices(backend, &targets, agent, options).await)
}

/// A failure is *final* (not worth another round) if the agent/peer refused or the
/// device is unknown. Everything else (timeouts, transient errors) is retryable.
fn is_final(e: &TransportError) -> bool {
    matches!(
        e,
        TransportError::PairingRejected(_) | TransportError::DeviceNotFound(_)
    )
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

    fn ids(n: u8) -> Vec<DeviceId> {
        (1..=n)
            .map(|i| {
                format!("AA:BB:CC:DD:EE:{i:02X}")
                    .parse::<DeviceId>()
                    .unwrap()
            })
            .collect()
    }

    fn backend_with(targets: &[DeviceId]) -> MockBackend {
        let mut b = MockBackend::builder();
        for id in targets {
            b = b.device(MockDevice::new(*id, "dev"));
        }
        b.build()
    }

    #[tokio::test]
    async fn batch_pairs_all_devices() {
        let targets = ids(5);
        let backend = backend_with(&targets);
        let results = pair_devices(
            &backend,
            &targets,
            Arc::new(AcceptAllAgent::new()),
            BatchPairOptions::default(),
        )
        .await;
        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|r| r.paired()));
        // Results come back in input order.
        assert_eq!(results[0].id, targets[0]);
    }

    #[tokio::test]
    async fn batch_partial_success_does_not_abort() {
        let targets = ids(3);
        // The middle device always fails pairing; the others must still succeed.
        let backend = MockBackend::builder()
            .device(MockDevice::new(targets[0], "ok"))
            .device(MockDevice::new(targets[1], "bad").failing_pairing("nope"))
            .device(MockDevice::new(targets[2], "ok"))
            .build();

        let results = pair_devices(
            &backend,
            &targets,
            Arc::new(AcceptAllAgent::new()),
            BatchPairOptions::default(),
        )
        .await;
        assert!(results[0].paired());
        assert!(!results[1].paired());
        assert!(results[2].paired());
        // The failing device was attempted every round (default rounds = 2).
        assert_eq!(results[1].attempts, 2);
    }

    #[tokio::test]
    async fn batch_sequential_and_concurrent_agree() {
        let targets = ids(4);
        let backend = backend_with(&targets);
        let agent = Arc::new(AcceptAllAgent::new());

        // Explicit sequential (concurrency 1) and the default (transparently
        // concurrent on the mock) must produce the same correct outcome.
        let seq = pair_devices(
            &backend,
            &targets,
            agent.clone(),
            BatchPairOptions {
                concurrency: Some(1),
                ..Default::default()
            },
        )
        .await;
        let conc = pair_devices(&backend, &targets, agent, BatchPairOptions::default()).await;
        assert!(seq.iter().all(|r| r.paired()));
        assert!(conc.iter().all(|r| r.paired()));
    }
}
