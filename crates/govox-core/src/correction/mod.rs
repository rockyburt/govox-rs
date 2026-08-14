//! The correction pipeline: recognised text → corrected text plus an action.
//!
//! Ported from `correction/pipeline.py`. The **stage order is load-bearing**
//! and every constraint in `apply_rules` is justified where it is applied.

pub mod commands;
pub mod dictionary;
pub mod emoji;
pub mod grammar;
pub mod numbers;
pub mod punctuation;

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

/// Where capitals are never meaningful, all of them go. A hostname is
/// case-insensitive and conventionally lowercase, and so is an email address.
pub const LOWERCASE_WHOLE: &[&str] = &["URL", "EMAIL"];

/// In a terminal only the *command* is reliably lowercase. Arguments and paths
/// are case-sensitive and often deliberately capitalised — "cd Documents" must
/// survive — so only the first word is corrected.
pub const LOWERCASE_FIRST_WORD: &[&str] = &["TERMINAL"];

const SENTENCE_TERMINATORS: &[char] = &['.', '!', '?'];

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
}

pub struct CorrectionPipeline {
    pub config: CorrectionConfig,
    pub dictionary: PersonalDictionary,
    /// Lives in `[editing]`, not `[correction]`, because it governs how the
    /// daemon treats an utterance rather than how text is cleaned up.
    pub mode_switching: bool,
    /// The dictionary's patterns, compiled once at construction.
    compiled: dictionary::CompiledDictionary,
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
        }
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
            is_prose_field(purpose),
        );
        // Before replacements, so a replacement's own casing wins over this.
        corrected = undo_prose_casing(&corrected, purpose);
        corrected = self.compiled.apply(&corrected);

        let action =
            commands::detect_command(&corrected, self.mode_switching, context.command_mode);
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
    prose: bool,
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
    // Outside prose, none of what follows applies: a capital and a closing full
    // stop are wrong in a URL bar, and so is a separating space. Spoken
    // punctuation still works — saying "dot" is an explicit instruction, not an
    // assumption govox is making on the user's behalf.
    if !prose {
        return normalized;
    }

    // Continuing an unfinished sentence is not the same job as starting one.
    let continuing = is_continuation(preceding);
    if !continuing {
        normalized = sentence_case(&normalized);
    }
    if config.spoken_punctuation {
        normalized = punctuation::capitalize_after_terminators(&normalized);
    }
    if !continuing {
        normalized = ensure_terminal_punctuation(&normalized);
    }
    format!("{}{normalized}", separator_for(preceding))
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
    match purpose {
        None => true,
        Some(purpose) => !VERBATIM_PURPOSES.contains(&purpose),
    }
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
    !SENTENCE_TERMINATORS.contains(&last)
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

/// "the the dog" → "the dog"; case-insensitive comparison, keep the first form.
///
/// Needs a **backreference**, which the `regex` crate cannot express, so this is
/// the second of the two `fancy-regex` users. Deliberately *not* hand-rolled as
/// a token scan: `\b` means `"the, the"` is not collapsed, and reproducing that
/// by hand invites exactly the silent divergence the corpus exists to catch.
#[must_use]
pub fn collapse_repeated_words(text: &str) -> String {
    static PATTERN: LazyLock<FancyRegex> =
        LazyLock::new(|| FancyRegex::new(r"(?i)\b(\w+)(\s+\1\b)+").unwrap());
    punctuation::replace_all(&PATTERN, text, |caps| {
        caps.get(1).expect("first group").as_str().to_owned()
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
    // Strip horizontal whitespace only. A trailing newline is a break the
    // speaker asked for — "hello new line" should leave the caret on the next
    // line — and a plain strip would throw it away, turning a lone "new line"
    // into the empty string.
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
