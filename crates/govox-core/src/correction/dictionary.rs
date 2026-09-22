//! Personal-dictionary replacements, applied after recognition.
//!
//! Ported from `correction/dictionary.py`.

use fancy_regex::Regex;

use crate::domain::PersonalDictionary;

/// A dictionary with its patterns already compiled.
///
/// Compiling per call is what the reference effectively does, but Python's `re`
/// keeps an internal pattern cache that makes the cost invisible; Rust has no
/// such cache, so a naive port recompiles every replacement on every utterance.
/// Building once and reusing is both faster and closer to the reference's real
/// behaviour.
#[derive(Debug, Clone, Default)]
pub struct CompiledDictionary {
    /// `(pattern, replacement)`. Empty sources are dropped at build time.
    rules: Vec<(Regex, String)>,
}

impl CompiledDictionary {
    #[must_use]
    pub fn new(dictionary: &PersonalDictionary) -> Self {
        let rules = dictionary
            .replacements
            .iter()
            .filter(|(source, _)| {
                // An empty pattern matches everywhere; it can only do harm.
                // This guard is also why `bounded_pattern` may index the first
                // character without checking.
                !source.is_empty()
            })
            .filter_map(|(source, target)| {
                Regex::new(&format!("(?i){}", bounded_pattern(source)))
                    .ok()
                    .map(|pattern| (pattern, target.clone()))
            })
            .collect();
        Self { rules }
    }

    /// Apply each rule in order. Order matters and is the user's.
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        let mut result = text.to_owned();
        for (pattern, target) in &self.rules {
            // The replacement is inserted verbatim. As a template string it
            // would be scanned for group references, so a backslash or "\1" in
            // a user's dictionary would corrupt the output or fail.
            let before = result.clone();
            result = super::punctuation::replace_all(pattern, &before, |caps| {
                let whole = caps.get(0).expect("whole match");
                keep_sentence_capital(&before[..whole.start()], whole.as_str(), target)
            });
        }
        result
    }
}

/// `target`, capitalised when it replaces a sentence's capitalised first word.
///
/// The dictionary runs after the casing stages, so a lowercase target undid
/// their work: "Sim links are broken." became "symlinks are broken." The
/// capital is carried over only where casing put it — the match opens a
/// sentence *and* arrives capitalised — so a mid-sentence "LOL" still becomes
/// "lol", and an utterance continuing a sentence, which casing left lowercase,
/// stays lowercase. A target that is not lowercase is the user's casing and is
/// never touched.
fn keep_sentence_capital(preceding: &str, matched: &str, target: &str) -> String {
    let opens_sentence = preceding
        .trim_end_matches([' ', '\t'])
        .chars()
        .next_back()
        .is_none_or(|last| matches!(last, '.' | '!' | '?' | '…' | '\n'));
    let capitalised = matched.chars().next().is_some_and(char::is_uppercase);
    let lowercase_target = target.chars().next().is_some_and(char::is_lowercase);
    if opens_sentence && capitalised && lowercase_target {
        super::sentence_case(target)
    } else {
        target.to_owned()
    }
}

/// Apply each `(from, to)` in order, case-insensitively.
///
/// Convenience wrapper; hot paths should hold a [`CompiledDictionary`].
#[must_use]
pub fn apply_replacements(text: &str, dictionary: &PersonalDictionary) -> String {
    CompiledDictionary::new(dictionary).apply(text)
}

/// `source` as a regex that only matches whole words.
///
/// Without this, matching is a bare substring search, which quietly corrupts
/// longer words containing the pattern. Not hypothetical — it is what made the
/// obvious rules unusable:
///
/// ```text
/// "lol"        ->  "lollipop" becomes "LOLlipop", "Lola" becomes "LOLa"
/// "rent sync"  ->  "the current sync failed" becomes "the curRentsync failed"
/// ```
///
/// The boundary is applied per end and only where it means something. `\b` sits
/// between a word and a non-word character, so anchoring an end whose character
/// is already a non-word one (".ca", "c++") would demand an adjacent word
/// character and stop the pattern matching at all.
///
/// # Panics
///
/// On an empty `source`, matching the reference, which indexes `source[0]`
/// unguarded. Callers must skip empty sources — [`apply_replacements`] does.
#[must_use]
pub fn bounded_pattern(source: &str) -> String {
    let mut pattern = py_escape(source);
    let first = source.chars().next().expect("source must not be empty");
    let last = source
        .chars()
        .next_back()
        .expect("source must not be empty");
    if is_word_char(first) {
        pattern = format!(r"\b{pattern}");
    }
    if is_word_char(last) {
        pattern = format!(r"{pattern}\b");
    }
    pattern
}

fn is_word_char(char: char) -> bool {
    char.is_alphanumeric() || char == '_'
}

/// Python's `re.escape`, character for character.
///
/// Rust's `regex::escape` escapes only what is metacharacter-significant, while
/// Python (3.7+) also escapes ASCII whitespace and a few harmless punctuation
/// marks. The two produce functionally identical patterns, but
/// [`bounded_pattern`] returns the pattern *string*, and matching the reference
/// byte for byte is cheaper than reasoning each time about whether a difference
/// is cosmetic.
///
/// The set is `re`'s own: `()[]{}?*+-|^$\.&~#` plus space, tab, newline,
/// carriage return, vertical tab and form feed.
fn py_escape(source: &str) -> String {
    const SPECIAL: &[char] = &[
        '(', ')', '[', ']', '{', '}', '?', '*', '+', '-', '|', '^', '$', '\\', '.', '&', '~', '#',
        ' ', '\t', '\n', '\r', '\u{b}', '\u{c}',
    ];
    let mut out = String::with_capacity(source.len());
    for char in source.chars() {
        if SPECIAL.contains(&char) {
            out.push('\\');
        }
        out.push(char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::apply_replacements;
    use crate::domain::PersonalDictionary;

    fn apply(text: &str, from: &str, to: &str) -> String {
        let dictionary = PersonalDictionary {
            replacements: vec![(from.to_owned(), to.to_owned())],
            ..PersonalDictionary::default()
        };
        apply_replacements(text, &dictionary)
    }

    /// Casing capitalised the sentence; a lowercase target must not undo it.
    #[test]
    fn a_sentence_opening_capital_survives_the_replacement() {
        assert_eq!(
            apply("Sim links are broken.", "sim links", "symlinks"),
            "Symlinks are broken."
        );
        assert_eq!(
            apply("Done. Sim link it.", "sim link", "symlink"),
            "Done. Symlink it."
        );
        assert_eq!(apply("Fine!\nLol", "lol", "lol"), "Fine!\nLol");
    }

    /// Anywhere else the target's own casing wins.
    #[test]
    fn mid_sentence_and_continuations_take_the_target_as_written() {
        assert_eq!(
            apply("That's a lot, LOL.", "lol", "lol"),
            "That's a lot, lol."
        );
        assert_eq!(
            apply("then sim link it", "sim link", "symlink"),
            "then symlink it"
        );
        assert_eq!(
            apply("So Sim Link it", "sim link", "symlink"),
            "So symlink it"
        );
        // An utterance continuing a sentence arrives lowercase; it stays so.
        assert_eq!(apply("lol that's funny", "lol", "lol"), "lol that's funny");
    }

    /// A target with casing of its own is the user's, even at a sentence start.
    #[test]
    fn a_cased_target_is_never_altered() {
        assert_eq!(apply("Jura is down.", "jura", "Jira"), "Jira is down.");
        assert_eq!(apply("Cbd oil.", "cbd", "CBD"), "CBD oil.");
    }
}
