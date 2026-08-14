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
        return Box::new(FallbackInjector {
            primary: YdotoolInjector::new(runner),
            fallback: clipboard,
            notify,
        });
    }
    Box::new(clipboard)
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
