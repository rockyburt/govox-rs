//! Typing and keystroke injection via `ydotool`.

use govox_core::domain::{GovoxError, Injector, InsertionAction};
use govox_core::keycodes::{KeyEvent, chords_to_events};

use crate::runner::Runner;

/// Formatting commands, as key chords.
///
/// Compiled to raw keycodes before use — `ydotool key enter` exits 0 without
/// pressing anything (see [`govox_core::keycodes`]).
fn command_chords(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "newline" => Some(&["enter"]),
        "new_paragraph" => Some(&["enter", "enter"]),
        _ => None,
    }
}

pub struct YdotoolInjector<R: Runner> {
    runner: R,
}

impl<R: Runner> YdotoolInjector<R> {
    pub const fn new(runner: R) -> Self {
        Self { runner }
    }

    /// Type text, pressing Enter for each line break rather than typing it.
    ///
    /// Spoken "new line" can appear mid-utterance, so dictated text can carry
    /// newlines. Whether `ydotool type` turns a `\n` in its argument into an
    /// Enter keypress is not something this codebase should assume: `ydotool
    /// key enter` already looked like it worked while pressing nothing, and a
    /// break that silently vanishes is the same class of bug. Splitting on the
    /// newline and reusing the same chord path as `Command("newline")` removes
    /// the question entirely.
    fn type_text(&self, text: &str) -> Result<(), GovoxError> {
        for (index, segment) in text.split('\n').enumerate() {
            if index > 0 {
                self.press(&["enter"])?;
            }
            if !segment.is_empty() {
                self.run(&["ydotool", "type", segment])?;
            }
        }
        Ok(())
    }

    fn press<S: AsRef<str>>(&self, chords: &[S]) -> Result<(), GovoxError> {
        // A chord govox cannot translate must fail loudly. ydotool would
        // accept it and press nothing.
        let events = chords_to_events(chords)
            .map_err(|err| GovoxError::InjectionRejected(err.to_string()))?;
        if events.is_empty() {
            return Ok(());
        }
        // `KeyEvent` renders only as `<code>:<pressed>`, so this argv cannot
        // carry a key name however the chord was spelled.
        let rendered: Vec<String> = events.iter().map(KeyEvent::to_string).collect();
        let mut argv = vec!["ydotool".to_owned(), "key".to_owned()];
        argv.extend(rendered);
        self.run_owned(argv)
    }

    fn run(&self, command: &[&str]) -> Result<(), GovoxError> {
        self.run_owned(command.iter().map(|part| (*part).to_owned()).collect())
    }

    fn run_owned(&self, command: Vec<String>) -> Result<(), GovoxError> {
        let result = self.runner.run(&command, None);
        if result.is_ok() {
            return Ok(());
        }
        let detail = if result.stderr.is_empty() {
            format!(
                "{} failed",
                command.first().map_or("command", String::as_str)
            )
        } else {
            result.stderr
        };
        Err(GovoxError::InjectionRejected(detail))
    }
}

impl<R: Runner> Injector for YdotoolInjector<R> {
    fn insert(&self, action: &InsertionAction) -> Result<(), GovoxError> {
        match action {
            InsertionAction::Text(text) => self.type_text(text),
            // An unrecognised command name is a no-op, matching govox-py: the
            // correction pipeline is the only source of these names, and a
            // typo there should not surface as a failed injection.
            InsertionAction::Command(name) => match command_chords(name) {
                Some(chords) => self.press(chords),
                None => Ok(()),
            },
            InsertionAction::Keys(chords) => self.press(chords),
        }
    }
}
