//! `govox commands` — what the daemon understands, generated from the tables.
//!
//! Every line here is derived from the same constants the correction pipeline
//! matches against, so this listing cannot drift from the behaviour. Adding a
//! phrase to a table adds it here; that is the whole point of the module, and
//! the reason nothing below is a hand-written list.
//!
//! It also reports whether each group is *switched on*, because most of the
//! optional stages are off by default and "govox is ignoring me" is nearly
//! always that rather than a misremembered phrase.

use govox_core::config::Config;
use govox_core::correction::casing::{CASE_MARKERS, Mode, SWITCH_WORDS};
use govox_core::correction::commands::{COMMANDS, MODE_COMMANDS};
use govox_core::correction::emoji::SPOKEN_EMOJI;
use govox_core::correction::grammar::{
    DIRECTION_WORDS, EDGE_WORDS, PRESS_KEYS, SIMPLE_EDITS, UNIT_WORDS, VERB_OPS,
};
use govox_core::correction::numbers::CURRENCY;
use govox_core::correction::punctuation::{Attach, DETERMINERS, SPOKEN_PUNCTUATION};

/// Width the status note is right-aligned to. Narrow enough for an 80-column
/// terminal once the longest heading is in front of it.
const STATUS_COLUMN: usize = 52;

fn heading(out: &mut String, title: &str, status: &str) {
    let pad = STATUS_COLUMN.saturating_sub(title.chars().count());
    out.push_str(&format!("\n{title}{:pad$}{status}\n", "", pad = pad.max(2)));
}

fn on_off(enabled: bool, setting: &str) -> String {
    if enabled {
        "on".to_owned()
    } else {
        format!("off — set {setting}")
    }
}

/// Render a mark for display, since several of them are invisible.
fn show(mark: &str) -> String {
    match mark {
        "\n" => "(line break)".to_owned(),
        "\n\n" => "(blank line)".to_owned(),
        other => other.to_owned(),
    }
}

