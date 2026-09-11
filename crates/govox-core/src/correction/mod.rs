//! The correction pipeline: recognised text → corrected text plus an action.
//!
//! Ported from `correction/pipeline.py`. The **stage order is load-bearing**
//! and every constraint in `apply_rules` is justified where it is applied.

pub mod casing;
pub mod commands;
pub mod custom;
pub mod dictionary;
pub mod emoji;
pub mod grammar;
pub mod numbers;
pub mod punctuation;
pub mod spelling;

use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;

use crate::config::CorrectionConfig;
use crate::domain::{CorrectionResult, PersonalDictionary, PipelineAction};

/// Field purposes where prose rules are actively wrong: a closing full stop
/// breaks a URL, and a capital breaks a shell command.
///
/// Reported by the client through the input method (IBus `InputPurpose`). Only
/// the purpose is used, never the hints: `FREE_FORM` appears with and without
/// `SPELLCHECK`, so treating a missing hint as "not prose" would strip capitals
/// and full stops from ordinary text in every application that simply does not
/// set it — a far worse failure than the one it would fix.
pub const VERBATIM_PURPOSES: &[&str] = &[
    "URL", "EMAIL", "TERMINAL", "PASSWORD", "PIN", "DIGITS", "NUMBER", "PHONE",
];

/// Verbatim fields that nevertheless hold *words*, so consecutive utterances
/// need a space between them.
///
/// Standing prose rules down and running utterances together are two different
/// decisions, and treating them as one produced a real bug: dictating twice into
/// a terminal gave `…it does now.this is fun!`. A terminal line is words
/// separated by spaces — `cd` then `Documents` is two words — whereas every
/// other verbatim purpose holds a single token, where a space would be
/// corruption: `example` then `dot com` must join as `example.com`.
///
/// The cost is the mirror image, and it is deliberate: dictating a URL across
/// two utterances *in a terminal* now yields `example .com`. That needs a
/// hostname split mid-word in a shell; running every multi-utterance command
/// line together is the far commoner failure. See `docs/parity.md`.
pub const SPACED_PURPOSES: &[&str] = &["TERMINAL"];

/// Where capitals are never meaningful, all of them go. A hostname is
/// case-insensitive and conventionally lowercase, and so is an email address.
pub const LOWERCASE_WHOLE: &[&str] = &["URL", "EMAIL"];

/// In a terminal only the *command* is reliably lowercase. Arguments and paths
/// are case-sensitive and often deliberately capitalised — "cd Documents" must
/// survive — so only the first word is corrected.
pub const LOWERCASE_FIRST_WORD: &[&str] = &["TERMINAL"];

const SENTENCE_TERMINATORS: &[char] = &['.', '!', '?'];

/// What the focused field wants from the pipeline.
///
/// One value rather than two booleans, because only three of the four
/// combinations mean anything — there is no field that takes prose rules but
/// refuses a separating space, and encoding the choice this way means no caller
/// can ask for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldRules {
    /// Ordinary text: capitals, a closing full stop, and a space between
    /// consecutive utterances.
    Prose,
    /// A single token — a URL, an address, a PIN. No prose rules, and no
    /// separating space, since anything appended continues the same token.
    SingleToken,
    /// Words without prose: a terminal line. No capitals and no closing full
    /// stop, but consecutive utterances are still separated by a space.
    SpacedWords,
}

impl FieldRules {
    /// Whether a space goes between this utterance and what precedes it.
    #[must_use]
    pub fn separates(self) -> bool {
        matches!(self, Self::Prose | Self::SpacedWords)
    }
}

/// Runtime state the pipeline consults per utterance.
///
/// `govox-py` passes three zero-argument callables, because the mode changes
/// while the daemon runs and the `Corrector` protocol is `correct(text)` with
/// nowhere to put state. Here they are just values, resolved by the caller
/// immediately before the call — which is what the callables did anyway.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// Whether command mode is active, enabling Tier 2 phrase edits.
    pub command_mode: bool,
    /// Text already in front of the caret, so an utterance can continue a
    /// sentence instead of always starting one. `None` means "could not be
    /// read", which must behave exactly as govox did before this existed.
    pub preceding_text: Option<String>,
    /// What kind of field has focus, so prose rules can stand down where they
    /// would do damage. Unknown means unchanged.
    pub field_purpose: Option<String>,
    /// The focused window's label, for custom commands scoped to one
    /// application. `None` means it could not be read, which scoped commands
    /// treat as "not this application" rather than as a wildcard.
    pub app: Option<String>,
}

