//! Classify an utterance: mode switch, formatting command, edit, or text.
//!
//! Ported from `correction/commands.py`.

use std::sync::LazyLock;

use regex::Regex;

use super::grammar::{match_edit, match_phrase_edit};
use crate::domain::PipelineAction;

pub const COMMANDS: &[(&str, &str)] =
    &[("new line", "newline"), ("new paragraph", "new_paragraph")];

/// Phrases that switch mode rather than producing text.
///
/// Several spellings per direction because this is the one command a user
/// reaches for when govox is *already* misbehaving, and having to remember the
/// exact wording then is a bad joke.
pub const MODE_COMMANDS: &[(&str, bool)] = &[
    ("command mode", true),
    ("start command mode", true),
    ("start commands", true),
    ("dictation mode", false),
    ("stop command mode", false),
    ("exit command mode", false),
    ("stop commands", false),
];

/// The punctuation stage rewrites a spoken "new line" into a real newline
/// before this runs, so a whole-utterance break arrives as the character, not
/// the phrase. Routing it back to a command keeps the injector pressing Enter
/// rather than typing a `\n` — the more reliable path, and the only one the
/// clipboard injector can honour.
pub const BREAK_COMMANDS: &[(&str, &str)] = &[("\n", "newline"), ("\n\n", "new_paragraph")];

/// "Start over" discards the streaming session so far and begins again.
///
/// Matched as a **suffix**, unlike every other command, because that is how it
/// gets said: you are mid-sentence, change your mind, and finish with "no,
/// start over". Requiring the whole utterance to match would never fire.
///
/// The leading boundary is load-bearing: without it "restart over" would match.
static RESTART: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|\s)(?:start over|start again)$").unwrap());

/// Does this hypothesis end with a request to start the session over?
///
/// Trailing punctuation is ignored, since the correction pipeline may already
/// have added a full stop by the time this is asked.
#[must_use]
pub fn is_restart_request(text: &str) -> bool {
    let trimmed = text.trim();
    let cleaned = trimmed.trim_end_matches(['.', '!', '?', ',', ';', ':', ' ']);
    RESTART.is_match(cleaned)
}

static NON_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9\s]").unwrap());
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// Strip everything that is not ASCII alphanumeric or whitespace, collapse, lowercase.
///
/// ASCII-only by construction, so accented characters are destroyed. Deliberate
/// for an English command grammar, and reproduced exactly: "café" normalises to
/// "caf", which is what decides whether an utterance is a command.
#[must_use]
pub fn normalize_command_text(text: &str) -> String {
    let words_only = NON_WORD.replace_all(text, " ");
    WHITESPACE
        .replace_all(&words_only, " ")
        .trim()
        .to_lowercase()
}

fn lookup<T: Copy>(table: &[(&str, T)], key: &str) -> Option<T> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Classify an utterance.
///
/// Formatting commands are checked before editing commands so the pre-existing
/// "new line" / "new paragraph" behaviour is unchanged.
///
/// `mode_switching` is off unless `[editing] command_mode` is enabled, so with
/// the feature disabled "command mode" dictates as ordinary text — no dormant
/// phrase can surprise someone who never turned it on.
#[must_use]
pub fn detect_command(text: &str, mode_switching: bool, command_mode: bool) -> PipelineAction {
    if let Some(name) = lookup(BREAK_COMMANDS, text.trim_matches([' ', '\t'])) {
        return PipelineAction::Command(name.to_owned());
    }

    let normalized = normalize_command_text(text);

    if mode_switching && let Some(mode) = lookup(MODE_COMMANDS, &normalized) {
        return PipelineAction::Mode { command_mode: mode };
    }

    // Still reachable with `spoken_punctuation = false`, where the punctuation
    // stage never runs and the phrase arrives intact.
    if let Some(command) = lookup(COMMANDS, &normalized) {
        return PipelineAction::Command(command.to_owned());
    }

    if let Some(edit) = match_edit(&normalized) {
        return PipelineAction::Edit(edit);
    }

    // Tier 2 last, and only in command mode. These take a free-form slot, so
    // "delete the old draft" would otherwise stop being a sentence someone can
    // dictate.
    if command_mode && let Some(edit) = match_phrase_edit(&normalized) {
        return PipelineAction::Edit(edit);
    }

    PipelineAction::Text(text.to_owned())
}
