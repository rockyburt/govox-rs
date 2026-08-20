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
        "space" => Some(&["space"]),
        _ => None,
    }
}

/// Is this a character `ydotool type` has no way to produce?
///
/// `ydotool` types by emulating a keyboard, so a character reaches the screen
/// only if some keycode produces it. Emoji have none, which is why
/// `[correction] spoken_emoji` could be switched on and still put nothing in the
/// document — the phrase became 👍 and 👍 was then silently dropped.
///
/// The test is deliberately **narrow**: pictographic ranges only, not "non-ASCII".
/// Accented and non-Latin text is alphabetic, is typed today, and must keep
/// going down the same path — rerouting every non-English utterance through the
/// clipboard would be a far larger change than the one being made here. The two
/// non-ASCII marks govox itself produces, `—` (U+2014) and `…` (U+2026), sit
/// below every range listed and so are also untouched.
#[must_use]
pub fn is_pictographic(character: char) -> bool {
    matches!(character as u32,
        0x2600..=0x27BF      // miscellaneous symbols and dingbats: ⚠ ✅ ❌ ❤
        | 0x2B00..=0x2BFF    // miscellaneous symbols and arrows: ⭐
        | 0x1F000..=0x1FAFF  // emoticons, pictographs, transport, supplements
        | 0xFE0F             // variation selector-16, the "render as emoji" mark
        | 0x200D             // zero-width joiner, for composed sequences
    )
}

/// Does this text contain anything `ydotool` cannot type?
#[must_use]
pub fn contains_untypeable(text: &str) -> bool {
    text.chars().any(is_pictographic)
}

/// The same text with the untypeable characters removed.
///
/// Only for the case where there is no clipboard to route them through — the
/// choice there is between typing most of the utterance and typing none of it.
///
/// Spacing is renormalised afterwards rather than left as the hole the removal
/// makes: "Thanks 🙂." would otherwise become "Thanks ." — a space before a full
/// stop, which is wrong in a way the user would have to go back and fix.
/// `normalize_spacing` already collapses that, and reusing it keeps this from
/// growing a second opinion about spacing.
#[must_use]
pub fn strip_untypeable(text: &str) -> String {
    let kept: String = text.chars().filter(|c| !is_pictographic(*c)).collect();
    govox_core::correction::normalize_spacing(&kept)
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
