//! The parameterized editing-command grammar.
//!
//! Ported from `correction/grammar.py`. `delete`, `select` and `move` share one
//! rule and one compiler, differing only in the chord table the editor looks
//! them up in — so a new verb is a table entry, not a new regex.

use std::sync::LazyLock;

use regex::Regex;

use super::numbers::{NUMBER_WORDS, number_word};
use crate::domain::{Direction, EditAction, EditOp, Unit};

pub const UNIT_WORDS: &[(&str, Unit)] = &[
    ("character", Unit::Character),
    ("characters", Unit::Character),
    ("letter", Unit::Character),
    ("letters", Unit::Character),
    ("word", Unit::Word),
    ("words", Unit::Word),
    ("sentence", Unit::Sentence),
    ("sentences", Unit::Sentence),
    ("paragraph", Unit::Paragraph),
    ("paragraphs", Unit::Paragraph),
    ("line", Unit::Line),
    ("lines", Unit::Line),
    ("document", Unit::Document),
];

pub const DIRECTION_WORDS: &[(&str, Direction)] = &[
    ("previous", Direction::Previous),
    ("last", Direction::Previous),
    ("back", Direction::Previous),
    ("backward", Direction::Previous),
    ("backwards", Direction::Previous),
    ("next", Direction::Next),
    ("forward", Direction::Next),
    ("forwards", Direction::Next),
];

/// Which verb produces which op.
///
/// "extend selection" is deliberately the same op as "select": shift+motion
/// *extends* an existing selection natively, so the chords are identical. It
/// exists as a phrase because it is what people say.
///
/// "kill" is the same op as "delete", and exists for the length of the spoken
/// phrase rather than for the meaning. "delete previous word" is six syllables
/// for the most frequent edit there is; "kill last word" is three. Both stay,
/// since neither costs anything and people reach for different words under
/// frustration. It is safe as a bare verb because tier 1 requires a direction
/// *and* a unit — "kill the process" matches nothing here, and tier 2's
/// free-form patterns take `delete` and `select` but not `kill`, so no sentence
/// beginning with it can be swallowed as a command.
pub const VERB_OPS: &[(&str, EditOp)] = &[
    ("delete", EditOp::DeleteUnit),
    ("kill", EditOp::DeleteUnit),
    ("select", EditOp::SelectUnit),
    ("extend selection", EditOp::SelectUnit),
    ("move", EditOp::MoveUnit),
];

/// "beginning"/"end" name which end of the structure the caret goes to, mapping
/// onto the same `Direction` the motion rules use.
pub const EDGE_WORDS: &[(&str, Direction)] = &[
    ("beginning", Direction::Previous),
    ("start", Direction::Previous),
    ("end", Direction::Next),
];

/// Fixed phrases with no slots.
///
/// Only the "that" forms exist for case transforms: "uppercase &lt;phrase&gt;" would
/// need to read the field to know what it is transforming, while "that" is the
/// utterance govox just typed, which it already remembers.
pub const SIMPLE_EDITS: &[(&str, EditOp)] = &[
    ("undo that", EditOp::Undo),
    ("redo that", EditOp::Redo),
    ("delete that", EditOp::DeleteLast),
    ("scratch that", EditOp::DeleteLast),
    ("delete all", EditOp::DeleteAll),
    ("cut that", EditOp::Cut),
    ("copy that", EditOp::Copy),
    ("paste that", EditOp::Paste),
    ("select all", EditOp::SelectAll),
    ("select that", EditOp::SelectLast),
    ("deselect that", EditOp::Deselect),
    ("uppercase that", EditOp::UppercaseLast),
    ("lowercase that", EditOp::LowercaseLast),
    ("capitalize that", EditOp::CapitalizeLast),
];

