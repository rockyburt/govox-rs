//! Sentence and paragraph boundaries, computed from a field snapshot.
//!
//! Ported from `editing/spans.py`.
//!
//! **Every offset here is a character offset**, matching the reference and
//! AT-SPI's own units. Rust's regex reports byte offsets, so each is converted;
//! mixing the two would emit the wrong number of keystrokes into the user's
//! document the moment any non-ASCII text is involved.

use std::sync::LazyLock;

use regex::Regex;

use crate::domain::Unit;

/// A sentence ends at terminal punctuation plus the whitespace that follows.
/// Quotes and brackets may close after the mark: `He said "go!" Then left.`
static SENTENCE_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[.!?…]+["')\]]*\s+"#).unwrap());

/// A paragraph break is a blank line. A single newline is a line break, which
/// `Unit::Line` already handles with home/end.
static PARAGRAPH_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n\s*").unwrap());

/// The units this module can compute. Others have no boundary notion here.
#[must_use]
pub fn is_supported(unit: Unit) -> bool {
    matches!(unit, Unit::Sentence | Unit::Paragraph)
}

fn pattern_for(unit: Unit) -> Option<&'static Regex> {
    match unit {
        Unit::Sentence => Some(&SENTENCE_END),
        Unit::Paragraph => Some(&PARAGRAPH_END),
        _ => None,
    }
}

/// Character offsets where a unit starts, always including 0 and the length.
///
/// The end of the text counts as a boundary so "delete next sentence" works on
/// a final sentence with no terminator — which is most of what someone is
/// editing, since they just dictated it.
///
/// # Panics
///
/// If `unit` is not supported; callers check [`is_supported`] first, as the
/// reference's `_PATTERNS[unit]` lookup does implicitly.
#[must_use]
pub fn boundaries(text: &str, unit: Unit) -> Vec<usize> {
    let pattern = pattern_for(unit).expect("unit has a boundary pattern");
    let char_len = text.chars().count();

    let mut offsets = vec![0usize];
    for found in pattern.find_iter(text) {
        offsets.push(byte_to_char(text, found.end()));
    }
    if offsets.last() != Some(&char_len) {
        offsets.push(char_len);
    }
    offsets
}

/// Characters from `caret` back to the `count`-th boundary before it.
///
/// `None` when there is no such boundary — the caret is at or before the first
/// one, and there is nothing to act on.
#[must_use]
pub fn distance_back(text: &str, caret: usize, unit: Unit, count: usize) -> Option<usize> {
    let earlier: Vec<usize> = boundaries(text, unit)
        .into_iter()
        .filter(|offset| *offset < caret)
        .collect();
    if earlier.len() < count || count == 0 {
        return None;
    }
    Some(caret - earlier[earlier.len() - count])
}

/// Characters from `caret` forward to the `count`-th boundary after it.
#[must_use]
pub fn distance_forward(text: &str, caret: usize, unit: Unit, count: usize) -> Option<usize> {
    let later: Vec<usize> = boundaries(text, unit)
        .into_iter()
        .filter(|offset| *offset > caret)
        .collect();
    if later.len() < count || count == 0 {
        return None;
    }
    Some(later[count - 1] - caret)
}

/// Locate `phrase` in `text`, preferring the occurrence nearest the caret.
///
/// **Behind the caret wins.** Someone saying "replace teh with the" is almost
/// always fixing something they just dictated, which is behind them; a match
/// further forward in a long document would be a surprise they cannot see. Only
/// when there is nothing behind does the search look ahead.
///
/// Matching is case-insensitive because the recognizer capitalizes
/// sentence-initially on its own — the speaker did not choose that capital and
/// should not have to reproduce it. Whitespace inside the phrase is collapsed
/// for the same reason: what was heard as one space may be a line break.
///
/// Returns `(start, end)` as **character** offsets.
#[must_use]
pub fn find_phrase(text: &str, caret: usize, phrase: &str) -> Option<(usize, usize)> {
    if phrase.is_empty() {
        return None;
    }
    let words: Vec<String> = phrase.split_whitespace().map(regex::escape).collect();
    if words.is_empty() {
        // Whitespace-only phrase: Python's "".join over an empty split gives an
        // empty pattern, which matches at every position.
        return Some((0, 0));
    }
    let pattern = Regex::new(&format!("(?i){}", words.join(r"\s+"))).ok()?;

    let matches: Vec<(usize, usize)> = pattern
        .find_iter(text)
        .map(|m| (byte_to_char(text, m.start()), byte_to_char(text, m.end())))
        .collect();

    // Strictly behind the caret, nearest first.
    if let Some(found) = matches.iter().rev().find(|(_, end)| *end <= caret) {
        return Some(*found);
    }
    // Then the first match starting at or after the caret.
    if let Some(found) = matches.iter().find(|(start, _)| *start >= caret) {
        return Some(*found);
    }
    // Nothing on either side in the strict sense — but a phrase straddling the
    // caret is still the one that was meant.
    matches.first().copied()
}

/// Convert a byte offset into a character offset.
fn byte_to_char(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset].chars().count()
}