pub struct CorrectionPipeline {
    pub config: CorrectionConfig,
    pub dictionary: PersonalDictionary,
    /// Lives in `[editing]`, not `[correction]`, because it governs how the
    /// daemon treats an utterance rather than how text is cleaned up.
    pub mode_switching: bool,
    /// The dictionary's patterns, compiled once at construction.
    compiled: dictionary::CompiledDictionary,
    /// User-defined commands, consulted after every built-in has declined.
    commands: Vec<crate::config::CustomCommand>,
}

impl CorrectionPipeline {
    #[must_use]
    pub fn new(
        config: CorrectionConfig,
        dictionary: PersonalDictionary,
        mode_switching: bool,
    ) -> Self {
        let compiled = dictionary::CompiledDictionary::new(&dictionary);
        Self {
            config,
            dictionary,
            mode_switching,
            compiled,
            commands: Vec::new(),
        }
    }

    /// Attach the user's custom commands.
    ///
    /// Separate from `new` so every existing caller — and every golden replay,
    /// whose records were made before these existed — keeps its behaviour
    /// exactly. An empty list is not merely the default, it is the state the
    /// whole corpus was recorded in.
    #[must_use]
    pub fn with_commands(mut self, commands: Vec<crate::config::CustomCommand>) -> Self {
        self.commands = commands;
        self
    }

    #[must_use]
    pub fn correct(&self, text: &str, context: &Context) -> CorrectionResult {
        if !self.config.enabled {
            return CorrectionResult {
                raw_text: text.to_owned(),
                corrected_text: text.to_owned(),
                action: PipelineAction::Text(text.to_owned()),
            };
        }

        let purpose = context.field_purpose.as_deref();
        let mut corrected = apply_rules(
            text,
            &self.config,
            context.preceding_text.as_deref(),
            field_rules(purpose),
        );
        // Before replacements, so a replacement's own casing wins over this.
        corrected = undo_prose_casing(&corrected, purpose);
        corrected = self.compiled.apply(&corrected);

        let mut action =
            commands::detect_command(&corrected, self.mode_switching, context.command_mode);
        // Custom commands are consulted only where a built-in declined, so no
        // config file can take "delete that" away from the person who wrote it.
        // Matched on the *corrected* text for the same reason built-ins are:
        // the recogniser's punctuation and casing are already resolved by here,
        // so a phrase matches whether or not Whisper ended it with a full stop.
        if matches!(action, PipelineAction::Text(_))
            && let Some(custom) =
                custom::match_custom(&corrected, &self.commands, context.app.as_deref())
        {
            action = custom;
        }
        if let PipelineAction::Command(name) = &action {
            corrected = command_text(name).to_owned();
        }

        CorrectionResult {
            raw_text: text.to_owned(),
            corrected_text: corrected,
            action,
        }
    }
}

