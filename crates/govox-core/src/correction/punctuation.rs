//! Spoken punctuation: "hello comma world period" → "Hello, world."
//!
//! Ported from `correction/punctuation.py`. Not a command in the grammar sense:
//! editing commands match a whole utterance, punctuation words appear inline,
//! so this is a token-level rewrite.
//!
//! Whisper already punctuates from prosody, so this layer only handles the case
//! where the user says the punctuation *word* and expects the mark — which is
//! also why both the mark before and the mark after the spoken word are
//! absorbed. Otherwise "hello period" ("Hello. Period.") would render "Hello..".
//!
//! The pattern needs a **negative lookahead**, which the `regex` crate cannot
//! do, so this module uses `fancy-regex`. Backtracking is irrelevant here: the
//! input is one utterance.

use std::sync::LazyLock;

use fancy_regex::{Captures, Regex};

/// Which side of the mark absorbs the surrounding whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    /// Closes up against the *preceding* word: "hello comma" → "hello,".
    Left,
    /// Belongs to the word *after* it: "open quote hello" → `"hello`.
    Right,
    /// Closes up on both sides: "well hyphen known" → "well-known".
    Tight,
    /// A line break. Like `Tight` it takes no space, but unlike every other
    /// mark it *keeps* preceding auto-punctuation: "Hello. New line world" is
    /// "Hello.\nworld". The sentence really did end; the break is not replacing
    /// its full stop.
    Break,
}

/// Spoken phrase → (mark, attachment).
///
/// **Order is significant and must match the reference's dict order**: the
/// pattern's alternation is built in this order and alternation is ordered.
/// Prefix-overlapping phrases ("open paren" / "open parenthesis") are safe
/// because of the trailing `\b`, but the two break phrases rely on ordering.
pub const SPOKEN_PUNCTUATION: &[(&str, &str, Attach)] = &[
    ("exclamation mark", "!", Attach::Left),
    ("exclamation point", "!", Attach::Left),
    ("question mark", "?", Attach::Left),
    ("full stop", ".", Attach::Left),
    ("semicolon", ";", Attach::Left),
    ("ellipsis", "…", Attach::Left),
    ("period", ".", Attach::Left),
    ("comma", ",", Attach::Left),
    ("colon", ":", Attach::Left),
    ("hyphen", "-", Attach::Tight),
    ("dash", "—", Attach::Tight),
    // Deliberately no bare "quote": it is an everyday verb and noun and the
    // determiner guard cannot see far enough back to tell them apart, so the
    // opener must be "open quote". "unquote" is safe bare — not a word alone.
    ("open quote", "\"", Attach::Right),
    ("close quote", "\"", Attach::Left),
    ("unquote", "\"", Attach::Left),
    ("open parenthesis", "(", Attach::Right),
    ("open paren", "(", Attach::Right),
    ("close parenthesis", ")", Attach::Left),
    ("close paren", ")", Attach::Left),
    ("open bracket", "[", Attach::Right),
    ("close bracket", "]", Attach::Left),
    ("new paragraph", "\n\n", Attach::Break),
    ("new line", "\n", Attach::Break),
];

/// When one of these immediately precedes the word, it is a noun ("add a comma
/// here"), not a spoken mark.
///
/// Demonstratives are deliberately absent: "what is this question mark" means
/// "What is this?", so guarding on "this" would suppress the common case.
pub const DETERMINERS: &[&str] = &["a", "an", "the", "my", "your", "its", "another"];

/// A newline ends a sentence for casing purposes as surely as a full stop.
pub const TERMINATORS: &[char] = &['.', '!', '?', '…', '\n'];

const MARKS: &str = ".!?…,;:";

pub(crate) fn is_determiner(word: &str) -> bool {
    let lowered = word.to_lowercase();
    DETERMINERS.contains(&lowered.as_str())
}

fn lookup(phrase: &str) -> Option<(&'static str, Attach)> {
    let lowered = phrase.to_lowercase();
    SPOKEN_PUNCTUATION
        .iter()
        .find(|(name, _, _)| *name == lowered)
        .map(|(_, mark, attach)| (*mark, *attach))
}

