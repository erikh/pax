//! Turning the raw [`TransitEvent`] stream into ergonomic progress callbacks.
//!
//! A backend reports transfer activity by emitting events to its
//! [`Observer`]. To surface a progress bar you do not want to
//! match raw events yourself — attach a [`ProgressObserver`] (typically via a
//! [`FanOut`](pax_core::observe::FanOut) alongside your diagnostics recorder) and
//! receive clean [`ProgressUpdate`]s instead.

use std::sync::Mutex;

use pax_core::{Observer, TransitEvent, TransitEventKind};

/// A clean, transfer-oriented view of progress, distilled from the event stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgressUpdate {
    /// The object name being transferred.
    pub object: String,
    /// Bytes transferred so far.
    pub transferred: u64,
    /// Total bytes in the object.
    pub total: u64,
    /// `true` on the final update for an object.
    pub done: bool,
}

impl ProgressUpdate {
    /// Completion as a fraction in `0.0..=1.0`. Returns `1.0` for a zero-byte
    /// object once `done`.
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            if self.done {
                1.0
            } else {
                0.0
            }
        } else {
            (self.transferred as f64 / self.total as f64).min(1.0)
        }
    }
}

struct Inner<F> {
    object: String,
    total: u64,
    callback: F,
}

/// An [`Observer`] that invokes a callback with a [`ProgressUpdate`] for each
/// transfer milestone (start, each progress tick, completion).
///
/// ```
/// use std::sync::{Arc, Mutex};
/// use pax_core::{Observer, SharedObserver};
/// use pax_transfer::progress::{ProgressObserver, ProgressUpdate};
///
/// let log: Arc<Mutex<Vec<ProgressUpdate>>> = Arc::new(Mutex::new(Vec::new()));
/// let sink = log.clone();
/// let observer: SharedObserver = Arc::new(ProgressObserver::new(move |u: ProgressUpdate| {
///     sink.lock().unwrap().push(u);
/// }));
/// // `observer` can now be passed to `MockBackend::builder().observer(observer)`.
/// # let _ = observer;
/// ```
pub struct ProgressObserver<F> {
    inner: Mutex<Inner<F>>,
}

impl<F> ProgressObserver<F>
where
    F: FnMut(ProgressUpdate) + Send,
{
    /// Build a progress observer that calls `callback` on each milestone.
    pub fn new(callback: F) -> Self {
        ProgressObserver {
            inner: Mutex::new(Inner {
                object: String::new(),
                total: 0,
                callback,
            }),
        }
    }
}

impl<F> Observer for ProgressObserver<F>
where
    F: FnMut(ProgressUpdate) + Send,
{
    fn on_event(&self, event: &TransitEvent) {
        let mut inner = self.inner.lock().unwrap();
        match &event.kind {
            TransitEventKind::TransferStarted { name, total_bytes } => {
                inner.object = name.clone();
                inner.total = *total_bytes;
                let update = ProgressUpdate {
                    object: name.clone(),
                    transferred: 0,
                    total: *total_bytes,
                    done: false,
                };
                (inner.callback)(update);
            }
            TransitEventKind::TransferProgress { transferred, total } => {
                let update = ProgressUpdate {
                    object: inner.object.clone(),
                    transferred: *transferred,
                    total: *total,
                    done: false,
                };
                (inner.callback)(update);
            }
            TransitEventKind::TransferCompleted { bytes, .. } => {
                let object = inner.object.clone();
                let update = ProgressUpdate {
                    object,
                    transferred: *bytes,
                    total: *bytes,
                    done: true,
                };
                (inner.callback)(update);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_handles_zero_total() {
        let mid = ProgressUpdate {
            object: "f".into(),
            transferred: 0,
            total: 0,
            done: false,
        };
        assert_eq!(mid.fraction(), 0.0);
        let done = ProgressUpdate {
            object: "f".into(),
            transferred: 0,
            total: 0,
            done: true,
        };
        assert_eq!(done.fraction(), 1.0);
        let half = ProgressUpdate {
            object: "f".into(),
            transferred: 50,
            total: 100,
            done: false,
        };
        assert_eq!(half.fraction(), 0.5);
    }
}
