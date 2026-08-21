//! Desktop notifications.
//!
//! A deliberate divergence rather than a port. `govox-py` declares a
//! `NotifyBackend` protocol and then hardcodes `NullNotifier`, so **every**
//! `notify()` call in it today is a no-op: the clipboard-fallback message, the
//! command-mode announcements and the reload summary are all written and none
//! of them ever reach a user. Here they are delivered.

/// Sends a desktop notification.
pub trait Notifier: Send + Sync {
    fn notify(&self, title: &str, body: &str);
}

/// Delivers over the freedesktop notification service.
pub struct DesktopNotifier {
    /// Notifications to show, drained by the worker thread.
    ///
    /// The worker holds the id of the last one it showed, so each replaces the
    /// previous rather than stacking a new toast: govox speaks often — every
    /// mode switch, every fallback — and without that a minute of use leaves a
    /// column of stale ones.
    sender: std::sync::mpsc::Sender<(String, String)>,
}

impl DesktopNotifier {
    /// Spawns the worker that actually talks to the notification daemon.
    ///
    /// **The work cannot happen on the caller's thread.** `notify_rust` builds
    /// its own tokio runtime and `block_on`s it, which panics with "Cannot
    /// start a runtime from within a runtime" when called from a runtime
    /// worker — and every caller here is on one. That panic killed the
    /// utterance consumer, after which nothing drained the queue and every
    /// subsequent session was dropped as "backlogged": one notification cost
    /// the user every word they said afterwards.
    ///
    /// A plain thread, not `spawn_blocking`: this must work whether or not a
    /// runtime exists, and it keeps `last_id` on the one thread that reads it.
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel::<(String, String)>();
        let name = "govox".to_owned();
        std::thread::Builder::new()
            .name("govox-notify".to_owned())
            .spawn(move || {
                let mut last_id: Option<u32> = None;
                while let Ok((title, body)) = receiver.recv() {
                    show(&name, &title, &body, &mut last_id);
                }
            })
            .ok();
        Self { sender }
    }
}

/// One notification, on the worker thread. Replaces the previous one in place
/// where the daemon supports it, which is what `last_id` is for.
fn show(app_name: &str, title: &str, body: &str, last_id: &mut Option<u32>) {
    let mut notification = notify_rust::Notification::new();
    notification
        .appname(app_name)
        .summary(title)
        .body(body)
        .icon("audio-input-microphone-symbolic")
        .timeout(notify_rust::Timeout::Milliseconds(4000));
    if let Some(id) = *last_id {
        notification.id(id);
    }
    match notification.show() {
        Ok(handle) => *last_id = Some(handle.id()),
        // No notification daemon is an ordinary desktop configuration, not a
        // failure worth interrupting dictation over.
        Err(error) => tracing::debug!(%error, title, "notification not delivered"),
    }
}

impl Default for DesktopNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifier for DesktopNotifier {
    /// Hands the notification to the worker and returns.
    ///
    /// Never blocks and never panics, whatever the desktop is doing: this is
    /// called from the utterance consumer, and that task dying costs the user
    /// their dictation.
    fn notify(&self, title: &str, body: &str) {
        if self
            .sender
            .send((title.to_owned(), body.to_owned()))
            .is_err()
        {
            tracing::debug!(title, "the notifier thread is gone; notification dropped");
        }
    }
}

/// Logs instead of notifying. Used when there is no session bus.
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn notify(&self, title: &str, body: &str) {
        tracing::info!(title, body, "notification");
    }
}

/// Drops every notification.
///
/// This is what `govox-py` does for *all* notifications; here it is only for
/// tests, and choosing it is explicit.
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn notify(&self, _title: &str, _body: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_null_notifier_is_silent_and_infallible() {
        NullNotifier.notify("govox", "nothing happens");
    }

    /// Needs a session bus and a notification daemon.
    #[test]
    #[ignore = "shows a real desktop notification"]
    fn a_notification_reaches_the_desktop() {
        let notifier = DesktopNotifier::new();
        notifier.notify("govox", "M7 notification test");
        // A second one must replace the first rather than stack.
        notifier.notify("govox", "M7 notification test — replaced");
        // The worker owns `last_id` now, so give it a moment to do the work.
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    /// The regression that cost a user their dictation.
    ///
    /// `notify_rust` builds its own runtime and `block_on`s it, which panics
    /// with "Cannot start a runtime from within a runtime" on a tokio worker.
    /// Every caller in the daemon is on one, and the panic killed the utterance
    /// consumer — after which nothing drained the queue and every session was
    /// dropped as "backlogged". Notifying must never take the caller down,
    /// whatever the desktop is or is not running.
    #[tokio::test]
    async fn notifying_from_inside_a_runtime_does_not_kill_the_caller() {
        let notifier = DesktopNotifier::new();
        notifier.notify("govox", "from a runtime worker");
        notifier.notify("govox", "and again");

        // Reached only if the calls above neither panicked nor blocked. Under
        // the previous implementation this test's task died on the first one.
        tokio::task::yield_now().await;
    }

    /// It must also work with no runtime at all — `govox doctor` and the tray
    /// call this from ordinary threads.
    #[test]
    fn notifying_without_a_runtime_is_fine() {
        let notifier = DesktopNotifier::new();
        notifier.notify("govox", "from a plain thread");
    }
}