/// The fixed stage order. Each constraint is justified where it is applied.
#[must_use]
pub fn apply_rules(
    text: &str,
    config: &CorrectionConfig,
    preceding: Option<&str>,
    rules: FieldRules,
) -> String {
    let mut normalized = normalize_spacing(text);
    if normalized.is_empty() {
        return normalized;
    }
    normalized = filter_disfluencies(&normalized, config);
    if normalized.is_empty() {
        return normalized; // the whole utterance was filler — inject nothing
    }
    if config.spoken_punctuation {
        // Before casing: spoken marks create the sentence boundaries that
        // capitalization then depends on.
        normalized = normalize_spacing(&punctuation::apply_spoken_punctuation(&normalized));
        if normalized.is_empty() {
            return normalized;
        }
    }
    if config.number_formatting {
        // Before casing and before detect_command, so "delete previous twenty
        // five words" reaches the grammar as a digit count it already accepts.
        normalized =
            numbers::attach_units_to_digits(&numbers::apply_number_formatting(&normalized));
    }
    if config.spoken_emoji {
        // After punctuation (so a mark next to an emoji is already resolved),
        // before casing and terminal punctuation (so a trailing emoji does not
        // acquire a stray full stop).
        normalized = emoji::apply_spoken_emoji(&normalized);
    }
    // Outside prose none of what follows applies: a capital and a closing full
    // stop are wrong in a URL bar. Spoken punctuation and spoken case still
    // work — "dot" is an instruction, not an assumption. The separator is a
    // *separate* question, asked below for every field that holds words:
    // returning early without asking it is what ran two terminal utterances
    // together.
    if rules != FieldRules::Prose {
        normalized = case_control(&normalized, config);
        return separated(normalized, preceding, rules);
    }

    // Continuing an unfinished sentence is not the same job as starting one.
    let continuing = is_continuation(preceding);
    if !continuing {
        normalized = sentence_case(&normalized);
    }
    if config.spoken_punctuation {
        normalized = punctuation::capitalize_after_terminators(&normalized);
    }
    if !continuing && wants_terminal_punctuation(&normalized) {
        normalized = ensure_terminal_punctuation(&normalized);
    }
    // Last, and deliberately after both casing stages: they only ever *add*
    // capitals, so a "no caps" applied before them would be undone at exactly
    // the sentence start where it was most likely meant.
    normalized = case_control(&normalized, config);
    separated(normalized, preceding, rules)
}

/// Prefix the separating space, when this field and this caret both call for
/// one.
///
/// The empty check is not an optimisation: an utterance of nothing but case
/// markers corrects to nothing, and a lone space is worse than silence.
#[must_use]
fn separated(text: String, preceding: Option<&str>, rules: FieldRules) -> String {
    if text.is_empty() || !rules.separates() {
        return text;
    }
    format!("{}{text}", separator_for(preceding))
}

/// Spoken case control, when it is switched on.
///
/// A thin wrapper so the config check lives in one place: `apply_rules` calls
/// this from two arms, and a stage that ran in only one of them would be a
/// verbatim field silently behaving differently from a prose one.
#[must_use]
fn case_control(text: &str, config: &CorrectionConfig) -> String {
    if config.case_control {
        casing::apply_case_control(text)
    } else {
        text.to_owned()
    }
}

/// Strip the capitals Whisper adds out of habit, where they are wrong.
///
/// Whisper cases its output as prose whatever the field: "ls" comes back as
/// "Ls" and "rentals.ca" as "Rentals.Ca". Stopping govox from *adding* capitals
/// was not enough — the model's own have to be undone.
#[must_use]
pub fn undo_prose_casing(text: &str, purpose: Option<&str>) -> String {
    let Some(purpose) = purpose else {
        return text.to_owned();
    };
    if text.is_empty() {
        return text.to_owned();
    }
    if LOWERCASE_WHOLE.contains(&purpose) {
        return text.to_lowercase();
    }
    if LOWERCASE_FIRST_WORD.contains(&purpose) {
        // Python's str.partition(" "): split at the first space only.
        return match text.split_once(' ') {
            Some((head, tail)) => format!("{} {tail}", head.to_lowercase()),
            None => text.to_lowercase(),
        };
    }
    text.to_owned()
}

/// Should prose rules — capitals, a closing full stop — apply here?
///
/// `None` means the client said nothing, which must mean "carry on as before"
/// rather than "assume verbatim": most clients report nothing, and treating
/// silence as a signal would change behaviour everywhere at once.
#[must_use]
pub fn is_prose_field(purpose: Option<&str>) -> bool {
    field_rules(purpose) == FieldRules::Prose
}

