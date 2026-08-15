//! Choosing an injection backend, and falling back when it fails.

use std::sync::Arc;

use govox_core::config::{Config, InjectionMethod};
use govox_core::domain::{Capabilities, GovoxError, Injector, InsertionAction};

use crate::clipboard::ClipboardInjector;
use crate::runner::Runner;
use crate::ydotool::YdotoolInjector;

/// Which backend actually carried the most recent insertion.
///
/// Distinct from the one `select_injector` *chose*, which is all anything could
/// report before this existed. The choice is made once from probed
/// capabilities; what happens afterwards is not the same question. `ydotool`
/// can be selected and then reject every call, and the fallback wrapper
/// silently carries on over the clipboard — the exact "reports success and does
/// nothing" shape this project keeps running into, one level up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsedBackend {
    /// Nothing has been injected yet, so only the *selection* is known.
    NotYet,
    Ydotool,
    Clipboard,
}

impl UsedBackend {
    const fn code(self) -> u8 {
        match self {
            Self::NotYet => 0,
            Self::Ydotool => 1,
            Self::Clipboard => 2,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Ydotool,
            2 => Self::Clipboard,
            _ => Self::NotYet,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotYet => "not yet",
            Self::Ydotool => "ydotool",
            Self::Clipboard => "clipboard",
        }
    }
}

/// A shared note of the backend that last did the work.
///
/// An atomic rather than a channel so `govox-input` keeps its four
/// dependencies: this crate is on the injection hot path and has no async
/// runtime, and a watch channel would drag one in to publish a value that
/// changes a handful of times a session.
#[derive(Debug, Clone, Default)]
pub struct InjectionReport(Arc<std::sync::atomic::AtomicU8>);

impl InjectionReport {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, backend: UsedBackend) {
        self.0
            .store(backend.code(), std::sync::atomic::Ordering::Relaxed);
    }

    #[must_use]
    pub fn last(&self) -> UsedBackend {
        UsedBackend::from_code(self.0.load(std::sync::atomic::Ordering::Relaxed))
    }
}

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
    report: InjectionReport,
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
                report: report.clone(),
            },
            clipboard: ClipboardInjector::new(runner, true),
            report,
        });
    }
    // Nothing to discover here: with no `ydotool` there is one backend and it
    // is the one that will run. Recorded up front so the report is right from
    // the first utterance rather than after it.
    report.record(UsedBackend::Clipboard);
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
    pub report: InjectionReport,
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
            let result = self.clipboard.insert(action);
            if result.is_ok() {
                self.report.record(UsedBackend::Clipboard);
            }
            return result;
        }
        self.primary.insert(action)
    }
}

/// Runs `primary`, and on rejection runs `fallback` and says so.
pub struct FallbackInjector<P, F, N> {
    pub primary: P,
    pub fallback: F,
    pub notify: N,
    pub report: InjectionReport,
}

impl<P, F, N> Injector for FallbackInjector<P, F, N>
where
    P: Injector,
    F: Injector,
    N: Notify,
{
    fn insert(&self, action: &InsertionAction) -> Result<(), GovoxError> {
        match self.primary.insert(action) {
            Ok(()) => {
                self.report.record(UsedBackend::Ydotool);
                Ok(())
            }
            Err(GovoxError::InjectionRejected(_)) => {
                // A fallback failure propagates: both backends are gone, and
                // pretending otherwise would drop the utterance silently.
                self.fallback.insert(action)?;
                // Recorded only on success, so the report never claims a
                // backend that did not in fact deliver anything.
                self.report.record(UsedBackend::Clipboard);
                self.notify
                    .notify("govox clipboard fallback", "Text copied to clipboard.");
                Ok(())
            }
            Err(other) => Err(other),
        }
    }
}
