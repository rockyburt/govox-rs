//! User-defined commands, from the config file's `commands` entries.
//!
//! The fourth and last piece taken from macOS Voice Control's command model.
//! Everything else in this module tree is a compile-time table; this is the one
//! place where what govox will act on is decided at load time by the user.
//!
//! Three decisions shape it.
//!
//! **Built-ins always win.** A custom command is consulted only after
//! [`super::commands::detect_command`] has declined, so no config file can take
//! "delete that" away from the person who wrote it. A phrase that would shadow
//! a built-in is not silently outranked either — [`validate`] reports it at
//! load, because a command that is *accepted* and then never fires is the worst
//! of the three possible behaviours.
//!
//! **No new action type.** `insert` resolves to [`PipelineAction::Text`] and
//! `press` to the same [`EditOp::PressKey`] the built-in `press <key>` grammar
//! produces, so custom commands travel the paths that already carry text and
//! keystrokes rather than a parallel one that could rot. In particular they
//! inherit the password-field refusal and the asleep guard for free, which a
//! new variant would have had to remember to ask for.
//!
//! **Matching is the built-in normalization, unchanged.** Custom phrases are
//! folded by [`super::commands::normalize_command_text`], so "Let's Go!" and
//! "lets go" are the same phrase to a custom command for exactly the reasons
//! they are the same phrase to a built-in one. A second normalizer that agreed
//! in the common cases and diverged on apostrophes would be a bug nobody could
//! reproduce deliberately.

use crate::caret::app_label_matches;
use crate::config::CustomCommand;
use crate::domain::{EditAction, EditOp, PipelineAction};
use crate::keycodes::parse_chord;

use super::commands::{detect_command, normalize_command_text};
use super::grammar::{CHORD_KEYS, MODIFIER_WORDS, PRESS_KEYS};

/// The action for the first custom command whose phrase and scope both match.
///
/// `app` is the focused window's label, as `active_window` reports it, or
/// `None` when it could not be read. An unnamed window matches only the
/// commands that ask for no application — the same refusal the overlay's app
/// rules make, and for the same reason: a scoped command that fires in a window
/// nobody could identify is a keystroke sent somewhere the user did not aim it.
#[must_use]
pub fn match_custom(
    text: &str,
    commands: &[CustomCommand],
    app: Option<&str>,
) -> Option<PipelineAction> {
    if commands.is_empty() {
        return None;
    }
    let spoken = normalize_command_text(text);
    if spoken.is_empty() {
        return None;
    }
    commands
        .iter()
        .find(|command| {
            normalize_command_text(&command.phrase) == spoken && scope_allows(command, app)
        })
        .and_then(action_for)
}

/// The longest run of trailing words that is a custom command, and what precedes.
///
/// The exact counterpart of [`super::commands::split_trailing_command`], and it
/// exists for the same reason: with streaming on, an "utterance" is the whole
/// session, so a command said after other words is never the whole string.
/// Without this a custom command would work in a one-phrase session and
/// silently stop working the moment the user said anything before it — which is
/// the bug that shipped for the built-ins in 0.2.0 and is worth not repeating.
///
/// Shares `word_starts` and the word cap with the built-in scan rather than
/// re-deriving them: two ideas of where a word begins would disagree on exactly
/// the inputs nobody thinks to test.
#[must_use]
pub fn split_trailing_custom(
    text: &str,
    commands: &[CustomCommand],
    app: Option<&str>,
) -> Option<(String, PipelineAction)> {
    if commands.is_empty() {
        return None;
    }
    let starts = super::commands::word_starts(text);
    // `saturating_sub(1)` leaves at least one word in front: a command that is
    // the *whole* utterance is `match_custom`'s to find, and returning an empty
    // prefix here would inject a stray separator ahead of it.
    let most = super::commands::MAX_COMMAND_WORDS.min(starts.len().saturating_sub(1));
    for count in (1..=most).rev() {
        let cut = starts[starts.len() - count];
        if let Some(action) = match_custom(&text[cut..], commands, app) {
            return Some((text[..cut].trim_end().to_owned(), action));
        }
    }
    None
}

