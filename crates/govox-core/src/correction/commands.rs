//! Classify an utterance: mode switch, formatting command, edit, or text.
//!
//! Ported from `correction/commands.py`.

use std::sync::LazyLock;

use regex::Regex;

use super::grammar::{match_edit, match_phrase_edit};
use crate::domain::PipelineAction;

pub const COMMANDS: &[(&str, &str)] = &[
    ("new line", "newline"),
    ("new paragraph", "new_paragraph"),
    // A command rather than the text " ", so it reaches a terminal the same way
    // "new line" does: the injector presses the key instead of typing a
    // character the clipboard path would have to carry.
    ("space bar", "space"),
];

/// Phrases that suspend and resume listening.
///
/// Only the two macOS says, and deliberately no shorter ones: "sleep" and
/// "wake" alone are ordinary words, and a phrase that suspends dictation is the
/// worst possible false positive — everything after it is silently discarded
/// until you work out why.
///
/// "go to sleep" is still a sentence someone could dictate, and since commands
/// are matched as a trailing suffix it can fire mid-utterance. That is the
/// accepted cost of the phrase macOS chose; waking is one phrase away and the
/// standing indicator says what happened.
pub const SLEEP_COMMANDS: &[(&str, bool)] = &[("go to sleep", true), ("wake up", false)];

