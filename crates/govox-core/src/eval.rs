//! Scoring recognised text against a reference.
//!
//! Lives in `govox-core` rather than in the test that uses it because the
//! *normalisation* is a decision, not an implementation detail: whether
//! `Rentals.ca` and `rentals.ca` count as the same word decides whether a
//! dictionary rule looks like it is working. Decisions of that shape belong
//! where they can be read and unit-tested, next to the rest of the pinned text
//! behaviour.
//!
//! # Word error rate is a tripwire here, not a measurement
//!
//! The eval corpus is a few dozen short utterances. On a six-word sentence a
//! single wrong word is ~16% WER, so the aggregate moves in visible steps and
//! will not resolve a one- or two-point difference between models. It answers
//! "did this get materially worse", and nothing finer.
//!
//! [`term_recall`] is the metric that answers the question actually being
//! asked. The corpus exists because "Twillingate" kept arriving as "twiddling
//! gate"; whether that specific word survived is a fact, where the WER it
//! contributes is a ratio diluted by every word around it.

/// Text reduced to the form the scores compare.
///
/// Lowercased, stripped of punctuation that is not part of a word, and
/// whitespace-collapsed. The rules are deliberately few, and each one exists to
/// stop a difference that is not a recognition error from counting as one:
///
/// - **Case** is the decoder's business and varies by model. `transcribes_the_hello_fixture`
///   already refuses to assert on it for the same reason.
/// - **Trailing punctuation** is added by the correction pipeline, not heard,
///   so scoring it would measure `ensure_terminal_punctuation` rather than the
///   model.
/// - **Interior punctuation is kept**, which is the point of the exercise:
///   `rentals.ca` must not score equal to `rentals ca`, because turning the
///   second into the first is exactly what the dictionary is for.
#[must_use]
pub fn normalize_for_scoring(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            let word: String = word.to_lowercase();
            // Only the edges: an inner '.' or '-' is part of the token
            // ("rentals.ca", "large-v3-turbo") and must survive.
            word.trim_matches(|c: char| !c.is_alphanumeric()).to_owned()
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Tokens as the scores see them.
#[must_use]
pub fn tokens(text: &str) -> Vec<String> {
    normalize_for_scoring(text)
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect()
}

/// The edit distance between two token sequences, and the reference length.
///
/// Levenshtein over *words*, not characters: a word is the unit a listener
/// notices, and character distance would score a plural as most of a hit.
/// Hand-rolled rather than pulled from a crate — it is fifteen lines, and
/// `govox-core` taking a dependency for it would be a poor trade.
#[must_use]
pub fn edit_distance(reference: &[String], hypothesis: &[String]) -> usize {
    // Two rows rather than the full matrix: the corpus is small, but this is
    // the shape that does not surprise anyone reading it later with a long one.
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut current = vec![0_usize; hypothesis.len() + 1];

    for (i, reference_word) in reference.iter().enumerate() {
        current[0] = i + 1;
        for (j, hypothesis_word) in hypothesis.iter().enumerate() {
            let substitution = previous[j] + usize::from(reference_word != hypothesis_word);
            let deletion = previous[j + 1] + 1;
            let insertion = current[j] + 1;
            current[j + 1] = substitution.min(deletion).min(insertion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[hypothesis.len()]
}

/// Word error rate: edit distance over the reference length.
///
/// Can exceed 1.0 — a hypothesis longer than the reference has more insertions
/// than the reference has words — and is **not** clamped. A rate above 1.0 means
/// the recogniser produced more wrong words than there were words to get right,
/// usually a hallucination, and flattening that to "100%" would hide the one
/// case most worth seeing.
///
/// An empty reference scores 0.0 for an empty hypothesis and 1.0 for anything
/// else, rather than dividing by zero.
#[must_use]
pub fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let reference = tokens(reference);
    let hypothesis = tokens(hypothesis);
    if reference.is_empty() {
        return if hypothesis.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&reference, &hypothesis) as f64 / reference.len() as f64
}

/// Did the terms that matter survive?
///
/// A term may be several words ("rentals dot ca" is one idea), so this matches
/// on the normalised *string* rather than token equality, and anchors on word
/// boundaries so `port` does not match inside `lewisporte`.
#[must_use]
pub fn term_recall<'a>(hypothesis: &str, terms: &'a [String]) -> Vec<(&'a str, bool)> {
    let hypothesis = normalize_for_scoring(hypothesis);
    terms
        .iter()
        .map(|term| {
            let term_normalized = normalize_for_scoring(term);
            (term.as_str(), contains_words(&hypothesis, &term_normalized))
        })
        .collect()
}

/// Whole-word containment: `"lewisporte"` does not contain `"port"`.
///
/// The same rule the personal dictionary uses for its replacements, and for the
/// same reason — `bounded_pattern` exists because "rentals ca" matching inside
/// "rentals cancelled" was a real bug.
fn contains_words(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack: Vec<&str> = haystack.split(' ').collect();
    let needle: Vec<&str> = needle.split(' ').collect();
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Vec<String> {
        tokens(text)
    }

    #[test]
    fn identical_text_scores_zero() {
        assert_eq!(word_error_rate("hello world", "hello world"), 0.0);
    }

    #[test]
    fn case_and_trailing_punctuation_are_not_errors() {
        // Both are added downstream or vary by model, so scoring them would
        // measure the correction pipeline rather than the recogniser.
        assert_eq!(word_error_rate("hello world", "Hello, world."), 0.0);
    }

    /// The distinction the corpus exists to measure: an interior full stop is
    /// part of the token, and turning "rentals ca" into "rentals.ca" is exactly
    /// what a dictionary rule does.
    #[test]
    fn an_interior_full_stop_is_part_of_the_word() {
        assert_eq!(word_error_rate("rentals.ca", "Rentals.ca"), 0.0);
        assert_eq!(word_error_rate("rentals.ca", "rentals ca"), 2.0);
    }

    #[test]
    fn each_edit_kind_counts_once() {
        // substitution
        assert_eq!(word_error_rate("a b c", "a x c"), 1.0 / 3.0);
        // deletion
        assert_eq!(word_error_rate("a b c", "a c"), 1.0 / 3.0);
        // insertion
        assert_eq!(word_error_rate("a b c", "a b x c"), 1.0 / 3.0);
    }

    /// Not clamped: a hallucination on a short reference is the case most worth
    /// seeing, and 1.0 would hide it.
    #[test]
    fn a_rate_above_one_is_reported_as_it_is() {
        let rate = word_error_rate("hello", "www.github.com thanks for watching");
        assert!(rate > 1.0, "got {rate}");
    }

    #[test]
    fn an_empty_reference_does_not_divide_by_zero() {
        assert_eq!(word_error_rate("", ""), 0.0);
        assert_eq!(word_error_rate("", "something"), 1.0);
    }

    #[test]
    fn edit_distance_is_symmetric_in_cost() {
        assert_eq!(edit_distance(&t("a b"), &t("a b c")), 1);
        assert_eq!(edit_distance(&t("a b c"), &t("a b")), 1);
    }

    #[test]
    fn a_present_term_is_recalled() {
        let terms = vec!["Twillingate".to_owned()];
        assert_eq!(
            term_recall("we drove to Twillingate today", &terms),
            vec![("Twillingate", true)]
        );
    }

    /// The failure the corpus was built from.
    #[test]
    fn a_mis_transcribed_term_is_missed() {
        let terms = vec!["Twillingate".to_owned()];
        assert_eq!(
            term_recall("we drove to twiddling gate today", &terms),
            vec![("Twillingate", false)]
        );
    }

    #[test]
    fn a_multi_word_term_is_matched_as_a_phrase() {
        let terms = vec!["ultra filtered milk".to_owned()];
        assert_eq!(
            term_recall("a carton of ultra filtered milk", &terms),
            vec![("ultra filtered milk", true)]
        );
        assert_eq!(
            term_recall("a carton of ultra fiddle", &terms),
            vec![("ultra filtered milk", false)]
        );
    }

    /// Whole words only, matching the dictionary's own rule: "lewisporte" must
    /// not count as containing "port".
    #[test]
    fn a_term_does_not_match_inside_a_longer_word() {
        let terms = vec!["port".to_owned()];
        assert_eq!(
            term_recall("we sailed from lewisporte", &terms),
            vec![("port", false)]
        );
    }
}