/// Which rules the focused field gets — the full answer, of which
/// [`is_prose_field`] is the first third.
///
/// Defined here and derived there, rather than the two reading the purpose
/// tables independently: they answered the same question in two places once
/// already, and the half that was never asked is the bug this fixes.
#[must_use]
pub fn field_rules(purpose: Option<&str>) -> FieldRules {
    let Some(purpose) = purpose else {
        return FieldRules::Prose; // silence means "carry on as before"
    };
    if !VERBATIM_PURPOSES.contains(&purpose) {
        return FieldRules::Prose;
    }
    if SPACED_PURPOSES.contains(&purpose) {
        return FieldRules::SpacedWords;
    }
    FieldRules::SingleToken
}

/// Is the caret sitting mid-sentence?
///
/// `None` means the field could not be read, the ordinary answer for a terminal
/// or an application that exposes nothing. It must return `false` — govox then
/// behaves exactly as it did before context existed, keeping field access an
/// enhancement and never a dependency.
#[must_use]
pub fn is_continuation(preceding: Option<&str>) -> bool {
    let Some(preceding) = preceding else {
        return false;
    };
    let trimmed = preceding.trim_end_matches([' ', '\t']);
    let Some(last) = trimmed.chars().next_back() else {
        return false; // empty field: this utterance starts the first sentence
    };
    if last == '\r' || last == '\n' {
        return false; // a new line starts a new sentence
    }
    if SENTENCE_TERMINATORS.contains(&last) {
        return false;
    }
    // No terminator, which used to settle it. It no longer does: a one-word
    // answer and an emoji are left unpunctuated on purpose, so their missing
    // full stop is not evidence of an unfinished sentence. Without this, "Yes"
    // would swallow whatever was dictated next into the same lowercase run.
    //
    // Only the fragment since the last terminator is the sentence in progress;
    // asking about the whole field would call a long document a one-word answer
    // the moment it happened to end in one.
    let current = trimmed
        .rsplit(|char| SENTENCE_TERMINATORS.contains(&char) || char == '\n')
        .next()
        .unwrap_or(trimmed);
    wants_terminal_punctuation(current)
}

/// A single space when the caret is flush against existing text.
///
/// Without this, dictating twice produces "the first sentence.The second" — the
/// corrected text is stripped, so nothing else would separate them.
#[must_use]
pub fn separator_for(preceding: Option<&str>) -> &'static str {
    match preceding {
        None | Some("") => "",
        Some(text) => match text.chars().next_back() {
            Some(last) if last.is_whitespace() => "",
            _ => " ",
        },
    }
}

#[must_use]
pub fn filter_disfluencies(text: &str, config: &CorrectionConfig) -> String {
    let mut text = text.to_owned();
    if config.drop_fillers && !config.filler_words.is_empty() {
        text = drop_filler_words(&text, &config.filler_words);
    }
    if config.collapse_repeats {
        text = collapse_repeated_words(&text);
    }
    normalize_spacing(&text) // clean up gaps left by removals
}

/// Remove filler words, longest phrase first so "you know" beats "you"/"know".
#[must_use]
pub fn drop_filler_words(text: &str, fillers: &[String]) -> String {
    let mut ordered: Vec<&String> = fillers.iter().collect();
    ordered.sort_by_key(|f| std::cmp::Reverse(f.len()));
    let mut text = text.to_owned();
    for filler in ordered {
        // Whole word(s), case-insensitive, plus a trailing comma the filler
        // introduced.
        let Ok(pattern) = Regex::new(&format!(r"(?i)\b{}\b,?", regex::escape(filler))) else {
            continue;
        };
        text = pattern.replace_all(&text, "").into_owned();
    }
    text
}

/// Laughter tokens whose repeat is the word, not a stutter.
///
/// **"he" is deliberately absent, and that is the hard case.** It is the form
/// most often reported for a spoken "hehe", and it is also a pronoun — so
/// exempting it would preserve every genuine stutter of "he he went…" as well.
/// A stutter surviving into the document is a worse failure than a laugh
/// arriving as one "he", and stutters are exactly what this stage exists to
/// absorb. Bias is the lever for that case: teaching the recogniser to emit
/// "hehe" as one token stops the repeat ever forming. See the dictionary.
const LAUGHTER: &[&str] = &["ha", "hah", "heh", "hee"];