/// Spoken key name → the chord name `keycodes::parse_chord` understands.
///
/// **Vetted, and that is the whole design.** `ydotool key` accepts a name,
/// exits 0, and presses nothing, so a grammar that passed spoken words through
/// would fail silently — the failure mode this project spends most of its
/// comments on. Every value here is a name present in `keycodes::KEYCODES`,
/// which is what makes the resulting chord translatable; a test asserts that
/// for every row, so adding one that is not translatable fails the build rather
/// than pressing nothing at runtime.
///
/// Left column is what a person says, right column is what the injector gets,
/// so several spellings can share a key. Keys the table cannot reach — letters
/// beyond the handful the editing chords use, function keys, space — are absent
/// on purpose: `KEYCODES` does not carry them, and inventing codes that cannot
/// be verified on this hardware is how a silent no-op gets shipped.
pub const PRESS_KEYS: &[(&str, &str)] = &[
    ("enter", "enter"),
    ("return", "enter"),
    ("tab", "tab"),
    ("escape", "esc"),
    ("esc", "esc"),
    ("backspace", "backspace"),
    ("delete", "delete"),
    ("insert", "insert"),
    ("home", "home"),
    ("end", "end"),
    ("page up", "pageup"),
    ("page down", "pagedown"),
    ("up", "up"),
    ("down", "down"),
    ("left", "left"),
    ("right", "right"),
    ("up arrow", "up"),
    ("down arrow", "down"),
    ("left arrow", "left"),
    ("right arrow", "right"),
];

/// Spoken modifier → the chord token `keycodes` knows it by.
///
/// "command" and "windows" map to `meta` because that is the key people mean by
/// them; the Linux name is neither.
pub const MODIFIER_WORDS: &[(&str, &str)] = &[
    ("control", "ctrl"),
    ("ctrl", "ctrl"),
    ("shift", "shift"),
    ("alt", "alt"),
    ("option", "alt"),
    ("command", "meta"),
    ("super", "meta"),
    ("meta", "meta"),
    ("windows", "meta"),
];

/// Keys reachable **only** with a modifier in front of them.
///
/// Letters, digits and function keys are here rather than in [`PRESS_KEYS`]
/// because a bare "press a" is a plausible thing to say — "press a button" is
/// not, but the whole-utterance "press a" is — and a single letter is the
/// easiest thing in the language for a recogniser to hear by mistake. Requiring
/// "press control a" costs nothing, since a lone letter keypress is not a thing
/// anyone dictates: they would just say the letter.
///
/// Function keys are here for a different reason: nothing is lost by requiring
/// the modifier form to be spelled out, and "press f 1" mishears easily.
pub const CHORD_KEYS: &[(&str, &str)] = &[
    ("a", "a"),
    ("b", "b"),
    ("c", "c"),
    ("d", "d"),
    ("e", "e"),
    ("f", "f"),
    ("g", "g"),
    ("h", "h"),
    ("i", "i"),
    ("j", "j"),
    ("k", "k"),
    ("l", "l"),
    ("m", "m"),
    ("n", "n"),
    ("o", "o"),
    ("p", "p"),
    ("q", "q"),
    ("r", "r"),
    ("s", "s"),
    ("t", "t"),
    ("u", "u"),
    ("v", "v"),
    ("w", "w"),
    ("x", "x"),
    ("y", "y"),
    ("z", "z"),
    ("0", "0"),
    ("1", "1"),
    ("2", "2"),
    ("3", "3"),
    ("4", "4"),
    ("5", "5"),
    ("6", "6"),
    ("7", "7"),
    ("8", "8"),
    ("9", "9"),
    ("f1", "f1"),
    ("f2", "f2"),
    ("f3", "f3"),
    ("f4", "f4"),
    ("f5", "f5"),
    ("f6", "f6"),
    ("f7", "f7"),
    ("f8", "f8"),
    ("f9", "f9"),
    ("f10", "f10"),
    ("f11", "f11"),
    ("f12", "f12"),
    ("space", "space"),
];

/// Longest first, so "characters" is tried before "character".
///
/// Alternation is ordered and relying on backtracking here is fragile. Python's
/// `sorted(key=len, reverse=True)` is stable, and `sort_by_key` is too, so
/// equal-length words keep table order.
fn alternation<'a, I: IntoIterator<Item = &'a str>>(words: I) -> String {
    let mut words: Vec<&str> = words.into_iter().collect();
    words.sort_by_key(|w| std::cmp::Reverse(w.len()));
    words.join("|")
}

