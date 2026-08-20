//! Injection by way of the clipboard: `wl-copy`, then optionally Ctrl+V.
//!
//! The fallback backend. It cannot press keys, so it is never the whole story,
//! but it works on compositors where `/dev/uinput` is unavailable.

use govox_core::domain::{GovoxError, Injector, InsertionAction};
use govox_core::keycodes::{KeyEvent, chords_to_events};

use crate::runner::Runner;

pub struct ClipboardInjector<R: Runner> {
    runner: R,
    paste_after_copy: bool,
}

impl<R: Runner> ClipboardInjector<R> {
    pub const fn new(runner: R, paste_after_copy: bool) -> Self {
        Self {
            runner,
            paste_after_copy,
        }
    }

    fn paste_argv() -> Vec<String> {
        // Raw keycodes: `ydotool key ctrl+v` exits 0 and pastes nothing.
        let events = chords_to_events(&["ctrl+v"]).expect("ctrl+v is in the keycode table");
        let mut argv = vec!["ydotool".to_owned(), "key".to_owned()];
        argv.extend(events.iter().map(KeyEvent::to_string));
        argv
    }
}

impl<R: Runner> Injector for ClipboardInjector<R> {
    fn insert(&self, action: &InsertionAction) -> Result<(), GovoxError> {
        if matches!(action, InsertionAction::Keys(_)) {
            // Keystrokes cannot be expressed as clipboard content; there is
            // nothing this backend can do with them.
            return Err(GovoxError::InjectionRejected(
                "clipboard injector cannot emit keystrokes".to_owned(),
            ));
        }

        let text = action_text(action);
        let copy = self.runner.run(&["wl-copy".to_owned()], Some(text));
        if !copy.is_ok() {
            return Err(GovoxError::InjectionRejected(reason(
                copy.stderr,
                "wl-copy failed",
            )));
        }

        if self.paste_after_copy {
            let paste = self.runner.run(&Self::paste_argv(), None);
            if !paste.is_ok() {
                return Err(GovoxError::InjectionRejected(reason(
                    paste.stderr,
                    "paste failed",
                )));
            }
        }
        Ok(())
    }
}

fn reason(stderr: String, fallback: &str) -> String {
    if stderr.is_empty() {
        fallback.to_owned()
    } else {
        stderr
    }
}

/// The clipboard payload for an action.
///
/// `Keys` never reaches here — [`ClipboardInjector::insert`] rejects it first.
fn action_text(action: &InsertionAction) -> &str {
    match action {
        InsertionAction::Text(text) => text,
        InsertionAction::Command(name) if name == "newline" => "\n",
        InsertionAction::Command(name) if name == "new_paragraph" => "\n\n",
        InsertionAction::Command(name) if name == "space" => " ",
        _ => "",
    }
}