/// Words that keep their repeats, because for these a repeat carries meaning.
///
/// Derived from `SPOKEN_PUNCTUATION` rather than listed, so a phrase added to
/// that table is exempt here for free and the two cannot drift apart. Only
/// single-word phrases are taken: the pattern captures `\w+`, so a multi-word
/// phrase could never have been collapsed anyway.
static COLLAPSE_EXEMPT: LazyLock<std::collections::HashSet<String>> = LazyLock::new(|| {
    let mut set: std::collections::HashSet<String> = punctuation::SPOKEN_PUNCTUATION
        .iter()
        .map(|(phrase, _, _)| *phrase)
        .filter(|phrase| !phrase.contains(' '))
        .map(str::to_lowercase)
        .collect();
    set.extend(LAUGHTER.iter().map(|word| (*word).to_owned()));
    set
});

fn is_collapse_exempt(word: &str) -> bool {
    COLLAPSE_EXEMPT.contains(&word.to_lowercase())
}

/// "the the dog" → "the dog"; case-insensitive comparison, keep the first form.
///
/// Needs a **backreference**, which the `regex` crate cannot express, so this is
/// the second of the two `fancy-regex` users. Deliberately *not* hand-rolled as
/// a token scan: `\b` means `"the, the"` is not collapsed, and reproducing that
/// by hand invites exactly the silent divergence the corpus exists to catch.
///
/// A word in `COLLAPSE_EXEMPT` keeps its repeats. This stage runs *before*
/// `apply_spoken_punctuation` and before the personal dictionary, so anything it
/// eats is gone before the stage that needed it ever runs — which is how
/// "hyphen hyphen" stopped being able to produce `--`. Three separate symptoms
/// traced back to that one mechanism; see docs/parity.md.
#[must_use]
pub fn collapse_repeated_words(text: &str) -> String {
    static PATTERN: LazyLock<FancyRegex> =
        LazyLock::new(|| FancyRegex::new(r"(?i)\b(\w+)(\s+\1\b)+").unwrap());
    punctuation::replace_all(&PATTERN, text, |caps| {
        let first = caps.get(1).expect("first group").as_str();
        if is_collapse_exempt(first) {
            // Returned verbatim, so the repeat survives exactly as spoken
            // rather than being normalised to some canonical spacing.
            return caps.get(0).expect("whole match").as_str().to_owned();
        }
        first.to_owned()
    })
}

/// Collapse runs of horizontal whitespace, preserving line structure.
///
/// Newlines are load-bearing once "new line" is a spoken mark, so this cannot
/// use a blanket `\s+` collapse — that would silently undo every break the
/// punctuation stage just produced. `[^\S\n]` is "whitespace that is not a
/// newline".
#[must_use]
pub fn normalize_spacing(text: &str) -> String {
    static BEFORE_MARK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[^\S\n]+([,.;:!?])").unwrap());
    static HORIZONTAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\S\n]+").unwrap());
    static AROUND_BREAK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" *\n *").unwrap());
    static MANY_BREAKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());

    let text = BEFORE_MARK.replace_all(text, "$1");
    let text = HORIZONTAL.replace_all(&text, " ");
    // A break swallows the spaces around it: "hello \n world" is "hello\nworld".
    let text = AROUND_BREAK.replace_all(&text, "\n");
    // Two blank lines are a paragraph; more are a mistake.
    let text = MANY_BREAKS.replace_all(&text, "\n\n");
    // Horizontal whitespace only. A trailing newline is a break the speaker
    // asked for ("hello new line" leaves the caret on the next line); a plain
    // strip would throw it away, turning a lone "new line" into "".
    text.trim_matches([' ', '\t', '\r', '\u{c}', '\u{b}'])
        .to_owned()
}

