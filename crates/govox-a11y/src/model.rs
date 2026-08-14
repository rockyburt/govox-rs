//! A dictation buffer that can also look at the focused field.
//!
//! This does not *replace* the dictation buffer — it wraps one. Everything
//! govox typed is still tracked by `DictationBuffer`; the only thing AT-SPI
//! adds is the ability to look at the focused widget and check that belief
//! against reality. With an unreadable field, behaviour is identical to running
//! without this backend at all.
//!
//! Field access is an enhancement, never a dependency. Coverage is a property
//! of the focused *element*, not of the desktop: on one machine at one moment
//! GTK4 reads and writes, Chromium reads only with a launch flag, and Electron
//! and every terminal expose nothing.

use std::sync::Arc;
use std::time::Duration;

use govox_core::domain::{FieldSnapshot, TextModel};
use govox_core::textmodel::DictationBuffer;
use tokio::sync::{mpsc, oneshot};

use crate::A11yError;
use crate::reader::FieldReader;
use crate::tracker::{FocusTracker, follow};

/// How long a read may block the caller.
///
/// The reader's own budget plus room for the round-trips around it. Reaching
/// this means the accessibility bus is wedged, not that a tree is large — the
/// walk bounds itself, and an unbounded wait here would turn a hung peer into a
/// hung dictation daemon.
const READ_TIMEOUT: Duration = Duration::from_millis(250);

/// What the AT-SPI task is asked for.
enum Request {
    Read(oneshot::Sender<Option<FieldSnapshot>>),
    ActiveWindow(oneshot::Sender<Option<String>>),
}

/// A `TextModel` backed by the accessibility bus.
pub struct AtspiTextModel {
    buffer: Arc<DictationBuffer>,
    requests: mpsc::Sender<Request>,
}

impl AtspiTextModel {
    /// Connect, start following focus, and return a model.
    ///
    /// `ttl_s` is the dictation buffer's, unchanged. Focus events close the
    /// stale-delete window properly, but the age bound stays as the answer for
    /// a desktop that delivers no events — which is the configuration this has
    /// to keep working in.
    pub async fn connect(ttl_s: f64) -> Result<Self, A11yError> {
        let reader = FieldReader::connect().await?;
        let tracker = Arc::new(FocusTracker::new());
        let buffer = Arc::new(DictationBuffer::new(ttl_s));

        // `reset()` finally has a trigger. Nothing on Wayland told `govox-py`
        // the focused window had changed, so the buffer expired by age instead
        // — a bound on the stale-delete window rather than a fix for it.
        let on_change = {
            let buffer = Arc::clone(&buffer);
            move || buffer.reset()
        };
        tokio::spawn(follow(
            reader.connection().clone(),
            Arc::clone(&tracker),
            on_change,
        ));

        let (requests, incoming) = mpsc::channel(8);
        tokio::spawn(serve(reader, tracker, incoming));

        Ok(Self { buffer, requests })
    }

    /// A label for the window a read would come from, or `None` when unknown.
    ///
    /// Exposed on the model because per-application overrides need to know
    /// *which* application is being dictated into, and the reader is the only
    /// thing that can answer.
    #[must_use]
    pub fn active_window(&self) -> Option<String> {
        self.ask(Request::ActiveWindow).flatten()
    }

    /// Send a request and wait for its answer, bounded.
    ///
    /// `read_field` is synchronous because [`TextModel`] is, and `TextModel` is
    /// synchronous because it is read from the middle of the correction
    /// pipeline in `govox-core`, which has no runtime. So the async work lives
    /// on its own task and this blocks on the reply — inside `block_in_place`,
    /// which hands the runtime's other tasks to a different worker rather than
    /// stalling them behind an accessibility round-trip.
    ///
    /// Every failure path returns `None`, which is the answer callers already
    /// handle: a bus that has gone away, a task that has stopped, and an
    /// application that never answers are the same event to them.
    fn ask<T>(&self, build: impl FnOnce(oneshot::Sender<T>) -> Request) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        if self.requests.try_send(build(reply)).is_err() {
            // Full or closed. Full means a read is already in flight and the
            // caller is better served by the buffer than by queueing behind it.
            tracing::debug!("AT-SPI reader is unavailable; falling back to the buffer");
            return None;
        }
        let wait = || answer.blocking_recv().ok();
        match tokio::runtime::Handle::try_current() {
            Ok(_) => tokio::task::block_in_place(wait),
            // Not on a runtime: a test, or a caller that built the model
            // elsewhere. Blocking is then exactly what it looks like.
            Err(_) => wait(),
        }
    }
}

/// The AT-SPI task: owns the connection and serialises every read.
///
/// One at a time on purpose. Concurrent walks of the same tree contend on the
/// same peers, and the second answer is the same as the first.
async fn serve(
    reader: FieldReader,
    tracker: Arc<FocusTracker>,
    mut requests: mpsc::Receiver<Request>,
) {
    while let Some(request) = requests.recv().await {
        match request {
            Request::Read(reply) => {
                let tracked = tracker.focused();
                let snapshot = timeout(reader.read(tracked.as_ref())).await.flatten();
                let _ = reply.send(snapshot);
            }
            Request::ActiveWindow(reply) => {
                let _ = reply.send(timeout(reader.active_window()).await.flatten());
            }
        }
    }
}

/// Bound one accessibility operation.
async fn timeout<T>(work: impl Future<Output = Option<T>>) -> Option<Option<T>> {
    match tokio::time::timeout(READ_TIMEOUT, work).await {
        Ok(answer) => Some(answer),
        Err(_) => {
            tracing::debug!("the accessibility bus did not answer in time");
            None
        }
    }
}

impl TextModel for AtspiTextModel {
    fn last_insertion(&self) -> Option<String> {
        self.buffer.last_insertion()
    }

    fn record_insertion(&self, text: &str) {
        self.buffer.record_insertion(text);
    }

    fn consume_last(&self) -> Option<String> {
        self.buffer.consume_last()
    }

    /// The focused field, or `None` when it cannot be read.
    ///
    /// Never fails. A bus that has gone away, an application that dies
    /// mid-read, a toolkit that exposes a broken node — all of them mean the
    /// same thing to a caller, and turning any of them into an error would make
    /// field access a dependency.
    fn read_field(&self) -> Option<FieldSnapshot> {
        self.ask(Request::Read).flatten()
    }

    /// The focused window, as `"Application / Title"`.
    ///
    /// Delegates to the inherent method of the same name, which existed for
    /// the diagnostics before the trait did. Leaving the trait on its `None`
    /// default meant `[[feedback.app_rules]]` could never match anything,
    /// while the label it needed was already being produced a few lines away.
    fn active_window(&self) -> Option<String> {
        Self::active_window(self)
    }

    fn reset(&self) {
        self.buffer.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::DEFAULT_BUDGET;

    #[test]
    fn a_read_is_bounded_by_more_than_the_walks_own_budget() {
        // The walk bounds itself; this bounds everything around it. Equal or
        // shorter would make the timeout fire on healthy reads that simply
        // used their whole budget.
        assert!(READ_TIMEOUT > DEFAULT_BUDGET);
    }
}
