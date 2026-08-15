//! Spoken case control: "all caps hello" → "HELLO".
//!
//! An addition rather than a port; there is no counterpart in the reference.
//! Modelled on macOS Dictation, which is govox's UX target wherever parity does
//! not bind. See the case-control row in `docs/parity.md`.
//!
//! Two shapes, matching what macOS accepts:
//!
//! | Said | Effect |
//! |---|---|
//! | `all caps <word>` | that one word in capitals |
//! | `all caps on` … `all caps off` | every word between them in capitals |
//! | `caps <word>` / `caps on` … `caps off` | first letter capitalised |
//! | `no caps <word>` / `no caps on` … `no caps off` | forced lower case |
//!
//! ## Why this runs last, and why it is opt-in
//!
//! It runs after every other stage because `sentence_case` and
//! `capitalize_after_terminators` only ever *add* capitals. A "no caps" applied
//! before them would simply be undone at the start of a sentence, which is
//! exactly where someone is most likely to want it.
//!
//! It is off by default because "caps" is an ordinary English word — bottle
//! caps, knee caps, caps lock — and a marker that fires inside prose does not
//! merely fail, it silently eats the word after it. The same reasoning keeps
//! `spoken_emoji` and `number_formatting` off.
//!
//! ## Within one utterance only
//!
//! A span opened with "all caps on" closes at the end of the utterance whether
//! or not "all caps off" was said. Carrying the mode across utterances would
//! mean a daemon-level flag that outlives the sentence that set it, and a user
//! who forgets to close it types in capitals until they work out why — the same
//! trap `[editing] command_mode` is opt-in to avoid.

use std::sync::LazyLock;

use regex::Regex;

/// What a marker does to the words it governs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `all caps` — every character.
    Upper,
    /// `no caps` — every character.
    Lower,
    /// `caps` — the first character only. Like [`super::sentence_case`] it does
    /// not touch the rest of the word, so an acronym the model already got
    /// right survives.
    Title,
}

/// The spoken markers, longest-first so "all caps" wins over "caps".
///
/// Alternation is ordered, so this order is load-bearing: with "caps" first,
/// "all caps on" would parse as the word "all" followed by `caps on`.
pub const CASE_MARKERS: &[(&str, Mode)] = &[
    ("all caps", Mode::Upper),
    ("no caps", Mode::Lower),
    ("caps", Mode::Title),
];

/// The switch words that turn a marker into a span rather than a one-shot.
pub const SWITCH_WORDS: &[&str] = &["on", "off"];

static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    let kinds = CASE_MARKERS
        .iter()
        .map(|(phrase, _)| regex::escape(phrase))
        .collect::<Vec<_>>()
        .join("|");
    let switches = SWITCH_WORDS.join("|");
    Regex::new(&format!(
        r"(?i)\b(?P<kind>{kinds})(?:\s+(?P<switch>{switches}))?\b"
    ))
    .expect("case-marker pattern compiles")
});

enum Event {
    /// `Some` opens a span, `None` closes whatever was open.
    Span(Option<Mode>),
    /// Applies to the next word only.
    OneShot(Mode),
}

fn lookup(kind: &str) -> Mode {
    let lowered = kind.to_lowercase();
    CASE_MARKERS
        .iter()
        .find(|(phrase, _)| *phrase == lowered)
        .map(|(_, mode)| *mode)
        .expect("matched kind is in the table")
}

fn recase(word: &str, mode: Mode) -> String {
    match mode {
        Mode::Upper => word.to_uppercase(),
        Mode::Lower => word.to_lowercase(),
        Mode::Title => {
            let mut out = String::with_capacity(word.len());
            let mut chars = word.chars();
            match chars.find(|c| c.is_alphabetic()) {
                // Rebuild around the first alphabetic character, so a leading
                // quote or bracket does not absorb the capital.
                Some(first) => {
                    let index = word
                        .char_indices()
                        .find(|(_, c)| c.is_alphabetic())
                        .map(|(i, _)| i)
                        .expect("just found one");
                    out.push_str(&word[..index]);
                    out.extend(first.to_uppercase());
                    out.push_str(&word[index + first.len_utf8()..]);
                }
                None => out.push_str(word),
            }
            out
        }
    }
}