/// Uppercase the first alphabetic character.
///
/// `char::to_uppercase` can yield more than one character (ß → SS), exactly as
/// Python's `str.upper()` does, so the result may be longer than the input.
#[must_use]
pub fn sentence_case(text: &str) -> String {
    for (index, char) in text.char_indices() {
        if char.is_alphabetic() {
            let mut out = String::with_capacity(text.len());
            out.push_str(&text[..index]);
            out.extend(char.to_uppercase());
            out.push_str(&text[index + char.len_utf8()..]);
            return out;
        }
    }
    text.to_owned()
}

/// Is this utterance a sentence, or an answer?
///
/// A one-word reply — "yes", "approved", "tomorrow" — is not a sentence, and a
/// full stop on it is wrong twice over: it is not how anyone writes a one-word
/// answer, and in a chat box it reads as curt in a way the speaker did not
/// intend. An emoji is the same case with the alphabet removed: "👍." is not
/// something anyone types.
///
/// This is a *policy* about when to punctuate, so it lives beside the call
/// rather than inside [`ensure_terminal_punctuation`], which stays the
/// primitive that answers only "does this end in a terminator, and if not, add
/// one". Keeping the two apart is what lets the recorded behaviour of the
/// primitive stay unchanged while the policy above it moves.
///
/// Anything with no letter in it — an emoji, a bare number, a symbol — is never
/// a sentence. Beyond that the test is simply whether there is more than one
/// word.
#[must_use]
pub fn wants_terminal_punctuation(text: &str) -> bool {
    let trimmed = text.trim();
    if !trimmed.chars().any(char::is_alphabetic) {
        return false;
    }
    trimmed.split_whitespace().count() > 1
}

#[must_use]
pub fn ensure_terminal_punctuation(text: &str) -> String {
    if text.ends_with(['.', '!', '?', '\n']) {
        return text.to_owned();
    }
    format!("{text}.")
}

#[must_use]
pub fn command_text(name: &str) -> &'static str {
    match name {
        "newline" => "\n",
        "new_paragraph" => "\n\n",
        _ => "",
    }
}

#[cfg(test)]
mod short_answer_tests {
    use super::{is_continuation, wants_terminal_punctuation};

    #[test]
    fn a_one_word_answer_is_not_a_sentence() {
        for answer in ["yes", "no", "approved", "tomorrow", "  maybe  "] {
            assert!(
                !wants_terminal_punctuation(answer),
                "{answer:?} should not be given a full stop"
            );
        }
    }

    #[test]
    fn an_emoji_is_never_a_sentence() {
        // Including several: no alphabet, so no sentence, however many.
        for text in [
            "\u{1f44d}",
            "\u{2705}",
            "\u{1f44d} \u{1f389}",
            "42",
            "3.14",
            "?!",
        ] {
            assert!(
                !wants_terminal_punctuation(text),
                "{text:?} should not be given a full stop"
            );
        }
    }

    #[test]
    fn two_words_are_a_sentence_again() {
        assert!(wants_terminal_punctuation("looks good"));
        assert!(wants_terminal_punctuation("we drove out to Twillingate"));
        // An emoji does not disqualify a sentence that also has words in it.
        assert!(wants_terminal_punctuation("nice work \u{1f44d}"));
    }

    /// The reason this is not a one-line change.
    ///
    /// `is_continuation` read a missing full stop as an unfinished sentence.
    /// Once one-word answers stop getting one, that inference is wrong: "Yes"
    /// would swallow the next utterance into the same lowercase run.
    #[test]
    fn an_unpunctuated_answer_does_not_continue() {
        assert!(!is_continuation(Some("Yes")));
        assert!(!is_continuation(Some("\u{1f44d}")));
    }

    #[test]
    fn a_genuinely_unfinished_sentence_still_continues() {
        assert!(is_continuation(Some("we drove out to")));
        assert!(is_continuation(Some("the answer is")));
    }

    /// Only the fragment since the last terminator is the sentence in progress.
    /// A long field that happens to end in one word is not a one-word answer.
    #[test]
    fn a_long_field_ending_in_one_word_is_judged_on_its_last_sentence() {
        assert!(is_continuation(Some("We shipped it. Now we wait for")));
        // Ends mid-sentence with a single word after the full stop: still the
        // middle of a sentence, because more is plainly coming.
        assert!(!is_continuation(Some("We shipped it. Yes")));
    }

