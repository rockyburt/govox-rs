//! Tidying what the decoder hands back, and steering what it hears.

/// Collapse runs of whitespace and trim.
///
/// `preserve_leading_space` keeps a single leading space when the input began
/// with whitespace. Streaming relies on this: Whisper emits each word with its
/// own leading space as the word separator, so stripping the leading space of
/// every committed delta would glue consecutive deltas together — `"Hello"` +
/// `"world"` → `"Helloworld"`.
#[must_use]
pub fn postprocess_text(text: &str, preserve_leading_space: bool) -> String {
    let leading = preserve_leading_space && text.chars().next().is_some_and(|c| c.is_whitespace());

    let mut collapsed = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            in_space = true;
        } else {
            if in_space && !collapsed.is_empty() {
                collapsed.push(' ');
            }
            in_space = false;
            collapsed.push(ch);
        }
    }

    if leading && !collapsed.is_empty() {
        return format!(" {collapsed}");
    }
    collapsed
}

/// Build the `initial_prompt` that biases the decoder toward known terms.
///
/// Truncation is by whitespace-separated word, not by real tokenizer token —
/// the same approximation `govox-py` makes. A word is usually one to two BPE
/// tokens, so the budget is conservative rather than exact, which is the safe
/// direction: overshooting would push real audio context out of the window.
#[must_use]
pub fn bias_prompt(bias_terms: &[String], token_budget: u32) -> String {
    if token_budget == 0 {
        return String::new();
    }
    let budget = token_budget as usize;
    let mut tokens: Vec<&str> = Vec::new();
    for term in bias_terms {
        for token in term.split_whitespace() {
            if tokens.len() >= budget {
                return tokens.join(" ");
            }
            tokens.push(token);
        }
    }
    tokens.join(" ")
}

/// Map the config value to what Whisper expects.
///
/// `None` asks Whisper to detect the language from the audio, which it does per
/// utterance. Mid-sentence switching is not on offer — the decoder picks one
/// language for the whole segment — so this buys switching *between*
/// utterances, not within one.
#[must_use]
pub fn whisper_language(language: &str) -> Option<&str> {
    let trimmed = language.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        return None;
    }
    Some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn whitespace_runs_collapse_and_the_text_is_trimmed() {
        assert_eq!(
            postprocess_text("  Hello   world.  ", false),
            "Hello world."
        );
        assert_eq!(postprocess_text("a\n\nb\tc", false), "a b c");
    }

    #[test]
    fn an_empty_or_blank_input_yields_an_empty_string() {
        assert_eq!(postprocess_text("", false), "");
        assert_eq!(postprocess_text("   \n ", false), "");
        // Even asking to preserve the leading space: there is nothing to lead.
        assert_eq!(postprocess_text("   ", true), "");
    }

    #[test]
    fn a_leading_space_survives_only_when_asked_for() {
        assert_eq!(postprocess_text(" world", true), " world");
        assert_eq!(postprocess_text(" world", false), "world");
        // No leading whitespace in the input means none in the output.
        assert_eq!(postprocess_text("world", true), "world");
    }

    #[test]
    fn preserving_the_leading_space_is_what_keeps_deltas_apart() {
        // The bug this flag exists to prevent: "Hello" + "world" glued into
        // "Helloworld" because each delta was stripped independently.
        let first = postprocess_text(" Hello", true);
        let second = postprocess_text(" world", true);
        assert_eq!(format!("{first}{second}"), " Hello world");
    }

    #[test]
    fn a_zero_budget_disables_biasing() {
        assert_eq!(bias_prompt(&terms(&["Kubernetes"]), 0), "");
    }

    #[test]
    fn the_bias_prompt_is_truncated_at_the_word_budget() {
        let bias = terms(&["Rocky Burt", "Kubernetes", "govox"]);
        assert_eq!(bias_prompt(&bias, 10), "Rocky Burt Kubernetes govox");
        assert_eq!(bias_prompt(&bias, 3), "Rocky Burt Kubernetes");
        // The budget cuts mid-term, matching govox-py: terms are not atomic.
        assert_eq!(bias_prompt(&bias, 1), "Rocky");
    }

    #[test]
    fn empty_bias_terms_produce_no_prompt() {
        assert_eq!(bias_prompt(&[], 100), "");
        assert_eq!(bias_prompt(&terms(&["", "   "]), 100), "");
    }

    #[test]
    fn auto_and_blank_mean_detect_the_language() {
        assert_eq!(whisper_language("auto"), None);
        assert_eq!(whisper_language("AUTO"), None);
        assert_eq!(whisper_language(""), None);
        assert_eq!(whisper_language("  "), None);
        assert_eq!(whisper_language("en"), Some("en"));
        assert_eq!(whisper_language(" fr "), Some("fr"));
    }
}
