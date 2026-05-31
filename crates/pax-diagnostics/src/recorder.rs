//! Capturing a live event stream into memory for later analysis.

use std::sync::Mutex;

use pax_core::{Observer, TransitEvent};

/// An [`Observer`] that accumulates every [`TransitEvent`] it receives.
///
/// Attach it to a backend (wrapped in an `Arc`) and it records the whole session;
/// hand the captured events to an [`Analyzer`](crate::Analyzer) afterward. Because
/// the mock backend's stream is deterministic, a `Recorder` over a mock run yields
/// byte-for-byte reproducible analysis.
///
/// ```
/// use std::sync::Arc;
/// use pax_core::{Observer, SharedObserver};
/// use pax_diagnostics::Recorder;
///
/// let recorder = Arc::new(Recorder::new());
/// // Coerce to the shared observer type a backend expects:
/// let observer: SharedObserver = recorder.clone();
/// // ... drive the backend with `observer` ...
/// assert_eq!(recorder.len(), 0); // nothing recorded yet
/// # let _ = observer;
/// ```
#[derive(Debug, Default)]
pub struct Recorder {
    events: Mutex<Vec<TransitEvent>>,
}

impl Recorder {
    /// A fresh, empty recorder.
    pub fn new() -> Self {
        Recorder::default()
    }

    /// A snapshot copy of the events recorded so far, in arrival order.
    pub fn events(&self) -> Vec<TransitEvent> {
        self.events.lock().unwrap().clone()
    }

    /// How many events have been recorded.
    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop all recorded events, returning them.
    pub fn drain(&self) -> Vec<TransitEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

impl Observer for Recorder {
    fn on_event(&self, event: &TransitEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pax_core::{
        ControllerModel, CoreVersion, DeviceId, EventOrigin, SpecContext, StandardsProfile,
        Timestamp, TransitEventKind, Transport,
    };

    fn event(seq: u64) -> TransitEvent {
        TransitEvent::new(
            seq,
            Timestamp::from_millis(seq),
            EventOrigin::new(
                ControllerModel::virtual_model("mock-0"),
                DeviceId::default(),
                SpecContext::new(CoreVersion::V5_2, Transport::BrEdr),
                StandardsProfile::bredr_default(),
            ),
            TransitEventKind::Connected,
        )
    }

    #[test]
    fn records_in_order() {
        let r = Recorder::new();
        r.on_event(&event(0));
        r.on_event(&event(1));
        assert_eq!(r.len(), 2);
        assert_eq!(r.events()[1].seq, 1);
        let drained = r.drain();
        assert_eq!(drained.len(), 2);
        assert!(r.is_empty());
    }
}