    #[test]
    fn the_existing_answers_are_unchanged() {
        assert!(!is_continuation(None));
        assert!(!is_continuation(Some("")));
        assert!(!is_continuation(Some("Done.")));
        assert!(!is_continuation(Some("Really?")));
        assert!(!is_continuation(Some("a line ends\n")));
    }
}

#[cfg(test)]
mod tests {
    use super::{FieldRules, apply_rules, collapse_repeated_words, field_rules, punctuation};
    use crate::config::CorrectionConfig;

    fn config() -> CorrectionConfig {
        CorrectionConfig {
            enabled: true,
            dictionary_path: String::new(),
            drop_fillers: false,
            filler_words: Vec::new(),
            collapse_repeats: false,
            spoken_punctuation: true,
            spoken_emoji: false,
            number_formatting: false,
            case_control: false,
        }
    }

    /// The bug, at the level it was reported: two utterances into a terminal.
    ///
    /// `"let's see what it does now."` followed by `"this is fun!"` arrived as
    /// `"…now.this is fun!"`, because the verbatim arm returned before the
    /// separator was ever considered.
    #[test]
    fn a_second_terminal_utterance_is_separated_from_the_first() {
        let out = apply_rules(
            "this is fun",
            &config(),
            Some("let's see what it does now."),
            FieldRules::SpacedWords,
        );
        assert_eq!(out, " this is fun");
    }

    /// The differential that gives the test above its meaning: the same call
    /// against a single-token field must *not* gain a space, or `example` plus
    /// `dot com` would stop making `example.com`.
    #[test]
    fn a_single_token_field_still_joins_without_a_space() {
        let out = apply_rules(
            "dot com",
            &config(),
            Some("example"),
            FieldRules::SingleToken,
        );
        assert_eq!(out, ".com");
    }

    #[test]
    fn a_terminal_utterance_gains_no_prose_rules_with_its_space() {
        let out = apply_rules("list files", &config(), Some("ls"), FieldRules::SpacedWords);
        // A space, but no capital and no full stop.
        assert_eq!(out, " list files");
    }

    /// A space already at the caret is not doubled — the separator asks the
    /// caret, not just the field.
    #[test]
    fn an_existing_space_is_not_doubled() {
        let out = apply_rules(
            "list files",
            &config(),
            Some("ls "),
            FieldRules::SpacedWords,
        );
        assert_eq!(out, "list files");
    }

    #[test]
    fn an_empty_caret_starts_a_terminal_line_flush() {
        assert_eq!(
            apply_rules("list files", &config(), Some(""), FieldRules::SpacedWords),
            "list files"
        );
        assert_eq!(
            apply_rules("list files", &config(), None, FieldRules::SpacedWords),
            "list files"
        );
    }

    #[test]
    fn purposes_split_three_ways() {
        assert_eq!(field_rules(None), FieldRules::Prose);
        assert_eq!(field_rules(Some("FREE_FORM")), FieldRules::Prose);
        assert_eq!(field_rules(Some("TERMINAL")), FieldRules::SpacedWords);
        assert_eq!(field_rules(Some("URL")), FieldRules::SingleToken);
        assert_eq!(field_rules(Some("PASSWORD")), FieldRules::SingleToken);
    }

    /// Only the token fields refuse a separator. Stated as a sweep so a purpose
    /// added to one list and not the other cannot pass unnoticed.
    #[test]
    fn every_verbatim_purpose_is_classified_and_only_terminals_separate() {
        for purpose in super::VERBATIM_PURPOSES {
            let rules = field_rules(Some(purpose));
            assert_ne!(rules, FieldRules::Prose, "{purpose} is verbatim");
            assert_eq!(
                rules.separates(),
                *purpose == "TERMINAL",
                "{purpose} separator"
            );
        }
    }