/// Byte offset and text of each run of non-whitespace.
fn word_spans(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (index, char) in text.char_indices() {
        match (char.is_whitespace(), start) {
            (false, None) => start = Some(index),
            (true, Some(begin)) => {
                out.push((begin, &text[begin..index]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        out.push((begin, &text[begin..]));
    }
    out
}

/// Apply spoken case control, removing the markers themselves.
///
/// Whitespace is preserved exactly, newlines included: a "new line" that the
/// punctuation stage already turned into a break must survive this.
#[must_use]
pub fn apply_case_control(text: &str) -> String {
    // Strip the markers first, recording where each one stood in the text that
    // is left. Doing it in one pass would mean deciding a word's case before
    // knowing whether a later marker governs it.
    let mut cleaned = String::with_capacity(text.len());
    let mut events: Vec<(usize, Event)> = Vec::new();
    let mut last = 0;

    for caps in MARKER.captures_iter(text) {
        let whole = caps.get(0).expect("whole match");
        cleaned.push_str(&text[last..whole.start()]);

        let mode = lookup(caps.name("kind").expect("kind group").as_str());
        let event = match caps.name("switch").map(|m| m.as_str().to_lowercase()) {
            Some(switch) if switch == "on" => Event::Span(Some(mode)),
            Some(_) => Event::Span(None),
            None => Event::OneShot(mode),
        };
        events.push((cleaned.len(), event));

        last = whole.end();
        // A marker sits between two spaces and only one of them may survive.
        // Prefer to swallow the one after it; when there is none — the marker
        // ended the utterance, or ran up against a break — swallow the one in
        // front instead. Leaving either behind puts a stray space in the
        // document, since `normalize_spacing` has already run by this stage.
        if text[last..].starts_with(' ') {
            last += 1;
        } else if cleaned.ends_with(' ') {
            cleaned.pop();
        }
    }

    if events.is_empty() {
        return text.to_owned();
    }
    cleaned.push_str(&text[last..]);

    let mut out = String::with_capacity(cleaned.len());
    let mut span: Option<Mode> = None;
    let mut one_shot: Option<Mode> = None;
    let mut cursor = 0;
    let mut events = events.into_iter().peekable();

    for (start, word) in word_spans(&cleaned) {
        out.push_str(&cleaned[cursor..start]);
        while events.peek().is_some_and(|(position, _)| *position <= start) {
            match events.next().expect("just peeked").1 {
                Event::Span(mode) => span = mode,
                Event::OneShot(mode) => one_shot = Some(mode),
            }
        }
        match one_shot.take().or(span) {
            Some(mode) => out.push_str(&recase(word, mode)),
            None => out.push_str(word),
        }
        cursor = start + word.len();
    }
    out.push_str(&cleaned[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::apply_case_control as cased;

    #[test]
    fn one_shot_governs_exactly_one_word() {
        assert_eq!(cased("all caps hello world"), "HELLO world");
        assert_eq!(cased("say all caps hello there"), "say HELLO there");
    }

    #[test]
    fn a_span_runs_until_it_is_closed() {
        assert_eq!(
            cased("all caps on hello there all caps off world"),
            "HELLO THERE world"
        );
    }

    #[test]
    fn an_unclosed_span_runs_to_the_end_of_the_utterance() {
        assert_eq!(cased("all caps on hello there"), "HELLO THERE");
    }

    #[test]
    fn no_caps_forces_lower_case() {
        assert_eq!(cased("no caps Hello"), "hello");
        assert_eq!(cased("no caps on Hello There"), "hello there");
    }

    #[test]
    fn caps_titles_the_first_letter_and_leaves_the_rest() {
        assert_eq!(cased("caps hello"), "Hello");
        // An acronym the model already got right is not flattened.
        assert_eq!(cased("caps NASA"), "NASA");
    }

    #[test]
    fn caps_on_titles_every_word_in_the_span() {
        assert_eq!(cased("caps on this is a title caps off"), "This Is A Title");
    }

    /// The longest marker has to win, or "all caps on" parses as the word "all"
    /// followed by a `caps on` span.
    #[test]
    fn all_caps_beats_caps() {
        assert_eq!(cased("all caps on a b all caps off"), "A B");
    }

    #[test]
    fn text_with_no_marker_is_returned_untouched() {
        let text = "the bottle stayed where it was";
        assert_eq!(cased(text), text);
    }

    /// A break the punctuation stage produced must survive the rewrite.
    #[test]
    fn newlines_are_preserved() {
        assert_eq!(cased("all caps on one\ntwo"), "ONE\nTWO");
        assert_eq!(cased("no marker\nhere"), "no marker\nhere");
    }

    #[test]
    fn a_marker_can_end_the_utterance_without_eating_anything() {
        assert_eq!(cased("hello all caps"), "hello");
    }

    #[test]
    fn the_switch_words_are_case_insensitive() {
        assert_eq!(cased("ALL CAPS ON hello ALL CAPS OFF there"), "HELLO there");
    }
}
