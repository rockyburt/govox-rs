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
    /// Replaces the previous notification instead of stacking a new one.
    ///
    /// govox speaks often — every mode switch, every fallback — and without
    /// this a minute of use leaves a column of stale toasts.
    last_id: std::sync::Mutex<Option<u32>>,
    app_name: String,
}

impl DesktopNotifier {
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_id: std::sync::Mutex::new(None),
            app_name: "govox".to_owned(),
        }
    }
}

impl Default for DesktopNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifier for DesktopNotifier {
    fn notify(&self, title: &str, body: &str) {
        let mut last = self.last_id.lock().expect("notifier poisoned");

        let mut notification = notify_rust::Notification::new();
        notification
            .appname(&self.app_name)
            .summary(title)
            .body(body)
            .icon("audio-input-microphone-symbolic")
            .timeout(notify_rust::Timeout::Milliseconds(4000));
        if let Some(id) = *last {
            notification.id(id);
        }

        match notification.show() {
            Ok(handle) => *last = Some(handle.id()),
            // No notification daemon is an ordinary desktop configuration, not
            // a failure worth interrupting dictation over.
            Err(error) => {
                tracing::debug!(%error, title, "notification not delivered");
            }
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
        assert!(notifier.last_id.lock().unwrap().is_some());
    }
}
