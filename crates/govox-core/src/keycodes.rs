//! Key-name → Linux keycode translation for `ydotool`.
//!
//! `ydotool key` does **not** accept key names. Its help is explicit:
//!
//! ```text
//! Syntax: <keycode>:<pressed>
//! e.g. 28:1 28:0 means pressing on the Enter button on a standard US keyboard.
//! Non-interpretable values, such as 0, aaa, l0l, will only cause a delay.
//! ```
//!
//! It exits 0 on a name it cannot parse, so `ydotool key enter` is a *silent
//! no-op* that no return-code check can catch. Every keystroke govox emits
//! therefore has to be compiled to raw keycodes here.
//!
//! `govox-py` guards this with a negative test asserting the argv is
//! `["ydotool", "key", "28:1", "28:0"]` and never a name. That test is ported
//! too, but the type system carries the weight: [`KeyCode`] wraps a `u16` with
//! no public constructor and no `From<&str>`, so the only way to obtain one is
//! [`KeyCode::named`], a lookup in the table below. A [`KeyEvent`] renders as
//! `<code>:<pressed>` and nothing else. There is no code path that can hand
//! `ydotool key` a name, because there is no value of the right type to hand it.
//!
//! Codes are from `/usr/include/linux/input-event-codes.h` (`KEY_*`).

use std::fmt;

/// Every key name govox can translate, paired with its Linux keycode.
///
/// A slice of pairs rather than a map: 25 entries is far below the size where
/// hashing wins, and a `const` slice keeps the table visible in one screen and
/// diffable against `govox-py`'s dict.
const KEYCODES: &[(&str, u16)] = &[
    ("esc", 1),
    ("backspace", 14),
    ("tab", 15),
    ("y", 21),
    ("enter", 28),
    ("ctrl", 29),
    ("a", 30),
    ("shift", 42),
    ("z", 44),
    ("x", 45),
    ("c", 46),
    ("v", 47),
    ("alt", 56),
    ("space", 57),
    ("home", 102),
    ("up", 103),
    ("pageup", 104),
    ("left", 105),
    ("right", 106),
    ("end", 107),
    ("down", 108),
    ("pagedown", 109),
    ("insert", 110),
    ("delete", 111),
    ("meta", 125),
];

/// Modifiers must be pressed before, and released after, the base key.
const MODIFIERS: &[&str] = &["ctrl", "shift", "alt", "meta"];

/// A Linux input keycode that is known to be in [`KEYCODES`].
///
/// Deliberately opaque. There is no `KeyCode(28)`, no `From<u16>` and no
/// `From<&str>`: the only constructor is [`KeyCode::named`], so a `KeyCode`
/// existing at all is proof that a name was successfully translated. See the
/// module docs for why that matters more than it looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyCode(u16);

impl KeyCode {
    /// Look up `name` in the keycode table.
    ///
    /// `name` must already be normalized (trimmed, lowercase); [`parse_chord`]
    /// does that.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        KEYCODES
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, code)| Self(*code))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Whether `name` is a modifier, which decides press ordering.
    #[must_use]
    fn is_modifier(name: &str) -> bool {
        MODIFIERS.contains(&name)
    }
}

/// One press or release of one key.
///
/// Its [`Display`](fmt::Display) is the *only* way to produce a `ydotool key`
/// argument, and it can only render `<keycode>:<0|1>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub pressed: bool,
}

impl fmt::Display for KeyEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.code.get(), u8::from(self.pressed))
    }
}

/// A chord named a key govox cannot translate to a keycode.
///
/// This must fail loudly rather than be dropped: `ydotool` would accept the
/// name and press nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnknownKey {
    #[error("empty key chord: {chord:?}")]
    Empty { chord: String },
    #[error("unknown key name(s) {names:?} in chord {chord:?}")]
    Unknown { names: Vec<String>, chord: String },
    #[error("chord {chord:?} must name exactly one non-modifier key")]
    BaseKeyCount { chord: String },
}

/// Split `"ctrl+shift+left"` into `["ctrl", "shift", "left"]`, normalized.
///
/// Every part must be present in the keycode table; an unknown name is an
/// error, never a silently skipped element.
pub fn parse_chord(chord: &str) -> Result<Vec<String>, UnknownKey> {
    let parts: Vec<String> = chord
        .split('+')
        .map(|part| part.trim().to_lowercase())
        .filter(|part| !part.is_empty())
        .collect();

    if parts.is_empty() {
        return Err(UnknownKey::Empty {
            chord: chord.to_owned(),
        });
    }

    let unknown: Vec<String> = parts
        .iter()
        .filter(|part| KeyCode::named(part).is_none())
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(UnknownKey::Unknown {
            names: unknown,
            chord: chord.to_owned(),
        });
    }

    Ok(parts)
}

