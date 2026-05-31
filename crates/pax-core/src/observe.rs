//! The sink interface backends use to report [`TransitEvent`]s.
//!
//! This is the seam between the *producers* of events (the transport backends in
//! `pax-transport`) and the *consumers* (the recorder/analyzer in
//! `pax-diagnostics`, or your own logging code). Keeping the trait here in
//! `pax-core` means a backend depends only on core to emit events, and a consumer
//! depends only on core to receive them — neither needs to know about the other.

use std::sync::Arc;

use crate::event::TransitEvent;

/// A sink that receives [`TransitEvent`]s as a link does work.
///
/// Implementations must be cheap and non-blocking: a backend may call
/// [`Observer::on_event`] from a hot path while bytes are moving. If you need to
/// do expensive work (parse, persist, render), buffer the event and hand it off.
///
/// The method takes `&self` (not `&mut self`) so a single observer can be shared
/// behind an [`Arc`] across tasks; use interior mutability (e.g. a `Mutex`) if you
/// need to accumulate. See `pax_diagnostics::Recorder` for a ready-made
/// accumulating implementation.
///
/// ```
/// use std::sync::Mutex;
/// use pax_core::{Observer, TransitEvent};
///
/// /// An observer that just counts events.
/// #[derive(Default)]
/// struct Counter(Mutex<usize>);
/// impl Observer for Counter {
///     fn on_event(&self, _event: &TransitEvent) {
///         *self.0.lock().unwrap() += 1;
///     }
/// }
/// ```
pub trait Observer: Send + Sync {
    /// Called once per event, in `seq` order, on the producing task.
    fn on_event(&self, event: &TransitEvent);
}

/// A convenient alias for a shared, dynamically-dispatched observer. Backends hold
/// one of these and clone the `Arc` cheaply.
pub type SharedObserver = Arc<dyn Observer>;

/// An [`Observer`] that discards every event. This is the default a backend uses
/// when the caller does not care about diagnostics.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopObserver;

impl Observer for NoopObserver {
    fn on_event(&self, _event: &TransitEvent) {}
}

/// Fan an event out to several observers in turn. Useful to log *and* record at
/// once.
///
/// ```
/// use std::sync::Arc;
/// use pax_core::{observe::FanOut, NoopObserver, SharedObserver};
///
/// let fan = FanOut::new(vec![
///     Arc::new(NoopObserver) as SharedObserver,
///     Arc::new(NoopObserver) as SharedObserver,
/// ]);
/// let _shared: SharedObserver = Arc::new(fan);
/// ```
pub struct FanOut {
    observers: Vec<SharedObserver>,
}

impl FanOut {
    /// Build a fan-out over the given observers.
    pub fn new(observers: Vec<SharedObserver>) -> Self {
        FanOut { observers }
    }
}

impl Observer for FanOut {
    fn on_event(&self, event: &TransitEvent) {
        for o in &self.observers {
            o.on_event(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventOrigin, TransitEventKind};
    use crate::hardware::ControllerModel;
    use crate::spec::{CoreVersion, SpecContext, Transport};
    use crate::standards::StandardsProfile;
    use crate::time::Timestamp;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Counter(Mutex<usize>);
    impl Observer for Counter {
        fn on_event(&self, _event: &TransitEvent) {
            *self.0.lock().unwrap() += 1;
        }
    }

    fn sample() -> TransitEvent {
        TransitEvent::new(
            0,
            Timestamp::ZERO,
            EventOrigin::new(
                ControllerModel::virtual_model("mock-0"),
                "00:1A:7D:DA:71:13".parse().unwrap(),
                SpecContext::new(CoreVersion::V5_2, Transport::BrEdr),
                StandardsProfile::bredr_default(),
            ),
            TransitEventKind::Connected,
        )
    }

    #[test]
    fn fanout_delivers_to_all() {
        let a = Arc::new(Counter::default());
        let b = Arc::new(Counter::default());
        let fan = FanOut::new(vec![a.clone(), b.clone()]);
        fan.on_event(&sample());
        fan.on_event(&sample());
        assert_eq!(*a.0.lock().unwrap(), 2);
        assert_eq!(*b.0.lock().unwrap(), 2);
    }
}
