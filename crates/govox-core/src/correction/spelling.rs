//! Letter-by-letter entry, for the words recognition will never get right.
//!
//! Taken from macOS Voice Control's spelling mode. It exists because some
//! strings are not words: an identifier, a licence plate, a surname the model
//! has never seen. Dictating those is a losing game however good the recogniser
//! is, and the personal dictionary only helps for terms you can predict.
//!
//! **The alphabet is phonetic first.** Bare spoken letters are the single worst
//! case for a speech model — "b", "d", "e", "g", "p", "t", "v" are barely
//! distinguishable to a human on a phone line, let alone to whisper on a laptop
//! microphone — so `alpha bravo charlie` is the form that actually works, and
//! the one the listing shows. Bare letters and their common written-out
//! spellings are accepted too, because people say them and refusing would be
//! pedantry, but they are the unreliable path.

/// Spoken token → the character it produces.
///
/// Order is irrelevant: this is a lookup on whole whitespace-separated tokens,
/// never a regex, so nothing here can partially match a longer word.
pub const SPELLING_ALPHABET: &[(&str, char)] = &[
    // NATO, the reliable path.
    ("alpha", 'a'),
    ("bravo", 'b'),
    ("charlie", 'c'),
    ("delta", 'd'),
    ("echo", 'e'),
    ("foxtrot", 'f'),
    ("golf", 'g'),
    ("hotel", 'h'),
    ("india", 'i'),
    ("juliet", 'j'),
    ("juliett", 'j'),
    ("kilo", 'k'),
    ("lima", 'l'),
    ("mike", 'm'),
    ("november", 'n'),
    ("oscar", 'o'),
    ("papa", 'p'),
    ("quebec", 'q'),
    ("romeo", 'r'),
    ("sierra", 's'),
    ("tango", 't'),
    ("uniform", 'u'),
    ("victor", 'v'),
    ("whiskey", 'w'),
    ("whisky", 'w'),
    ("xray", 'x'),
    ("x-ray", 'x'),
    ("yankee", 'y'),
    ("zulu", 'z'),
    // Bare letters, as whisper writes them when it hears one cleanly.
    ("a", 'a'),
    ("b", 'b'),
    ("c", 'c'),
    ("d", 'd'),
    ("e", 'e'),
    ("f", 'f'),
    ("g", 'g'),
    ("h", 'h'),
    ("i", 'i'),
    ("j", 'j'),
    ("k", 'k'),
    ("l", 'l'),
    ("m", 'm'),
    ("n", 'n'),
    ("o", 'o'),
    ("p", 'p'),
    ("q", 'q'),
    ("r", 'r'),
    ("s", 's'),
    ("t", 't'),
    ("u", 'u'),
    ("v", 'v'),
    ("w", 'w'),
    ("x", 'x'),
    ("y", 'y'),
    ("z", 'z'),
    // The spellings whisper reaches for when it writes the *sound* of a letter
    // rather than the letter. Each of these was chosen because it is what a
    // transcript actually contains, not because it is how one would spell it.
    ("bee", 'b'),
    ("cee", 'c'),
    ("dee", 'd'),
    ("eff", 'f'),
    ("gee", 'g'),
    ("aitch", 'h'),
    ("haitch", 'h'),
    ("jay", 'j'),
    ("kay", 'k'),
    ("el", 'l'),
    ("ell", 'l'),
    ("em", 'm'),
    ("en", 'n'),
    ("oh", 'o'),
    ("pee", 'p'),
    ("cue", 'q'),
    ("queue", 'q'),
    ("ar", 'r'),
    ("are", 'r'),
    ("ess", 's'),
    ("tee", 't'),
    ("vee", 'v'),
    ("ex", 'x'),
    ("wye", 'y'),
    ("zed", 'z'),
    ("zee", 'z'),
    // Digits. "zero" and "oh" both occur; "oh" is already the letter, and a
    // digit zero is said "zero" far more often than a letter O is said "zero".
    ("zero", '0'),
    ("one", '1'),
    ("two", '2'),
    ("three", '3'),
    ("four", '4'),
    ("five", '5'),
    ("six", '6'),
    ("seven", '7'),
    ("eight", '8'),
    ("nine", '9'),
    // The separators an identifier actually needs. Not the full punctuation
    // table: spelling mode is for strings, and a comma in one is rare enough
    // that leaving it out costs less than a false positive would.
    ("space", ' '),
    ("dot", '.'),
    ("period", '.'),
    ("dash", '-'),
    ("hyphen", '-'),
    ("underscore", '_'),
    ("slash", '/'),
    ("at", '@'),
    ("colon", ':'),
];

/// Words that uppercase the letter that follows them.
///
/// A prefix rather than a mode, for the same reason the case markers elsewhere
/// are: a sticky shift is a state to get stuck in, and spelling is already a
/// mode. `capital alpha bravo` is `Ab`, not `AB`.
pub const CAPITAL_WORDS: &[&str] = &["capital", "cap", "uppercase"];

