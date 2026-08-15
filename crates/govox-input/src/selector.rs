//! Choosing an injection backend, and falling back when it fails.

use std::sync::Arc;

use govox_core::config::{Config, InjectionMethod};
use govox_core::domain::{Capabilities, GovoxError, Injector, InsertionAction};

use crate::clipboard::ClipboardInjector;
use crate::runner::Runner;
use crate::ydotool::YdotoolInjector;

/// Told when text went to the clipboard instead of the focused field.
///
/// The user has to know: nothing was typed, and the text is one Ctrl+V away.
pub trait Notify: Send + Sync {
    fn notify(&self, title: &str, body: &str);
}

impl<F> Notify for F
where
    F: Fn(&str, &str) + Send + Sync,
{
    fn notify(&self, title: &str, body: &str) {
        self(title, body);
    }
}

/// Drops every notification. Useful where there is nowhere to show one.
pub struct SilentNotify;

impl Notify for SilentNotify {
    fn notify(&self, _title: &str, _body: &str) {}
}

/// Pick the injector this session can actually use.
///
/// `ydotool` is preferred when configured and available, wrapped so a rejection
/// at runtime degrades to the clipboard rather than losing the utterance. Note
/// the fallback clipboard is built with `paste_after_copy = false`: pasting
/// needs `ydotool`, and we are only here because `ydotool` just failed.
pub fn select_injector<R, N>(
    caps: &Capabilities,
    config: &Config,
    runner: Arc<R>,
    notify: N,
) -> Box<dyn Injector>
where
    R: Runner + 'static,
    N: Notify + 'static,
{
    let clipboard = ClipboardInjector::new(Arc::clone(&runner), false);
    let prefers_ydotool = matches!(
        config.injection.method,
        InjectionMethod::Ydotool | InjectionMethod::Auto
    );

    if prefers_ydotool && caps.supports_injection("ydotool") {
        // `ydotool` is available, so Ctrl+V is available, so the pasting
        // clipboard is a real option here — unlike the fallback below, which
        // exists precisely because `ydotool` just failed.
        return Box::new(UntypeableViaClipboard {
            primary: FallbackInjector {
                primary: YdotoolInjector::new(Arc::clone(&runner)),
                fallback: clipboard,
                notify,
            },
            clipboard: ClipboardInjector::new(runner, true),
        });
    }
    Box::new(clipboard)
}

/// Sends text `ydotool` cannot type through the clipboard, and pastes it.
///
/// Only emoji reach this path in practice. They are the one thing the
/// correction pipeline can produce that the default injector cannot deliver:
/// `ydotool` emulates keycodes, and no keycode produces 👍. Before this, a
/// spoken emoji became the character and the character was then silently
/// dropped, so `[correction] spoken_emoji` appeared to do nothing.
///
/// This is a *router*, not another fallback: the decision is made from the text
/// before anything is attempted, because the failure it avoids is not one
/// `ydotool` reports. It exits 0 either way.
pub struct UntypeableViaClipboard<P, F> {
    pub primary: P,
    pub clipboard: F,
}

impl<P, F> Injector for UntypeableViaClipboard<P, F>
where
    P: Injector,
    F: Injector,
{
    fn insert(&self, action: &InsertionAction) -> Result<(), GovoxError> {
        if let InsertionAction::Text(text) = action
            && crate::ydotool::contains_untypeable(text)
        {
            // No notification: unlike the clipboard *fallback*, this pastes for
            // the user, so there is nothing for them to do and nothing to say.
            return self.clipboard.insert(action);
        }
        self.primary.insert(action)
    }
}

/// Runs `primary`, and on rejection runs `fallback` and says so.
pub struct FallbackInjector<P, F, N> {
    pub primary: P,
    pub fallback: F,
    pub notify: N,
}

impl<P, F, N> Injector for FallbackInjector<P, F, N>
where
    P: Injector,
    F: Injector,
    N: Notify,
{
    fn insert(&self, action: &InsertionAction) -> Result<(), GovoxError> {
        match self.primary.insert(action) {
            Ok(()) => Ok(()),
            Err(GovoxError::InjectionRejected(_)) => {
                // A fallback failure propagates: both backends are gone, and
                // pretending otherwise would drop the utterance silently.
                self.fallback.insert(action)?;
                self.notify
                    .notify("govox clipboard fallback", "Text copied to clipboard.");
                Ok(())
            }
            Err(other) => Err(other),
        }
    }
}
