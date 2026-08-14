//! Keeping the focused accessible current by listening, instead of searching.
//!
//! Focus is an *event*. Walking the tree to find it is both the slowest step in
//! a command and — as three separate bugs in `govox-py` showed — a search that
//! can quietly stop early and report "nothing focused" for an application that
//! is exposing exactly what was wanted.
//!
//! **The tracker is an accelerator, never a source of truth on its own.**
//! [`FocusTracker::focused`] returns `None` whenever it is not confident, and
//! the reader falls back to walking. In `govox-py` that caveat came with a
//! second one — AT-SPI events arrive only while a GLib main loop is pumping,
//! and the daemon has one only when the tray is enabled, so the tracker ran its
//! own loop on its own thread. Here it is a zbus signal stream on an ordinary
//! tokio task, so the caveat is gone; what remains is that a tracked node still
//! has to pass the same checks a walk would apply.

use std::sync::{Arc, Mutex};

use atspi::ObjectRefOwned;
use atspi::events::object::StateChangedEvent;
use futures_util::StreamExt;

/// The last node observed gaining focus, and the application it belongs to.
#[derive(Debug, Default)]
pub struct FocusTracker {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    focused: Option<ObjectRefOwned>,
    application: Option<String>,
}

impl FocusTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The node that last gained focus, if one is held.
    #[must_use]
    pub fn focused(&self) -> Option<ObjectRefOwned> {
        self.lock().focused.clone()
    }

    /// Record a focus change, returning whether the *application* changed.
    ///
    /// The application change is what matters to the caller: it is the trigger
    /// `DictationBuffer::reset` never had. Until focus events existed the
    /// buffer expired by age instead, which bounds the stale-delete window
    /// rather than closing it.
    fn gained(&self, object: ObjectRefOwned) -> bool {
        let application = object.name_as_str().unwrap_or_default().to_owned();
        let mut inner = self.lock();
        let changed = inner.application.as_deref() != Some(application.as_str());
        inner.focused = Some(object);
        inner.application = Some(application);
        changed
    }

    /// Record a focus loss.
    ///
    /// Only clears when the node losing focus is the one held: a stale loss for
    /// an object already replaced is noise, and acting on it would throw away a
    /// perfectly good tracked node.
    fn lost(&self, object: &ObjectRefOwned) {
        let mut inner = self.lock();
        if inner.focused.as_ref() == Some(object) {
            inner.focused = None;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Follow `object:state-changed:focused` until the connection ends.
///
/// `on_application_change` is called outside the lock: it resets the dictation
/// buffer, and holding a lock across a foreign callback invites a deadlock for
/// nothing.
pub async fn follow(
    connection: atspi::AccessibilityConnection,
    tracker: Arc<FocusTracker>,
    on_application_change: impl Fn() + Send + 'static,
) {
    if let Err(error) = connection.register_event::<StateChangedEvent>().await {
        // Degrades to exactly the behaviour from before the tracker existed:
        // every read walks the tree.
        tracing::debug!(%error, "AT-SPI focus tracking unavailable; using the walk");
        return;
    }
    tracing::info!("AT-SPI focus tracking active");

    let mut events = std::pin::pin!(connection.event_stream());
    while let Some(event) = events.next().await {
        let Ok(atspi::Event::Object(atspi::events::ObjectEvents::StateChanged(event))) = event
        else {
            continue;
        };
        if event.state != atspi::State::Focused {
            continue;
        }
        if event.enabled {
            if tracker.gained(event.item) {
                on_application_change();
            }
        } else {
            tracker.lost(&event.item);
        }
    }
    tracing::debug!("AT-SPI event stream ended");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(name: &'static str, path: &'static str) -> ObjectRefOwned {
        ObjectRefOwned::from_static_str_unchecked(name, path)
    }

    #[test]
    fn a_new_application_taking_focus_is_reported_as_a_change() {
        let tracker = FocusTracker::new();
        assert!(tracker.gained(object(":1.10", "/org/a11y/atspi/accessible/1")));
        assert!(
            !tracker.gained(object(":1.10", "/org/a11y/atspi/accessible/2")),
            "moving between fields in one application is not a window change"
        );
        assert!(tracker.gained(object(":1.20", "/org/a11y/atspi/accessible/1")));
    }

    #[test]
    fn losing_focus_clears_only_the_node_that_is_held() {
        let tracker = FocusTracker::new();
        let held = object(":1.10", "/org/a11y/atspi/accessible/1");
        tracker.gained(held.clone());

        // A late loss for something we already replaced is noise. Acting on it
        // would throw away a tracked node that is perfectly current.
        tracker.lost(&object(":1.10", "/org/a11y/atspi/accessible/99"));
        assert_eq!(tracker.focused(), Some(held.clone()));

        tracker.lost(&held);
        assert_eq!(tracker.focused(), None);
    }
}