static UNIT_MOTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    let number = format!(
        r"(?P<count>\d+|{})",
        alternation(NUMBER_WORDS.iter().map(|(w, _)| *w))
    );
    let unit = format!(
        r"(?P<unit>{})",
        alternation(UNIT_WORDS.iter().map(|(w, _)| *w))
    );
    let direction = format!(
        r"(?P<direction>{})",
        alternation(DIRECTION_WORDS.iter().map(|(w, _)| *w))
    );
    let verb = format!(
        r"(?P<verb>{})",
        alternation(VERB_OPS.iter().map(|(w, _)| *w))
    );
    Regex::new(&format!(r"^{verb} {direction}(?: {number})? {unit}$"))
        .expect("unit-motion pattern compiles")
});

/// `press <key>`, where `<key>` must be a name from [`PRESS_KEYS`].
///
/// Table-constrained, which is what makes a bare verb safe in tier 1: "press
/// the button" names no key and matches nothing, so it dictates as text.
static PRESS_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    let key = format!(
        r"(?P<key>{})",
        alternation(PRESS_KEYS.iter().map(|(spoken, _)| *spoken))
    );
    Regex::new(&format!(r"^press (?:the )?{key}(?: key)?$")).expect("press pattern compiles")
});

/// `press <modifier>… <key>`: at least one modifier, then one key.
///
/// Tried before [`PRESS_PATTERN`], because "press shift tab" must not be read as
/// the bare key "tab" with a stray word in front of it.
static PRESS_CHORD_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    let modifier = alternation(MODIFIER_WORDS.iter().map(|(spoken, _)| *spoken));
    // Both tables: a modifier makes "press control enter" as sensible as
    // "press control s".
    let key = alternation(
        PRESS_KEYS
            .iter()
            .chain(CHORD_KEYS.iter())
            .map(|(spoken, _)| *spoken),
    );
    Regex::new(&format!(
        r"^press (?:the )?(?P<mods>(?:(?:{modifier}) )+)(?P<key>{key})(?: key)?$"
    ))
    .expect("press-chord pattern compiles")
});

static MOVE_EDGE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    let edge = format!(
        r"(?P<edge>{})",
        alternation(EDGE_WORDS.iter().map(|(w, _)| *w))
    );
    let unit = format!(
        r"(?P<unit>{})",
        alternation(UNIT_WORDS.iter().map(|(w, _)| *w))
    );
    Regex::new(&format!(r"^move to {edge} of (?:the )?{unit}$"))
        .expect("move-edge pattern compiles")
});

/// Tier 2 patterns, in order.
///
/// "replace X with Y" must be tried before anything matching a bare
/// "replace X", and the two-slot patterns before the one-slot ones.
///
/// Case-insensitive because these are matched against the case-*preserving*
/// normalisation, where the leading verb may well have been sentence-cased into
/// "Replace" before it ever reached here.
static PHRASE_PATTERNS: LazyLock<Vec<(Regex, EditOp)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(r"(?i)^replace (?P<phrase>.+) with (?P<replacement>.+)$").unwrap(),
            EditOp::ReplacePhrase,
        ),
        (
            Regex::new(r"(?i)^move before (?P<phrase>.+)$").unwrap(),
            EditOp::MoveBeforePhrase,
        ),
        (
            Regex::new(r"(?i)^move after (?P<phrase>.+)$").unwrap(),
            EditOp::MoveAfterPhrase,
        ),
        (
            Regex::new(r"(?i)^select (?P<phrase>.+)$").unwrap(),
            EditOp::SelectPhrase,
        ),
        (
            Regex::new(r"(?i)^delete (?P<phrase>.+)$").unwrap(),
            EditOp::DeletePhrase,
        ),
    ]
});