/// Phrases that switch mode rather than producing text.
///
/// Several spellings per direction because this is the one command a user
/// reaches for when govox is *already* misbehaving, and having to remember the
/// exact wording then is a bad joke.
/// The short spellings on the dictation side are aliases, not replacements:
/// leaving command mode is the thing you want said in as few syllables as
/// possible, because it is what you reach for to get back to writing.
///
/// A mode phrase is matched in **both** modes, so adding one costs the ability
/// to ever dictate that phrase as text. That is what rules out "done" and
/// "stop", which are ordinary things to say alone; "dictate", "text mode" and
/// "type mode" are not.
pub const MODE_COMMANDS: &[(&str, bool)] = &[
    ("command mode", true),
    ("start command mode", true),
    ("start commands", true),
    ("lets command", true),
    ("let s command", true),
    ("let command", true),
    ("dictation mode", false),
    ("dictate", false),
    ("text mode", false),
    ("type mode", false),
    // "let's type" arrives here as "let s type": normalisation replaces the
    // apostrophe with a space rather than deleting it, so the contraction
    // splits into two tokens. All three spellings are listed because which one
    // the recogniser produces is not ours to decide — it may or may not punctuate
    // the contraction, and a phrase that works only when whisper felt like
    // adding an apostrophe is worse than no phrase at all.
    ("lets type", false),
    ("let s type", false),
    ("let type", false),
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

/// Strip everything that is not ASCII alphanumeric or whitespace, and collapse.
///
/// The same shaping [`normalize_command_text`] does, stopping short of folding
/// case. Only the tier 2 phrase grammar needs this: its replacement slot is
/// text destined for the user's document, so the capitals they spoke have to
/// survive the trip. Everything else compares against lower-case tables and
/// takes the folded form.
///
/// Punctuation is still stripped, and deliberately. `ensure_terminal_punctuation`
/// may already have put a full stop on the end of the utterance, so preserving
/// punctuation here would replace "the old file" with "the new file**.**" —
/// inserting a sentence-ending nobody spoke.
#[must_use]
pub fn normalize_preserving_case(text: &str) -> String {
    let words_only = NON_WORD.replace_all(text, " ");
    WHITESPACE.replace_all(&words_only, " ").trim().to_owned()
}

/// Strip everything that is not ASCII alphanumeric or whitespace, collapse, lowercase.
///
/// ASCII-only by construction, so accented characters are destroyed. Deliberate
/// for an English command grammar, and reproduced exactly: "café" normalises to
/// "caf", which is what decides whether an utterance is a command.
#[must_use]
pub fn normalize_command_text(text: &str) -> String {
    normalize_preserving_case(text).to_lowercase()
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

    // Before the mode phrases and before any text: while asleep this is the
    // only thing that will be honoured, so it must not be reachable only
    // through a path something else can claim first.
    if let Some(asleep) = lookup(SLEEP_COMMANDS, &normalized) {
        return PipelineAction::Sleep { asleep };
    }

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
    //
    // Matched against the case-preserving form, because "replace X with Y"
    // types Y into the document and lower-casing it there would make the
    // command unable to fix a name — the commonest reason to reach for it.
    if command_mode && let Some(edit) = match_phrase_edit(&normalize_preserving_case(text)) {
        return PipelineAction::Edit(edit);
    }

    PipelineAction::Text(text.to_owned())
}

/// The longest run of trailing words that is a command, and the text before it.
///
/// **Why this exists.** Every command matches as a whole utterance, and with
/// streaming on an "utterance" is the whole session — `finish_streaming` hands
/// over `session_text + tail`. So "delete that" said after anything else was
/// never a command, it was the last two words of a long string that matched
/// nothing. Streaming became the default in 0.2.0, which silently took every
/// whole-utterance command away from anyone dictating more than one phrase at a
/// time.
///
/// `start over` was the exception that showed the shape of the fix: it is
/// matched as a suffix precisely because it is said mid-flow, after other
/// words. This generalises that to the rest.
///
/// **Tier 2 is deliberately excluded** — `detect_command` is called with
/// `command_mode: false` here whatever the caller's mode. Those patterns take a
/// free-form slot, so `delete (?P<phrase>.+)` would match at the longest cut and
/// swallow the sentence in front of it as the thing to delete. Tier 1 is
/// bounded by its tables and cannot.
///
/// Returns `None` when no trailing command is found, when the whole text is one
/// (the caller has already tried that), or when there is nothing in front of
/// it to keep.
#[must_use]
pub fn split_trailing_command(
    text: &str,
    mode_switching: bool,
) -> Option<(String, PipelineAction)> {
    let starts = word_starts(text);
    // At least one word must remain in front, or this is the whole-utterance
    // case that `detect_command` already answered.
    let most = MAX_COMMAND_WORDS.min(starts.len().saturating_sub(1));

    // Longest first: "delete previous three words" must win over the "words"
    // that ends it, which is not a command but a shorter cut would test first.
    for count in (1..=most).rev() {
        let cut = starts[starts.len() - count];
        let action = detect_command(&text[cut..], mode_switching, false);
        if !matches!(action, PipelineAction::Text(_)) {
            let prefix = text[..cut].trim_end();
            if prefix.is_empty() {
                return None;
            }
            return Some((prefix.to_owned(), action));
        }
    }
    None
}

/// The longest tier 1 command, in words: "extend selection previous twenty five
/// words" and "move to beginning of the document" are both six. Eight leaves
/// room without letting the scan reach far enough back to be surprising.
const MAX_COMMAND_WORDS: usize = 8;

/// Byte offsets where each whitespace-separated word begins.
fn word_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut in_word = false;
    for (index, character) in text.char_indices() {
        if character.is_whitespace() {
            in_word = false;
        } else if !in_word {
            starts.push(index);
            in_word = true;
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::EditOp;

    fn replacement_for(text: &str) -> Option<String> {
        match detect_command(text, false, true) {
            PipelineAction::Edit(edit) => edit.replacement,
            other => panic!("{text} classified as {other:?}, not an edit"),
        }
    }

    #[test]
    fn the_replacement_survives_the_trip_with_its_capitals() {
        assert_eq!(
            replacement_for("replace rentsync with RentSync").as_deref(),
            Some("RentSync")
        );
    }

    #[test]
    fn a_sentence_cased_utterance_is_still_a_command() {
        // The correction pipeline runs first, so by the time an utterance
        // reaches here it may have been capitalised and given a full stop.
        assert_eq!(
            replacement_for("Replace the old file with the New File.").as_deref(),
            Some("the New File")
        );
    }

    #[test]
    fn the_terminal_full_stop_is_not_typed_into_the_document() {
        // Punctuation stays stripped precisely because of this: the pipeline's
        // own `ensure_terminal_punctuation` would otherwise have the command
        // insert a sentence ending nobody spoke.
        for text in [
            "replace the old file with the new file.",
            "replace the old file with the new file!",
            "replace the old file with the new file?",
        ] {
            assert_eq!(
                replacement_for(text).as_deref(),
                Some("the new file"),
                "{text}"
            );
        }
    }

    fn split(text: &str) -> Option<(String, PipelineAction)> {
        split_trailing_command(text, true)
    }

    #[test]
    fn a_command_said_after_other_words_is_still_a_command() {
        // The bug this exists for: with streaming on, the whole session arrives
        // as one string, so this is the ordinary case.
        let (prefix, action) = split("so i said hello command mode").expect("must split");
        assert_eq!(prefix, "so i said hello");
        assert_eq!(action, PipelineAction::Mode { command_mode: true });
    }

    #[test]
    fn the_longest_trailing_command_wins() {
        // A shorter cut would test "words" first, which is not a command; the
        // scan must reach the whole phrase.
        let (prefix, action) = split("here is the text delete previous three words").unwrap();
        assert_eq!(prefix, "here is the text");
        let PipelineAction::Edit(edit) = action else {
            panic!("expected an edit");
        };
        assert_eq!(edit.op, EditOp::DeleteUnit);
        assert_eq!(edit.count, 3);
    }

    #[test]
    fn punctuation_and_capitals_do_not_stop_it() {
        // By the time this runs the pipeline has sentence-cased the text and
        // added a full stop.
        let (prefix, action) = split("Hello there. Delete that.").unwrap();
        assert_eq!(prefix, "Hello there.");
        assert!(matches!(action, PipelineAction::Edit(_)));
    }

    #[test]
    fn every_kind_of_tier_one_command_is_reachable() {
        for (text, keep) in [
            ("some words new line", "some words"),
            ("some words space bar", "some words"),
            ("some words press enter", "some words"),
            ("some words press control s", "some words"),
            ("some words kill last word", "some words"),
            ("some words undo that", "some words"),
            ("some words dictate", "some words"),
            ("some words move to end of the document", "some words"),
        ] {
            let (prefix, _) = split(text).unwrap_or_else(|| panic!("{text} must split"));
            assert_eq!(prefix, keep, "{text}");
        }
    }

    #[test]
    fn ordinary_prose_is_left_alone() {
        for text in [
            "this is just a sentence",
            "i pressed the button",
            "the last word was hers",
            "we should select a venue",
        ] {
            assert_eq!(split(text), None, "{text} must stay text");
        }
    }

    #[test]
    fn the_whole_utterance_case_is_not_a_split() {
        // `detect_command` has already answered these; splitting them would
        // leave an empty prefix and type nothing.
        for text in ["command mode", "delete that", "press enter"] {
            assert_eq!(split(text), None, "{text}");
        }
    }

    #[test]
    fn a_free_form_phrase_edit_cannot_swallow_the_sentence() {
        // Tier 2 is excluded from the scan whatever the caller's mode. Were it
        // not, `delete (?P<phrase>.+)` would match at the longest cut and take
        // the words in front of it as the thing to delete.
        let text = "the quick brown fox delete the old draft";
        let split = split_trailing_command(text, true);
        // "delete the old draft" is not a tier 1 command, so nothing fires.
        assert_eq!(split, None);
    }

    #[test]
    fn the_scan_does_not_reach_further_back_than_a_command_can_be() {
        // Nine words of prose ending in a word that appears in a command table
        // must not be searched to the start of the string.
        let text = "one two three four five six seven eight nine words";
        assert_eq!(split(text), None);
    }

    #[test]
    fn space_bar_is_a_command_not_text() {
        assert_eq!(
            detect_command("space bar", false, false),
            PipelineAction::Command("space".to_owned())
        );
    }

    #[test]
    fn the_short_aliases_return_to_dictation() {
        for phrase in ["dictate", "text mode", "type mode", "dictation mode"] {
            assert_eq!(
                detect_command(phrase, true, true),
                PipelineAction::Mode {
                    command_mode: false
                },
                "{phrase}"
            );
        }
    }

    #[test]
    fn the_contraction_is_matched_however_it_was_punctuated() {
        // The apostrophe becomes a space, not nothing, so "let's type" and
        // "lets type" reach the table as different strings. Both must land.
        for phrase in ["let's type", "lets type", "let type", "Let's type."] {
            assert_eq!(
                detect_command(phrase, true, true),
                PipelineAction::Mode {
                    command_mode: false
                },
                "{phrase}"
            );
        }
        for phrase in ["let's command", "lets command", "let command"] {
            assert_eq!(
                detect_command(phrase, true, false),
                PipelineAction::Mode { command_mode: true },
                "{phrase}"
            );
        }
    }

    #[test]
    fn an_alias_is_heard_from_dictation_mode_too() {
        // Mode phrases are looked up regardless of which mode is current, which
        // is the reason the alias had to be a phrase nobody dictates as text.
        assert_eq!(
            detect_command("dictate", true, false),
            PipelineAction::Mode {
                command_mode: false
            }
        );
    }

    #[test]
    fn an_alias_is_inert_with_mode_switching_off() {
        // `[editing] command_mode = false` must leave no dormant phrase.
        assert!(matches!(
            detect_command("dictate", false, false),
            PipelineAction::Text(_)
        ));
    }

    #[test]
    fn only_the_bare_alias_switches_mode() {
        // Whole-utterance, so an alias inside a sentence is still just words.
        for phrase in ["dictate this", "i will dictate", "text mode is nice"] {
            assert!(
                matches!(detect_command(phrase, true, false), PipelineAction::Text(_)),
                "{phrase}"
            );
        }
    }

    #[test]
    fn tier_two_stays_shut_outside_command_mode() {
        // The case-preserving form is only ever reached behind this gate.
        assert!(matches!(
            detect_command("replace rocky with Rocky", false, false),
            PipelineAction::Text(_)
        ));
    }
}
