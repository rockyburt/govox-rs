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
pub const VERB_OPS: &[(&str, EditOp)] = &[
    ("delete", EditOp::DeleteUnit),
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
static PHRASE_PATTERNS: LazyLock<Vec<(Regex, EditOp)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(r"^replace (?P<phrase>.+) with (?P<replacement>.+)$").unwrap(),
            EditOp::ReplacePhrase,
        ),
        (
            Regex::new(r"^move before (?P<phrase>.+)$").unwrap(),
            EditOp::MoveBeforePhrase,
        ),
        (
            Regex::new(r"^move after (?P<phrase>.+)$").unwrap(),
            EditOp::MoveAfterPhrase,
        ),
        (
            Regex::new(r"^select (?P<phrase>.+)$").unwrap(),
            EditOp::SelectPhrase,
        ),
        (
            Regex::new(r"^delete (?P<phrase>.+)$").unwrap(),
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
#[must_use]
pub fn match_phrase_edit(normalized: &str) -> Option<EditAction> {
    for (pattern, op) in PHRASE_PATTERNS.iter() {
        let Some(caps) = pattern.captures(normalized) else {
            continue;
        };
        let phrase = caps.name("phrase").expect("phrase group").as_str().trim();
        if phrase.is_empty() {
            return None;
        }
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
            phrase: Some(phrase.to_owned()),
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