    /// The defect this exemption exists for: `--all-targets` could not be said.
    ///
    /// `collapse_repeated_words` ran before `apply_spoken_punctuation`, so the
    /// second "hyphen" was eaten before the stage that renders marks ever saw
    /// it, and the flag came out `-all-targets`.
    #[test]
    fn a_repeated_punctuation_word_survives_to_be_rendered() {
        assert_eq!(
            collapse_repeated_words("hyphen hyphen all hyphen targets"),
            "hyphen hyphen all hyphen targets"
        );
    }

    /// Derived from the table, not listed, so this holds for every single-word
    /// phrase in it rather than the handful someone remembered to write down.
    #[test]
    fn every_single_word_punctuation_phrase_is_exempt() {
        for (phrase, _, _) in punctuation::SPOKEN_PUNCTUATION {
            if phrase.contains(' ') {
                continue;
            }
            let doubled = format!("{phrase} {phrase}");
            assert_eq!(
                collapse_repeated_words(&doubled),
                doubled,
                "{phrase} should survive being repeated"
            );
        }
    }

    /// The stage still does its job. This is what it exists for.
    #[test]
    fn an_ordinary_repeat_still_collapses() {
        assert_eq!(collapse_repeated_words("the the dog"), "the dog");
        assert_eq!(collapse_repeated_words("very very good"), "very good");
        assert_eq!(
            collapse_repeated_words("but the the pipeline runs runs"),
            "but the pipeline runs"
        );
    }

    /// Laughter keeps its repeat so a dictionary rule can reach it.
    #[test]
    fn laughter_survives_but_the_pronoun_still_collapses() {
        assert_eq!(collapse_repeated_words("ha ha"), "ha ha");
        assert_eq!(collapse_repeated_words("hee hee"), "hee hee");

        // "he" is NOT exempt, deliberately: it is a pronoun, so exempting it
        // would preserve every genuine stutter. Bias is the lever for a spoken
        // "hehe", not this table.
        assert_eq!(collapse_repeated_words("he he went home"), "he went home");
    }

    /// Case is folded for the lookup, as it is for the match itself.
    #[test]
    fn an_exempt_word_is_recognised_whatever_its_case() {
        assert_eq!(collapse_repeated_words("Hyphen hyphen"), "Hyphen hyphen");
        assert_eq!(collapse_repeated_words("HA ha"), "HA ha");
    }

    /// Three or more repeats are one match, and all of them survive.
    #[test]
    fn a_longer_exempt_run_survives_whole() {
        assert_eq!(
            collapse_repeated_words("hyphen hyphen hyphen"),
            "hyphen hyphen hyphen"
        );
    }
}

#[cfg(test)]
mod after_a_short_answer_tests {
    use super::{FieldRules, apply_rules};
    use crate::config::CorrectionConfig;

    fn config() -> CorrectionConfig {
        CorrectionConfig {
            enabled: true,
            dictionary_path: String::new(),
            drop_fillers: false,
            filler_words: Vec::new(),
            collapse_repeats: false,
            spoken_punctuation: true,
            spoken_emoji: false,
            number_formatting: false,
            case_control: false,
        }
    }

    fn run(text: &str, preceding: Option<&str>) -> String {
        apply_rules(text, &config(), preceding, FieldRules::Prose)
    }

    /// What actually lands in the field when dictation carries on past a
    /// one-word answer. Recorded because the seam is the visible cost of the
    /// change, and a future reader should see it stated rather than discover it.
    #[test]
    fn dictating_on_after_a_one_word_answer() {
        // The answer itself: no full stop, which is the point.
        assert_eq!(run("yes", None), "Yes");
        // Carrying on starts a new sentence rather than running into the answer
        // in lowercase. "Yes We" has no full stop between the two, and govox
        // cannot reach back to add one -- it only emits the new text. The
        // alternative is worse: continuing yields "Yes we should ship it" with
        // no terminator anywhere.
        assert_eq!(run("we should ship it", Some("Yes")), " We should ship it.");
    }

    #[test]
    fn a_real_unfinished_sentence_is_still_continued() {
        assert_eq!(
            run("to Twillingate", Some("we drove out")),
            " to Twillingate"
        );
    }
}
