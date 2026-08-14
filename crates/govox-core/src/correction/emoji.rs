//! Spoken emoji: "thumbs up" → 👍. Off by default.
//!
//! Ported from `correction/emoji.py`. Deliberately short: every entry is a
//! phrase someone would only say when they mean the character. An open-ended
//! emoji vocabulary is exactly the false-positive problem this is shaped to
//! avoid, and two-word phrases only, for the same reason.
//!
//! Three entries are additions rather than ports — "sad face", "kissing face"
//! and "kiss face". They are recorded in the parity ledger; see the emoji rows
//! in `docs/parity.md`.
//!
//! Several values are multi-code-point: `❤️` and `⚠️` carry a U+FE0F variation
//! selector, so Python's `len()` reports 2 for them. That matters downstream —
//! the editor emits one backspace per code point — and `CharIdx` reproduces it.

use std::sync::LazyLock;

use fancy_regex::Regex;

use super::punctuation::{is_determiner, replace_all};

pub const SPOKEN_EMOJI: &[(&str, &str)] = &[
    ("smiley face", "🙂"),
    ("smiling face", "🙂"),
    ("winking face", "😉"),
    ("laughing face", "😂"),
    ("crying face", "😢"),
    ("frowning face", "🙁"),
    ("sad face", "🙁"),
    ("kissing face", "😘"),
    ("kiss face", "😘"),
    ("thinking face", "🤔"),
    ("thumbs up", "👍"),
    ("thumbs down", "👎"),
    ("red heart", "❤️"),
    ("broken heart", "💔"),
    ("party popper", "🎉"),
    ("fire emoji", "🔥"),
    ("check mark", "✅"),
    ("cross mark", "❌"),
    ("warning sign", "⚠️"),
    ("shrug emoji", "🤷"),
    ("rocket emoji", "🚀"),
    ("eyes emoji", "👀"),
    ("clapping hands", "👏"),
];

static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Longest first: alternation is ordered, so "smiling face" must be tried
    // before any phrase that is a prefix of it. Python's `sorted(key=len,
    // reverse=True)` is stable, so equal-length phrases keep table order —
    // `sort_by_key` is stable too.
    let mut phrases: Vec<&str> = SPOKEN_EMOJI.iter().map(|(p, _)| *p).collect();
    phrases.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let alternation = phrases
        .iter()
        .map(|p| fancy_regex::escape(p).into_owned())
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&format!(
        r"(?i)(?:(?P<prefix>\w+)\s+)?\b(?P<phrase>{alternation})\b"
    ))
    .expect("emoji pattern compiles")
});

fn lookup(phrase: &str) -> Option<&'static str> {
    let lowered = phrase.to_lowercase();
    SPOKEN_EMOJI
        .iter()
        .find(|(p, _)| *p == lowered)
        .map(|(_, e)| *e)
}

/// Replace spoken emoji phrases with their characters.
#[must_use]
pub fn apply_spoken_emoji(text: &str) -> String {
    replace_all(&PATTERN, text, |caps| {
        let prefix = caps.name("prefix").map(|m| m.as_str());
        if prefix.is_some_and(is_determiner) {
            // A noun phrase, not a spoken emoji.
            return caps.get(0).expect("whole match").as_str().to_owned();
        }
        let phrase = caps.name("phrase").expect("phrase group").as_str();
        let emoji = lookup(phrase).expect("matched phrase is in the table");
        match prefix {
            Some(prefix) => format!("{prefix} {emoji}"),
            None => emoji.to_owned(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::apply_spoken_emoji;

    // The entries added beyond govox-py's table. The golden corpus is generated
    // by running govox-py, so it can never cover these — they are pinned here
    // or nowhere.

    #[test]
    fn sad_face_is_the_frowning_face() {
        assert_eq!(apply_spoken_emoji("sad face"), "🙁");
        // Same character as the phrase it sits beside, which is the point:
        // people say either and mean one thing.
        assert_eq!(
            apply_spoken_emoji("frowning face"),
            apply_spoken_emoji("sad face")
        );
    }

    #[test]
    fn both_kiss_phrasings_work() {
        assert_eq!(apply_spoken_emoji("kissing face"), "😘");
        assert_eq!(apply_spoken_emoji("kiss face"), "😘");
    }

    #[test]
    fn the_new_phrases_keep_a_leading_word() {
        assert_eq!(apply_spoken_emoji("well done kiss face"), "well done 😘");
    }

    #[test]
    fn a_determiner_still_blocks_the_new_phrases() {
        // "a sad face" is someone describing a face, not dictating an emoji —
        // the same guard every other entry gets.
        assert_eq!(apply_spoken_emoji("a sad face"), "a sad face");
        assert_eq!(apply_spoken_emoji("the kissing face"), "the kissing face");
    }
}