#[must_use]
pub fn parse_count(token: Option<&str>) -> i64 {
    let Some(token) = token else { return 1 };
    if !token.is_empty() && token.chars().all(|c| c.is_ascii_digit()) {
        // Python's int() is unbounded; saturate rather than wrap on absurd input.
        return token.parse::<i64>().unwrap_or(i64::MAX);
    }
    number_word(token).unwrap_or(1)
}

fn lookup<T: Copy>(table: &[(&str, T)], key: &str) -> Option<T> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Tier 2 intents. Only called when command mode is active.
///
/// `text` is the **case-preserving** normalisation — punctuation stripped and
/// whitespace collapsed, but capitals intact. The two slots then part company:
///
/// - `phrase` names something to *find*, and is folded to lower case. Nothing
///   downstream cares: `find_phrase` searches case-insensitively either way.
/// - `replacement` is text to *type*, so it is kept exactly as spoken. Folding
///   it would make "replace rocky with Rocky" a no-op — and fixing a name is
///   the commonest reason to reach for the command at all.
#[must_use]
pub fn match_phrase_edit(text: &str) -> Option<EditAction> {
    for (pattern, op) in PHRASE_PATTERNS.iter() {
        let Some(caps) = pattern.captures(text) else {
            continue;
        };
        let phrase = caps.name("phrase").expect("phrase group").as_str().trim();
        if phrase.is_empty() {
            return None;
        }
        let phrase = phrase.to_lowercase();
        let replacement = caps
            .name("replacement")
            .map(|m| m.as_str().trim())
            .filter(|r| !r.is_empty())
            .map(str::to_owned);
        if *op == EditOp::ReplacePhrase && replacement.is_none() {
            return None;
        }
        return Some(EditAction {
            op: *op,
            unit: None,
            direction: None,
            count: 1,
            phrase: Some(phrase),
            replacement,
        });
    }
    None
}