/// Compile one chord into its press/release sequence.
///
/// Modifiers are held down around the base key so the compositor sees a real
/// chord rather than a sequence of independent taps.
pub fn chord_to_events(chord: &str) -> Result<Vec<KeyEvent>, UnknownKey> {
    let parts = parse_chord(chord)?;
    let (modifiers, base): (Vec<&String>, Vec<&String>) = parts
        .iter()
        .partition(|part| KeyCode::is_modifier(part.as_str()));

    let [base] = base.as_slice() else {
        return Err(UnknownKey::BaseKeyCount {
            chord: chord.to_owned(),
        });
    };

    // Unwraps are sound: parse_chord already rejected every name absent from
    // the table, so each lookup here is a repeat of one that just succeeded.
    let code = |name: &str| KeyCode::named(name).expect("parse_chord validated every name");

    let mut events = Vec::with_capacity(parts.len() * 2);
    for name in &modifiers {
        events.push(KeyEvent {
            code: code(name),
            pressed: true,
        });
    }
    events.push(KeyEvent {
        code: code(base),
        pressed: true,
    });
    events.push(KeyEvent {
        code: code(base),
        pressed: false,
    });
    for name in modifiers.iter().rev() {
        events.push(KeyEvent {
            code: code(name),
            pressed: false,
        });
    }
    Ok(events)
}

/// Compile a sequence of chords into one flat event list.
pub fn chords_to_events<S>(chords: &[S]) -> Result<Vec<KeyEvent>, UnknownKey>
where
    S: AsRef<str>,
{
    let mut events = Vec::new();
    for chord in chords {
        events.extend(chord_to_events(chord.as_ref())?);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(chord: &str) -> Vec<String> {
        chord_to_events(chord)
            .expect("chord compiles")
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn enter_compiles_to_the_raw_keycode() {
        // KEY_ENTER = 28. The literal that `ydotool key enter` fails to be.
        assert_eq!(rendered("enter"), ["28:1", "28:0"]);
    }

    #[test]
    fn modifiers_wrap_the_base_key() {
        assert_eq!(
            rendered("ctrl+shift+left"),
            ["29:1", "42:1", "105:1", "105:0", "42:0", "29:0"]
        );
    }

    #[test]
    fn modifier_order_follows_the_chord_not_the_table() {
        // govox-py preserves the order the chord names them in, and releases in
        // reverse. "shift+ctrl" is therefore not the same argv as "ctrl+shift",
        // even though both press the same two keys.
        assert_eq!(
            rendered("shift+ctrl+a"),
            ["42:1", "29:1", "30:1", "30:0", "29:0", "42:0"]
        );
    }

    #[test]
    fn names_are_trimmed_and_lowercased() {
        assert_eq!(rendered(" CTRL + V "), rendered("ctrl+v"));
    }

    #[test]
    fn unknown_names_are_rejected_rather_than_skipped() {
        // The whole point: ydotool would take "f13" and press nothing.
        let error = chord_to_events("ctrl+f13").expect_err("f13 is not in the table");
        assert_eq!(
            error,
            UnknownKey::Unknown {
                names: vec!["f13".to_owned()],
                chord: "ctrl+f13".to_owned(),
            }
        );
    }

    #[test]
    fn a_chord_needs_exactly_one_non_modifier() {
        assert!(matches!(
            chord_to_events("ctrl+shift"),
            Err(UnknownKey::BaseKeyCount { .. })
        ));
        assert!(matches!(
            chord_to_events("a+z"),
            Err(UnknownKey::BaseKeyCount { .. })
        ));
    }

    #[test]
    fn empty_chords_are_rejected() {
        assert!(matches!(chord_to_events(""), Err(UnknownKey::Empty { .. })));
        assert!(matches!(
            chord_to_events(" + "),
            Err(UnknownKey::Empty { .. })
        ));
    }

    #[test]
    fn chords_concatenate_in_order() {
        let events = chords_to_events(&["enter", "enter"]).expect("compiles");
        let rendered: Vec<String> = events.iter().map(ToString::to_string).collect();
        assert_eq!(rendered, ["28:1", "28:0", "28:1", "28:0"]);
    }

    #[test]
    fn every_table_name_round_trips() {
        for (name, code) in KEYCODES {
            assert_eq!(
                KeyCode::named(name).map(KeyCode::get),
                Some(*code),
                "{name} is unreachable through the only constructor"
            );
        }
    }

    #[test]
    fn a_key_event_can_only_render_as_code_and_state() {
        // Executable documentation of the trap. A KeyEvent has no rendering
        // that includes the key's name, so no argv built from one can carry a
        // name to ydotool.
        let event = KeyEvent {
            code: KeyCode::named("enter").expect("enter is in the table"),
            pressed: true,
        };
        let rendered = event.to_string();
        assert_eq!(rendered, "28:1");
        assert!(!rendered.contains("enter"));
    }
}