/// Rewrite a configured chord into the spelling `keycodes` knows.
///
/// The rule is: **a `press` accepts anything you could say out loud, plus
/// anything the keycode table already names.** So `"control+s"`, `"ctrl+s"`,
/// `"command+s"` and `"meta+s"` all reach the same keystroke, because the first
/// and third are what the spoken `press` grammar accepts and the second and
/// fourth are what the table calls them.
///
/// Without this the config would take a *third* vocabulary — neither the words
/// govox teaches you to say nor the ones it prints — and "control+s" would be
/// rejected as an unknown key by a program that understands "press control s"
/// perfectly. Parts that match nothing are passed through untouched, so
/// `parse_chord` is still the one thing that decides what a valid key is.
#[must_use]
fn canonical_chord(chord: &str) -> String {
    chord
        .split('+')
        .map(|part| {
            let part = part.trim().to_lowercase();
            let spoken = |table: &[(&str, &'static str)]| {
                table
                    .iter()
                    .find(|(word, _)| *word == part)
                    .map(|(_, key)| *key)
            };
            spoken(MODIFIER_WORDS)
                .or_else(|| spoken(PRESS_KEYS))
                .or_else(|| spoken(CHORD_KEYS))
                .map_or(part, str::to_owned)
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Whether a command's `while_using` scope admits the focused window.
fn scope_allows(command: &CustomCommand, app: Option<&str>) -> bool {
    match command.app.as_deref() {
        None => true,
        Some(pattern) => app.is_some_and(|label| app_label_matches(pattern, label)),
    }
}

/// The action one command performs, or `None` if it is misconfigured.
///
/// Returning `None` rather than a best guess is the whole discipline here: a
/// command with both `insert` and `press`, or with a chord this build cannot
/// parse, has an *ambiguous* intent, and guessing at one would put text or a
/// keystroke into the user's document on the strength of a coin flip.
/// [`validate`] reports the same cases at load, so the silence here is a
/// backstop rather than the only signal.
fn action_for(command: &CustomCommand) -> Option<PipelineAction> {
    match (command.insert.as_deref(), command.press.as_deref()) {
        (Some(text), None) => Some(PipelineAction::Text(text.to_owned())),
        (None, Some(chord)) => {
            let chord = canonical_chord(chord);
            parse_chord(&chord).ok().map(|_| {
                PipelineAction::Edit(EditAction {
                    phrase: Some(chord),
                    ..EditAction::simple(EditOp::PressKey)
                })
            })
        }
        _ => None,
    }
}

/// Everything wrong with the configured commands, one message each.
///
/// Warnings rather than a failed startup, for the reason retired keys are:
/// refusing to dictate at all because one of eleven custom commands has a typo
/// in its chord is a worse outcome than dictating with ten of them.
///
/// The checks are ordered so the *first* thing wrong with a command is the one
/// reported, rather than a cascade — a phrase that is empty produces one
/// message, not also "shadows a built-in" and "is a duplicate".
#[must_use]
pub fn validate(commands: &[CustomCommand]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen: Vec<(String, Option<String>)> = Vec::new();

    for command in commands {
        let phrase = normalize_command_text(&command.phrase);
        let quoted = &command.phrase;

        if phrase.is_empty() {
            problems
                .push("a command has an empty when_i_say, so nothing can ever say it".to_owned());
            continue;
        }

        match (command.insert.is_some(), command.press.is_some()) {
            (false, false) => {
                problems.push(format!(
                    "{quoted:?} has neither insert nor press, so it would do nothing"
                ));
                continue;
            }
            (true, true) => {
                problems.push(format!(
                    "{quoted:?} has both insert and press; give exactly one, \
                     because which of the two was meant cannot be guessed"
                ));
                continue;
            }
            _ => {}
        }

        if let Some(chord) = command.press.as_deref()
            && let Err(why) = parse_chord(&canonical_chord(chord))
        {
            // The parser's own message names the offending part, which is what
            // makes a typo in a nine-key chord fixable without bisecting it.
            problems.push(format!("{quoted:?} presses {chord:?}: {why}"));
            continue;
        }

        // Mode switching on, command mode off: the widest set a built-in can
        // occupy, so a phrase cleared here is clear in every mode.
        if !matches!(
            detect_command(&phrase, true, false),
            PipelineAction::Text(_)
        ) {
            problems.push(format!(
                "{quoted:?} is already a built-in command, which always wins; \
                 the custom one would never fire, so give it another phrase"
            ));
            continue;
        }

        let key = (phrase, command.app.clone());
        if seen.contains(&key) {
            problems.push(format!(
                "{quoted:?} is defined more than once for the same application; \
                 only the first would ever fire"
            ));
            continue;
        }
        seen.push(key);
    }

    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(phrase: &str, text: &str) -> CustomCommand {
        CustomCommand {
            phrase: phrase.to_owned(),
            insert: Some(text.to_owned()),
            press: None,
            app: None,
        }
    }

    fn press(phrase: &str, chord: &str) -> CustomCommand {
        CustomCommand {
            phrase: phrase.to_owned(),
            insert: None,
            press: Some(chord.to_owned()),
            app: None,
        }
    }

    #[test]
    fn an_insert_command_types_its_text() {
        let commands = vec![insert("sign off", "Best regards,\nRocky")];
        assert_eq!(
            match_custom("sign off", &commands, None),
            Some(PipelineAction::Text("Best regards,\nRocky".to_owned()))
        );
    }

    #[test]
    fn a_press_command_becomes_the_same_action_the_built_in_grammar_makes() {
        // Sharing `EditOp::PressKey` is what makes a custom chord inherit the
        // injector's handling of one, rather than needing its own.
        let commands = vec![press("save it", "control+s")];
        let action = match_custom("save it", &commands, None).expect("matched");
        let PipelineAction::Edit(edit) = action else {
            panic!("a press command must produce an edit");
        };
        assert_eq!(edit.op, EditOp::PressKey);
        // Rewritten to the table's spelling on the way through, so the
        // injector never sees a word only the spoken grammar knows.
        assert_eq!(edit.phrase.as_deref(), Some("ctrl+s"));
    }

    #[test]
    fn a_phrase_matches_with_the_same_folding_a_built_in_uses() {
        // Not a nicety: the recogniser decides capitalisation and trailing
        // punctuation on its own, so an exact-string match would make a custom
        // command fire only when Whisper happened to agree with the config.
        let commands = vec![insert("let's go", "away")];
        for spoken in ["let's go", "Let's go", "LET'S GO", "let's go."] {
            assert_eq!(
                match_custom(spoken, &commands, None),
                Some(PipelineAction::Text("away".to_owned())),
                "{spoken}"
            );
        }
    }

    #[test]
    fn a_chord_may_be_written_the_way_it_is_spoken() {
        // The config must not need a third vocabulary. Someone who has read
        // `govox commands` knows "press control s"; someone who has read the
        // keycode table knows "ctrl". Both spellings have to work, or the
        // feature is gated on guessing which one this field wanted.
        for chord in ["control+s", "ctrl+s", "Control + S"] {
            let commands = [press("do it", chord)];
            assert_eq!(validate(&commands), Vec::<String>::new(), "{chord}");
            let Some(PipelineAction::Edit(edit)) = match_custom("do it", &commands, None) else {
                panic!("{chord} did not resolve");
            };
            assert_eq!(edit.phrase.as_deref(), Some("ctrl+s"), "{chord}");
        }
    }

    #[test]
    fn the_macos_spelling_of_a_modifier_works_too() {
        // "command" is what a macOS user would write, and the spoken grammar
        // already accepts it. Sending it to the injector unchanged would press
        // nothing at all.
        let commands = [press("do it", "command+shift+p")];
        assert_eq!(validate(&commands), Vec::<String>::new());
        let Some(PipelineAction::Edit(edit)) = match_custom("do it", &commands, None) else {
            panic!("did not resolve");
        };
        assert_eq!(edit.phrase.as_deref(), Some("meta+shift+p"));
    }

    #[test]
    fn a_scoped_command_fires_only_in_its_application() {
        let commands = vec![CustomCommand {
            app: Some("chrome".to_owned()),
            ..press("save it", "control+s")
        }];
        assert!(match_custom("save it", &commands, Some("Google Chrome / Inbox")).is_some());
        assert!(match_custom("save it", &commands, Some("GNOME Terminal")).is_none());
    }

    #[test]
    fn a_scoped_command_does_not_fire_in_a_window_we_could_not_name() {
        // The same refusal the overlay's app rules make. Firing a keystroke
        // scoped to one application in a window nobody could identify sends it
        // somewhere the user did not aim it.
        let commands = vec![CustomCommand {
            app: Some("chrome".to_owned()),
            ..press("save it", "control+s")
        }];
        assert!(match_custom("save it", &commands, None).is_none());
        assert!(match_custom("save it", &commands, Some("")).is_none());
    }

    #[test]
    fn an_unscoped_command_fires_in_a_window_we_could_not_name() {
        let commands = vec![insert("sign off", "bye")];
        assert!(match_custom("sign off", &commands, None).is_some());
    }

    #[test]
    fn a_more_specific_scope_can_be_put_first() {
        // Order is the only precedence rule, and it is the config's to set —
        // the same as the app rules, where the first match wins.
        let commands = vec![
            CustomCommand {
                app: Some("chrome".to_owned()),
                ..insert("go home", "chrome")
            },
            insert("go home", "anywhere"),
        ];
        assert_eq!(
            match_custom("go home", &commands, Some("Google Chrome")),
            Some(PipelineAction::Text("chrome".to_owned()))
        );
        assert_eq!(
            match_custom("go home", &commands, Some("Terminal")),
            Some(PipelineAction::Text("anywhere".to_owned()))
        );
    }

    #[test]
    fn ordinary_speech_matches_nothing() {
        let commands = vec![insert("sign off", "bye")];
        assert!(match_custom("sign off the paperwork", &commands, None).is_none());
        assert!(match_custom("", &commands, None).is_none());
    }

    #[test]
    fn no_commands_configured_is_the_cheap_path() {
        assert!(match_custom("anything at all", &[], None).is_none());
    }

    // --- the trailing scan --------------------------------------------------

    #[test]
    fn a_custom_command_said_after_other_words_is_still_found() {
        // The whole reason this exists: with streaming on, an utterance is the
        // entire session, so a command is almost never the whole string.
        let commands = vec![press("save it", "ctrl+s")];
        let (prefix, action) =
            split_trailing_custom("here is the paragraph save it", &commands, None)
                .expect("found the command");
        assert_eq!(prefix, "here is the paragraph");
        let PipelineAction::Edit(edit) = action else {
            panic!("expected a keypress");
        };
        assert_eq!(edit.phrase.as_deref(), Some("ctrl+s"));
    }

    #[test]
    fn the_trailing_scan_leaves_a_whole_utterance_command_alone() {
        // That one is `match_custom`'s, and returning an empty prefix here
        // would inject a stray separator in front of the action.
        let commands = vec![insert("sign off", "bye")];
        assert!(split_trailing_custom("sign off", &commands, None).is_none());
    }

    #[test]
    fn the_trailing_scan_respects_the_application_scope() {
        let commands = vec![CustomCommand {
            app: Some("chrome".to_owned()),
            ..press("save it", "ctrl+s")
        }];
        let scan = |label| split_trailing_custom("some words save it", &commands, Some(label));
        assert!(scan("Google Chrome").is_some());
        assert!(scan("GNOME Terminal").is_none());
    }

    #[test]
    fn ordinary_prose_survives_the_trailing_scan() {
        let commands = vec![insert("sign off", "bye")];
        assert!(
            split_trailing_custom("i need to sign off the paperwork", &commands, None).is_none()
        );
    }

    // --- validation ---------------------------------------------------------

    #[test]
    fn a_well_formed_set_reports_nothing() {
        let commands = vec![insert("sign off", "bye"), press("save it", "control+s")];
        assert_eq!(validate(&commands), Vec::<String>::new());
    }

    #[test]
    fn a_phrase_that_shadows_a_built_in_is_reported() {
        // The command would be accepted and then never fire, which is the one
        // outcome a user cannot debug from the outside.
        let problems = validate(&[insert("delete that", "nothing")]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("built-in"), "{}", problems[0]);
    }

    #[test]
    fn a_mode_phrase_is_a_built_in_too() {
        // Mode switching is a setting, so "command mode" is a built-in only
        // sometimes. Validation asks with it on, which is the widest set — a
        // phrase cleared here cannot be shadowed by a later config edit.
        let problems = validate(&[insert("command mode", "nope")]);
        assert_eq!(problems.len(), 1, "{problems:?}");
    }

    #[test]
    fn a_command_with_no_action_is_reported() {
        let problems = validate(&[CustomCommand {
            phrase: "do it".to_owned(),
            insert: None,
            press: None,
            app: None,
        }]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("neither"), "{}", problems[0]);
    }

    #[test]
    fn a_command_with_two_actions_is_reported_rather_than_resolved() {
        let commands = [CustomCommand {
            press: Some("control+s".to_owned()),
            ..insert("do it", "text")
        }];
        let problems = validate(&commands);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("both"), "{}", problems[0]);
        // And it does nothing at runtime, rather than picking one.
        assert!(match_custom("do it", &commands, None).is_none());
    }

    #[test]
    fn an_unparseable_chord_is_reported_rather_than_swallowed() {
        // `ydotool key <name>` reports success for a key it does not have, so
        // a chord that cannot be parsed must be caught here or it becomes a
        // command that silently does nothing for ever.
        let problems = validate(&[press("do it", "control+nonsense")]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("control+nonsense"), "{}", problems[0]);
    }

    #[test]
    fn an_empty_phrase_is_reported() {
        let problems = validate(&[insert("   ", "bye")]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("empty"), "{}", problems[0]);
    }

    #[test]
    fn a_duplicate_phrase_is_reported_but_the_same_phrase_in_two_apps_is_not() {
        let dupes = validate(&[insert("sign off", "one"), insert("sign off", "two")]);
        assert_eq!(dupes.len(), 1);
        assert!(dupes[0].contains("more than once"), "{}", dupes[0]);

        let scoped = validate(&[
            CustomCommand {
                app: Some("chrome".to_owned()),
                ..insert("sign off", "one")
            },
            insert("sign off", "two"),
        ]);
        assert_eq!(scoped, Vec::<String>::new());
    }

    #[test]
    fn one_bad_command_is_reported_once_rather_than_as_a_cascade() {
        // A message per fault per command would bury the actionable one.
        let problems = validate(&[CustomCommand {
            phrase: String::new(),
            insert: Some("a".to_owned()),
            press: Some("b".to_owned()),
            app: None,
        }]);
        assert_eq!(problems.len(), 1, "{problems:?}");
    }
}