/// The editing intent for `normalized`, or `None` to dictate it as text.
///
/// `normalized` must already have been through `normalize_command_text`.
#[must_use]
pub fn match_edit(normalized: &str) -> Option<EditAction> {
    if let Some(op) = lookup(SIMPLE_EDITS, normalized) {
        return Some(EditAction::simple(op));
    }

    // Before the bare form, so "press shift tab" is a chord rather than a key
    // with a word in front of it.
    if let Some(caps) = PRESS_CHORD_PATTERN.captures(normalized) {
        let spoken = caps.name("key").expect("key group").as_str();
        let key = lookup(PRESS_KEYS, spoken).or_else(|| lookup(CHORD_KEYS, spoken))?;
        // Modifiers in the order spoken, de-duplicated: "control control s" is
        // a mishearing, not a request to press it twice.
        let mut chord: Vec<&str> = Vec::new();
        for word in caps
            .name("mods")
            .expect("mods group")
            .as_str()
            .split_whitespace()
        {
            let token = lookup(MODIFIER_WORDS, word)?;
            if !chord.contains(&token) {
                chord.push(token);
            }
        }
        chord.push(key);
        return Some(EditAction {
            op: EditOp::PressKey,
            unit: None,
            direction: None,
            count: 1,
            phrase: Some(chord.join("+")),
            replacement: None,
        });
    }

    if let Some(caps) = PRESS_PATTERN.captures(normalized) {
        let spoken = caps.name("key").expect("key group").as_str();
        return Some(EditAction {
            op: EditOp::PressKey,
            unit: None,
            direction: None,
            count: 1,
            // Resolved here rather than at compile time, so the only string
            // that can reach the injector came out of the vetted table.
            phrase: Some(lookup(PRESS_KEYS, spoken)?.to_owned()),
            replacement: None,
        });
    }

    if let Some(caps) = UNIT_MOTION_PATTERN.captures(normalized) {
        let count = parse_count(caps.name("count").map(|m| m.as_str()));
        if count < 1 {
            return None;
        }
        return Some(EditAction {
            op: lookup(VERB_OPS, caps.name("verb").expect("verb").as_str()).expect("verb in table"),
            unit: lookup(UNIT_WORDS, caps.name("unit").expect("unit").as_str()),
            direction: lookup(
                DIRECTION_WORDS,
                caps.name("direction").expect("direction").as_str(),
            ),
            count,
            phrase: None,
            replacement: None,
        });
    }

    if let Some(caps) = MOVE_EDGE_PATTERN.captures(normalized) {
        return Some(EditAction {
            op: EditOp::MoveToEdge,
            unit: lookup(UNIT_WORDS, caps.name("unit").expect("unit").as_str()),
            direction: lookup(EDGE_WORDS, caps.name("edge").expect("edge").as_str()),
            count: 1,
            phrase: None,
            replacement: None,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spoken_key_resolves_to_a_translatable_chord() {
        // The point of the vetted table. `ydotool key` would accept an
        // untranslatable name, exit 0 and press nothing, so a row whose chord
        // is not in `KEYCODES` must fail here rather than at someone's caret.
        for (spoken, chord) in PRESS_KEYS {
            assert!(
                crate::keycodes::parse_chord(chord).is_ok(),
                "{spoken} → {chord:?} is not a translatable chord"
            );
        }
    }

    #[test]
    fn every_chord_key_and_modifier_is_translatable() {
        for (spoken, chord) in CHORD_KEYS {
            assert!(
                crate::keycodes::parse_chord(chord).is_ok(),
                "{spoken} → {chord:?} is not translatable"
            );
        }
        for (spoken, token) in MODIFIER_WORDS {
            assert!(
                crate::keycodes::parse_chord(&format!("{token}+a")).is_ok(),
                "{spoken} → {token:?} is not translatable"
            );
        }
    }

    #[test]
    fn modifiers_compose_in_the_order_spoken() {
        for (phrase, expected) in [
            ("press control s", "ctrl+s"),
            ("press ctrl s", "ctrl+s"),
            ("press command s", "meta+s"),
            ("press option f", "alt+f"),
            ("press control shift z", "ctrl+shift+z"),
            ("press shift tab", "shift+tab"),
            ("press alt tab", "alt+tab"),
            ("press control enter", "ctrl+enter"),
            ("press control f5", "ctrl+f5"),
            ("press control 1", "ctrl+1"),
            ("press super left", "meta+left"),
        ] {
            let edit = phrase_edit(phrase);
            assert_eq!(edit.op, EditOp::PressKey, "{phrase}");
            assert_eq!(edit.phrase.as_deref(), Some(expected), "{phrase}");
        }
    }

    #[test]
    fn a_repeated_modifier_is_a_mishearing_not_a_double_press() {
        assert_eq!(
            phrase_edit("press control control s").phrase.as_deref(),
            Some("ctrl+s")
        );
    }

    #[test]
    fn a_letter_needs_a_modifier_to_be_a_key() {
        // Bare "press a" stays text: a lone letter is the easiest thing to
        // mishear, and nobody dictates a single letter keypress.
        for phrase in ["press a", "press s", "press f1", "press 5", "press z"] {
            assert_eq!(match_edit(phrase), None, "{phrase} must dictate as text");
        }
        assert!(match_edit("press control a").is_some());
    }

    #[test]
    fn press_accepts_the_ways_a_key_gets_named() {
        for phrase in [
            "press enter",
            "press the enter",
            "press enter key",
            "press return",
        ] {
            let edit = phrase_edit(phrase);
            assert_eq!(edit.op, EditOp::PressKey, "{phrase}");
            assert_eq!(edit.phrase.as_deref(), Some("enter"), "{phrase}");
        }
    }

    #[test]
    fn spellings_collapse_onto_one_chord() {
        for (spoken, expected) in [
            ("press escape", "esc"),
            ("press esc", "esc"),
            ("press page down", "pagedown"),
            ("press down arrow", "down"),
            ("press down", "down"),
            ("press tab", "tab"),
        ] {
            assert_eq!(
                phrase_edit(spoken).phrase.as_deref(),
                Some(expected),
                "{spoken}"
            );
        }
    }

    #[test]
    fn press_needs_a_key_from_the_table() {
        // Tier 1 is always on, so an unvetted target must dictate as text
        // rather than reach an API that fails silently.
        for phrase in [
            "press",
            "press the button",
            "press on",
            "press f1",
            "press space",
            // "press command s" was here until modifier chords existed; it is
            // now `meta+s`, and `modifiers_compose_in_the_order_spoken` covers
            // it. What stays true is that its *bare* forms do not match.
            "press command",
            "press control",
        ] {
            assert_eq!(match_edit(phrase), None, "{phrase} must dictate as text");
        }
    }

    fn phrase_edit(phrase: &str) -> EditAction {
        match_edit(phrase).unwrap_or_else(|| panic!("{phrase} must parse"))
    }

    #[test]
    fn kill_is_delete_for_every_unit_the_grammar_knows() {
        for (phrase, unit) in [
            ("kill last character", Unit::Character),
            ("kill last word", Unit::Word),
            ("kill last sentence", Unit::Sentence),
        ] {
            let edit = match_edit(phrase).unwrap_or_else(|| panic!("{phrase} must parse"));
            assert_eq!(edit.op, EditOp::DeleteUnit, "{phrase}");
            assert_eq!(edit.unit, Some(unit), "{phrase}");
            assert_eq!(edit.direction, Some(Direction::Previous), "{phrase}");
            assert_eq!(edit.count, 1, "{phrase}");
        }
    }

    #[test]
    fn kill_and_delete_produce_the_identical_intent() {
        // The whole point of the alias: shorter to say, same op. If these ever
        // diverge, one of them compiles to keystrokes the other does not.
        for (short, long) in [
            ("kill last word", "delete previous word"),
            ("kill last three words", "delete previous three words"),
            ("kill next sentence", "delete next sentence"),
        ] {
            assert_eq!(match_edit(short), match_edit(long), "{short} vs {long}");
        }
    }

    #[test]
    fn kill_needs_a_direction_and_a_unit_so_ordinary_speech_is_left_alone() {
        // Tier 1 is always on — it does not wait for command mode — so a bare
        // verb must not be able to swallow a sentence. None of these name both
        // a direction and a unit.
        for phrase in [
            "kill",
            "kill it",
            "kill the process",
            "kill last",
            "kill the last word i said",
        ] {
            assert_eq!(match_edit(phrase), None, "{phrase} must dictate as text");
        }
    }

    #[test]
    fn the_replacement_keeps_the_capitals_that_were_spoken() {
        // The whole point: this command exists to fix a name, and a folded
        // replacement could never do it.
        let edit = match_phrase_edit("replace rocky with Rocky").expect("must parse");
        assert_eq!(edit.op, EditOp::ReplacePhrase);
        assert_eq!(edit.replacement.as_deref(), Some("Rocky"));
        // The phrase is a search key, not typed text, so it stays folded.
        assert_eq!(edit.phrase.as_deref(), Some("rocky"));
    }

    #[test]
    fn a_sentence_cased_verb_still_matches() {
        // `sentence_case` runs before this, so the utterance may well arrive as
        // "Replace …". Matching folds; only the slots keep their case.
        let edit = match_phrase_edit("Replace the old file with the New File").expect("must parse");
        assert_eq!(edit.phrase.as_deref(), Some("the old file"));
        assert_eq!(edit.replacement.as_deref(), Some("the New File"));
    }

    #[test]
    fn the_one_slot_verbs_still_fold_their_phrase() {
        for (spoken, expected) in [
            ("Delete The Old Draft", "the old draft"),
            ("Select The Heading", "the heading"),
            ("move before The Table", "the table"),
            ("move after The Table", "the table"),
        ] {
            let edit = match_phrase_edit(spoken).unwrap_or_else(|| panic!("{spoken} must parse"));
            assert_eq!(edit.phrase.as_deref(), Some(expected), "{spoken}");
            assert_eq!(edit.replacement, None, "{spoken}");
        }
    }

    #[test]
    fn kill_is_not_a_phrase_verb() {
        // Tier 2 takes `delete <phrase>` and `select <phrase>`; `kill` is
        // deliberately absent, so it cannot delete a free-form target.
        assert_eq!(match_phrase_edit("kill the old draft"), None);
    }
}