/// What one spelled utterance produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spelled {
    /// The characters, ready to type.
    pub text: String,
    /// Tokens that named no letter, in the order they were said.
    ///
    /// Kept rather than dropped so the caller can say what it did not
    /// understand. Spelling mode exists for strings that must be exact, so
    /// guessing at a token would defeat the point of using it.
    pub unrecognised: Vec<String>,
}

/// Convert one utterance of spelled tokens.
///
/// `text` should already be normalised — lower case, punctuation stripped —
/// which is what `normalize_command_text` produces.
///
/// Returns `None` for an utterance with nothing spellable in it at all, which
/// the caller reports rather than typing: in this mode a stray sentence is a
/// misrecognition, not dictation.
#[must_use]
pub fn spell(text: &str) -> Option<Spelled> {
    let mut out = String::new();
    let mut unrecognised = Vec::new();
    let mut capitalise_next = false;
    let mut recognised_any = false;

    for token in text.split_whitespace() {
        if CAPITAL_WORDS.contains(&token) {
            capitalise_next = true;
            recognised_any = true;
            continue;
        }
        match lookup(token) {
            Some(character) => {
                recognised_any = true;
                if capitalise_next {
                    out.extend(character.to_uppercase());
                    capitalise_next = false;
                } else {
                    out.push(character);
                }
            }
            None => unrecognised.push(token.to_owned()),
        }
    }

    // A trailing "capital" with nothing after it is a truncated utterance, not
    // a character; it counts as recognised but produces nothing.
    if !recognised_any {
        return None;
    }
    Some(Spelled {
        text: out,
        unrecognised,
    })
}

fn lookup(token: &str) -> Option<char> {
    SPELLING_ALPHABET
        .iter()
        .find(|(spoken, _)| *spoken == token)
        .map(|(_, character)| *character)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spelled(text: &str) -> String {
        spell(text).expect("must spell").text
    }

    #[test]
    fn the_phonetic_alphabet_spells() {
        assert_eq!(spelled("alpha bravo charlie"), "abc");
        assert_eq!(spelled("romeo oscar charlie kilo yankee"), "rocky");
    }

    #[test]
    fn bare_letters_and_their_written_sounds_both_work() {
        assert_eq!(spelled("a b c"), "abc");
        assert_eq!(spelled("bee ee"), "b");
        assert_eq!(spelled("jay kay el"), "jkl");
    }

    #[test]
    fn capital_applies_to_one_letter_only() {
        // A sticky shift would be a mode inside a mode.
        assert_eq!(spelled("capital alpha bravo"), "Ab");
        assert_eq!(spelled("capital romeo capital bravo"), "RB");
        assert_eq!(spelled("cap alpha"), "A");
    }

    #[test]
    fn digits_and_separators_are_there_because_identifiers_need_them() {
        assert_eq!(spelled("alpha dash one two"), "a-12");
        assert_eq!(spelled("romeo underscore nine"), "r_9");
        assert_eq!(spelled("alpha at bravo dot charlie"), "a@b.c");
    }

    #[test]
    fn what_it_did_not_understand_is_reported_rather_than_guessed() {
        // The whole point of the mode is exactness; a guess defeats it.
        let out = spell("alpha wibble bravo").expect("partial still spells");
        assert_eq!(out.text, "ab");
        assert_eq!(out.unrecognised, ["wibble"]);
    }

    #[test]
    fn an_utterance_with_nothing_spellable_is_not_spelling() {
        assert_eq!(spell("the quick brown fox jumped"), None);
        assert_eq!(spell(""), None);
    }

    #[test]
    fn every_letter_of_the_alphabet_is_reachable_phonetically() {
        // The reliable path must be complete: a missing letter would send
        // someone back to bare letters for that one character.
        for letter in 'a'..='z' {
            assert!(
                SPELLING_ALPHABET
                    .iter()
                    .any(|(spoken, produced)| *produced == letter && spoken.len() > 1),
                "no phonetic word produces {letter}"
            );
        }
    }

    #[test]
    fn no_spoken_token_produces_two_different_characters() {
        // A duplicate key would make the table order load-bearing, and the
        // first entry would silently win.
        for (spoken, character) in SPELLING_ALPHABET {
            let all: Vec<char> = SPELLING_ALPHABET
                .iter()
                .filter(|(other, _)| other == spoken)
                .map(|(_, c)| *c)
                .collect();
            assert!(
                all.iter().all(|c| c == character),
                "{spoken:?} maps to {all:?}"
            );
        }
    }

    #[test]
    fn a_capital_word_is_never_also_a_letter() {
        // "cap" must not be both "uppercase the next one" and the letter it
        // would otherwise spell.
        for word in CAPITAL_WORDS {
            assert!(lookup(word).is_none(), "{word:?} is ambiguous");
        }
    }
}