static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // "open"/"close" must not be swallowed as an ordinary prefix word when they
    // open a paired-mark phrase, or "close quote" parses as prefix="close",
    // phrase="quote" — dropping the prefix and losing the no-space attachment.
    // Derived from the table, so a future "open …" phrase is guarded too.
    let paired: Vec<String> = SPOKEN_PUNCTUATION
        .iter()
        .filter(|(p, _, _)| p.starts_with("open ") || p.starts_with("close "))
        .map(|(p, _, _)| fancy_regex::escape(p.split_once(' ').unwrap().1).into_owned())
        .collect();
    // frozenset in the reference, so duplicates collapse; do the same.
    let mut unique: Vec<String> = Vec::new();
    for word in paired {
        if !unique.contains(&word) {
            unique.push(word);
        }
    }
    let guard = format!(r"(?:open|close)\s+(?:{})\b", unique.join("|"));

    let phrases: Vec<String> = SPOKEN_PUNCTUATION
        .iter()
        .map(|(p, _, _)| fancy_regex::escape(p).into_owned())
        .collect();

    let marks = fancy_regex::escape(MARKS);
    let source = format!(
        r"(?i)(?:(?P<lead>[{marks}])\s*|(?!{guard})(?P<prefix>\w+)\s+)?\b(?P<phrase>{})\b(?P<tail>\s*[{marks}])?(?P<suffix>\s+)?",
        phrases.join("|"),
    );
    Regex::new(&source).expect("punctuation pattern compiles")
});

/// Replace spoken punctuation words with their marks.
#[must_use]
pub fn apply_spoken_punctuation(text: &str) -> String {
    replace_all(&PATTERN, text, |caps| {
        let prefix = caps.name("prefix").map(|m| m.as_str());
        let phrase = caps
            .name("phrase")
            .expect("phrase group always matches")
            .as_str();
        let suffix = caps.name("suffix").map_or("", |m| m.as_str());
        let lead_mark = caps.name("lead").map(|m| m.as_str());

        let (mark, attach) = lookup(phrase).expect("matched phrase is in the table");

        if prefix.is_some_and(is_determiner) {
            // A noun, not a spoken mark. Return the match untouched.
            return caps.get(0).expect("whole match").as_str().to_owned();
        }

        // `lead` and `tail` are Whisper's auto-punctuation around the spoken
        // word; dropping them is what stops ".." and ". ." forming.
        let (lead, trail) = match attach {
            Attach::Right => {
                // The mark belongs to the word after it: keep the space that
                // separated the prefix, drop the trailing one. When
                // auto-punctuation was absorbed instead ("he said. Open quote
                // hello") the mark goes but its space stays, or the opener
                // glues to the previous word.
                let lead = if let Some(prefix) = prefix {
                    format!("{prefix} ")
                } else if lead_mark.is_some() {
                    " ".to_owned()
                } else {
                    String::new()
                };
                (lead, "")
            }
            Attach::Break => {
                // Keep whatever preceded, auto-punctuation included: the
                // sentence before a break genuinely ended.
                let lead = prefix.map_or_else(|| lead_mark.unwrap_or("").to_owned(), str::to_owned);
                (lead, "")
            }
            Attach::Tight => (prefix.unwrap_or("").to_owned(), ""),
            Attach::Left => (prefix.unwrap_or("").to_owned(), suffix),
        };
        format!("{lead}{mark}{trail}")
    })
}

/// `Regex::replace_all` with a closure, for `fancy-regex`.
///
/// fancy-regex has no closure-taking `replace_all`, so this walks the matches.
pub(crate) fn replace_all<F>(pattern: &Regex, text: &str, mut render: F) -> String
where
    F: FnMut(&Captures<'_, str>) -> String,
{
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in pattern.captures_iter(text).flatten() {
        let whole = caps.get(0).expect("whole match");
        // Zero-width matches would loop forever and cannot rewrite anything.
        if whole.start() == whole.end() {
            continue;
        }
        out.push_str(&text[last..whole.start()]);
        out.push_str(&render(&caps));
        last = whole.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Capitalize the first letter of each sentence after `.`, `!`, `?`, `…`, `\n`.
///
/// Without this, "hello period world period" renders "Hello. world." — the
/// spoken full stop creates a boundary nothing else capitalizes.
#[must_use]
pub fn capitalize_after_terminators(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut capitalize_next = false;
    for char in text.chars() {
        if capitalize_next && char.is_alphabetic() {
            // to_uppercase can yield several chars (ß → SS), matching Python.
            out.extend(char.to_uppercase());
            capitalize_next = false;
            continue;
        }
        if TERMINATORS.contains(&char) {
            capitalize_next = true;
        }
        out.push(char);
    }
    out
}