/// Group phrases that produce the same output, preserving table order.
fn grouped<'a, T: PartialEq>(rows: impl Iterator<Item = (&'a str, T)>) -> Vec<(T, Vec<&'a str>)> {
    let mut out: Vec<(T, Vec<&'a str>)> = Vec::new();
    for (phrase, value) in rows {
        match out.iter_mut().find(|(existing, _)| *existing == value) {
            Some((_, phrases)) => phrases.push(phrase),
            None => out.push((value, vec![phrase])),
        }
    }
    out
}

fn bullet(out: &mut String, left: &str, right: &str) {
    if right.is_empty() {
        out.push_str(&format!("  {left}\n"));
    } else {
        let pad = 34usize.saturating_sub(left.chars().count());
        out.push_str(&format!("  {left}{:pad$}{right}\n", "", pad = pad.max(2)));
    }
}

fn words<T: Copy>(table: &[(&str, T)]) -> String {
    table
        .iter()
        .map(|(word, _)| *word)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The whole listing for this configuration.
#[must_use]
pub fn render(config: &Config) -> String {
    let mut out = String::new();
    out.push_str("What govox understands. Everything is said as a whole utterance\n");
    out.push_str("unless noted; say it, then stop speaking.\n");

    heading(&mut out, "Editing", "always on");
    for (phrase, _) in SIMPLE_EDITS {
        bullet(&mut out, phrase, "");
    }

    heading(&mut out, "Motion and selection", "always on");
    bullet(&mut out, "<verb> <direction> [count] <unit>", "");
    bullet(&mut out, "  verb", &words(VERB_OPS));
    bullet(&mut out, "  direction", &words(DIRECTION_WORDS));
    bullet(&mut out, "  unit", &words(UNIT_WORDS));
    bullet(&mut out, "  for example", "\"delete previous three words\"");
    // The short forms are the point of the `kill` verb; a listing that only
    // ever shows the long one hides the reason it exists.
    bullet(&mut out, "  or, shorter", "\"kill last word\"");
    bullet(&mut out, "move to <edge> of [the] <unit>", "");
    bullet(&mut out, "  edge", &words(EDGE_WORDS));

    heading(&mut out, "Formatting", "always on");
    for (phrase, _) in COMMANDS {
        bullet(&mut out, phrase, "");
    }
    bullet(&mut out, "... start over", "restarts a streaming session");

    heading(&mut out, "Keys", "always on");
    bullet(&mut out, "press [the] <key> [key]", "");
    // Grouped so the several spellings of one key read as one row rather than
    // as several keys that happen to look alike.
    for (_, spellings) in grouped(PRESS_KEYS.iter().map(|(spoken, chord)| (*spoken, *chord))) {
        bullet(&mut out, &format!("  {}", spellings.join(", ")), "");
    }

    let mode = config.editing.command_mode;
    heading(
        &mut out,
        "Command mode",
        &on_off(mode, "[editing] command_mode = true"),
    );
    for (phrase, enabling) in MODE_COMMANDS {
        bullet(
            &mut out,
            phrase,
            if *enabling {
                "→ commands"
            } else {
                "→ dictation"
            },
        );
    }

    heading(
        &mut out,
        "Phrase editing (only while in command mode)",
        &on_off(mode, "[editing] command_mode = true"),
    );
    for phrase in [
        "replace <phrase> with <phrase>",
        "move before <phrase>",
        "move after <phrase>",
        "select <phrase>",
        "delete <phrase>",
    ] {
        bullet(&mut out, phrase, "");
    }

    let punctuation = config.correction.spoken_punctuation;
    heading(
        &mut out,
        "Spoken punctuation",
        &on_off(punctuation, "[correction] spoken_punctuation = true"),
    );
    // Keyed on the attachment as well as the mark, or phrases that produce the
    // same character in different places collapse into one misleading line:
    // "open quote" and "close quote" are both `"`, and "period" and "dot" are
    // both `.` but only one of them closes up against the next word.
    for ((mark, attach), phrases) in
        grouped(SPOKEN_PUNCTUATION.iter().map(|(p, m, a)| (*p, (*m, *a))))
    {
        let note = match attach {
            Attach::Right => format!("{}   leads the next word", show(mark)),
            Attach::Tight => format!("{}   closes up on both sides", show(mark)),
            Attach::Spaced => format!("{}   spaced", show(mark)),
            Attach::Break | Attach::Left => show(mark),
        };
        bullet(&mut out, &phrases.join(", "), &note);
    }
    bullet(&mut out, "  suppressed after", &DETERMINERS.join(", "));

    let emoji = config.correction.spoken_emoji;
    heading(
        &mut out,
        "Spoken emoji",
        &on_off(emoji, "[correction] spoken_emoji = true"),
    );
    for (character, phrases) in grouped(SPOKEN_EMOJI.iter().map(|(p, e)| (*p, *e))) {
        bullet(&mut out, &phrases.join(", "), character);
    }

    let case = config.correction.case_control;
    heading(
        &mut out,
        "Spoken case",
        &on_off(case, "[correction] case_control = true"),
    );
    for (marker, mode) in CASE_MARKERS {
        let effect = match mode {
            Mode::Upper => "CAPITALS",
            Mode::Lower => "lower case",
            Mode::Title => "First letter",
        };
        bullet(&mut out, &format!("{marker} <word>"), effect);
    }
    bullet(
        &mut out,
        &format!("<marker> {}", SWITCH_WORDS.join(" / ")),
        "opens and closes a span",
    );

    let numbers = config.correction.number_formatting;
    heading(
        &mut out,
        "Numbers",
        &on_off(numbers, "[correction] number_formatting = true"),
    );
    bullet(&mut out, "twenty five", "25");
    bullet(
        &mut out,
        &format!(
            "twenty five {}",
            CURRENCY.first().map_or("dollars", |(word, _)| *word)
        ),
        "$25",
    );
    bullet(&mut out, "fifty percent", "50%");
    bullet(
        &mut out,
        "  a lone number needs a unit",
        "\"I have one idea\" is left alone",
    );

    out
}

#[cfg(test)]
mod tests {
    use super::render;
    use govox_core::config::{Config, Environment};

    fn defaults() -> Config {
        Config::load_from(None, &Environment::default()).expect("defaults load")
    }

    /// The listing exists so a phrase cannot be understood but undocumented.
    /// Spot-check one entry from each table rather than all of them, which
    /// would just restate the tables here.
    #[test]
    fn every_table_reaches_the_listing() {
        let out = render(&defaults());
        for phrase in [
            "scratch that",          // SIMPLE_EDITS
            "extend selection",      // VERB_OPS
            "paragraph",             // UNIT_WORDS
            "new paragraph",         // COMMANDS
            "start command mode",    // MODE_COMMANDS
            "at sign",               // SPOKEN_PUNCTUATION, the new symbols
            "thumbs up",             // SPOKEN_EMOJI
            "all caps <word>",       // CASE_MARKERS
            "replace <phrase> with", // phrase editing
        ] {
            assert!(out.contains(phrase), "{phrase:?} missing from the listing");
        }
    }

    /// The listing's other job: saying why a phrase is being ignored.
    #[test]
    fn a_setting_that_is_off_says_how_to_turn_it_on() {
        let out = render(&defaults());
        assert!(out.contains("[correction] spoken_emoji = true"));
        assert!(out.contains("[correction] case_control = true"));
        assert!(out.contains("[editing] command_mode = true"));
    }

    #[test]
    fn a_setting_that_is_on_is_not_advertised_as_off() {
        let mut config = defaults();
        config.correction.spoken_emoji = true;
        let out = render(&config);
        assert!(!out.contains("[correction] spoken_emoji = true"));
    }

    /// An invisible mark has to be described, or the line reads as a blank.
    #[test]
    fn line_breaks_are_shown_by_name() {
        let out = render(&defaults());
        assert!(out.contains("(line break)"));
        assert!(out.contains("(blank line)"));
    }
}
