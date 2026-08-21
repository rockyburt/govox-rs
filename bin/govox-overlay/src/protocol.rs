//! The newline-delimited command protocol, parsed.
//!
//! Kept **byte-identical** to `govox-py`'s, which is what makes either helper
//! drivable by either daemon. That is not politeness about compatibility: it is
//! the single most useful debugging seam in the project, because a HUD that
//! misbehaves can be bisected between the two implementations by changing one
//! environment variable.
//!
//! The encoder lives in `govox-ui`; this is the decoder, and the pair are
//! checked against each other in `tests/protocol_round_trip.rs`.
// The renderer that consumes these is the remaining half of M12; until it
// lands most of them are referenced only by the tests below. `allow` rather
// than `expect` because which items count as dead shifts as the drawing code
// arrives, and an unfulfilled expectation would then be a build error that
// says nothing useful.
#![allow(dead_code)]

use crate::geometry::Rect;

/// One line from the daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A caret rectangle is coming; draw nothing until it lands or the wait
    /// expires.
    ExpectAnchor,
    /// Become visible. `pulse` is a synonym on the wire and always has been.
    Show,
    Hide,
    /// Sit under this rectangle, or `None` to release the anchor.
    Anchor(Option<Rect>),
    /// Microphone level, already clamped to 0..=1 by the sender.
    Level(f32),
    /// Interim transcript, or empty to clear it.
    Caption(String),
    /// Swap the card for the listening pill.
    Compact(bool),
    /// Draw a diagnostic box on the reported caret.
    CaretMarker(bool),
    /// Which mode the daemon is in, or `None` for ordinary dictation.
    Mode(Option<String>),
    Quit,
}

impl Command {
    /// Parse one line, or `None` for anything unrecognised.
    ///
    /// Unknown and malformed lines are **ignored rather than fatal**. This runs
    /// in a process whose whole job is to be dispensable: taking the HUD down
    /// because a newer daemon sent a command this build does not know would
    /// turn a forward-compatibility question into a visible failure.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        let (head, rest) = match line.split_once(' ') {
            Some((head, rest)) => (head, Some(rest)),
            None => (line, None),
        };
        match (head, rest) {
            ("expect-anchor", None) => Some(Self::ExpectAnchor),
            // `pulse` and `show` do the same thing. The daemon sends `pulse`
            // to re-assert liveness on a card that is already up.
            ("show" | "pulse", None) => Some(Self::Show),
            ("hide", None) => Some(Self::Hide),
            ("quit", None) => Some(Self::Quit),
            ("anchor", None) => Some(Self::Anchor(None)),
            ("anchor", Some(args)) => Some(Self::Anchor(parse_rect(args))),
            // Only a parsed value counts. Treating an unparseable level as 0
            // would drop the meter to silence on one malformed line.
            ("level", Some(value)) => value
                .trim()
                .parse::<f32>()
                .ok()
                .filter(|level| level.is_finite())
                .map(|level| Self::Level(level.clamp(0.0, 1.0))),
            // Bare `caption` clears it. Note the argument is *not* trimmed:
            // the caption is user text and its spacing is the sender's to
            // decide.
            ("caption", None) => Some(Self::Caption(String::new())),
            ("caption", Some(text)) => Some(Self::Caption(text.to_owned())),
            // Bare `mode` returns to ordinary dictation. An argument that is
            // not a single bare word is read as that same clear rather than
            // as a mode: a name this build does not know still paints
            // *something*, but a name that could not have been sent means the
            // stream is not saying what it appears to.
            ("mode", None) => Some(Self::Mode(None)),
            ("mode", Some(name)) => Some(Self::Mode(parse_mode(name))),
            ("compact", Some(flag)) => Some(Self::Compact(flag.trim() == "1")),
            ("caret-marker", Some(flag)) => Some(Self::CaretMarker(flag.trim() == "1")),
            _ => None,
        }
    }
}

/// One bare word of ASCII letters -> that mode; anything else -> `None`.
fn parse_mode(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic())).then(|| name.to_owned())
}

/// `x y w h` -> a rectangle; anything else -> `None`.
///
/// Malformed input falls back to the corner rather than raising. A HUD that
/// vanishes because a coordinate arrived as `nan` is worse than one that is
/// briefly in the wrong place.
fn parse_rect(args: &str) -> Option<Rect> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 4 {
        return None;
    }
    let mut values = [0_i32; 4];
    for (slot, part) in values.iter_mut().zip(&parts) {
        *slot = part.parse().ok()?;
    }
    Some(Rect::new(values[0], values[1], values[2], values[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simple_commands_round_trip() {
        assert_eq!(Command::parse("show"), Some(Command::Show));
        assert_eq!(Command::parse("hide"), Some(Command::Hide));
        assert_eq!(Command::parse("quit"), Some(Command::Quit));
        assert_eq!(Command::parse("expect-anchor"), Some(Command::ExpectAnchor));
    }

    #[test]
    fn pulse_is_a_synonym_for_show() {
        // Not a separate state: the daemon sends it to re-assert liveness on a
        // card that is already up, and the reference treats them in one branch.
        assert_eq!(Command::parse("pulse"), Some(Command::Show));
    }

    #[test]
    fn an_anchor_carries_four_integers_and_a_bare_one_releases_it() {
        assert_eq!(
            Command::parse("anchor 10 20 30 40"),
            Some(Command::Anchor(Some(Rect::new(10, 20, 30, 40))))
        );
        assert_eq!(Command::parse("anchor"), Some(Command::Anchor(None)));
    }

    #[test]
    fn a_malformed_anchor_releases_rather_than_guesses() {
        // Falling back to the corner is the safe direction: the alternative is
        // placing the card on a coordinate that came from a partial parse.
        for line in [
            "anchor 1 2 3",
            "anchor 1 2 3 4 5",
            "anchor a b c d",
            "anchor 1.5 2 3 4",
        ] {
            assert_eq!(Command::parse(line), Some(Command::Anchor(None)), "{line}");
        }
    }

    #[test]
    fn a_negative_anchor_is_accepted() {
        // Monitors left of or above the primary have negative origins, and a
        // caret on one is perfectly ordinary.
        assert_eq!(
            Command::parse("anchor -1920 -100 2 20"),
            Some(Command::Anchor(Some(Rect::new(-1920, -100, 2, 20))))
        );
    }

    #[test]
    fn a_level_is_clamped_to_the_meters_range() {
        assert_eq!(Command::parse("level 0.5"), Some(Command::Level(0.5)));
        assert_eq!(Command::parse("level 2"), Some(Command::Level(1.0)));
        assert_eq!(Command::parse("level -1"), Some(Command::Level(0.0)));
    }

    #[test]
    fn an_unparseable_level_is_dropped_rather_than_read_as_silence() {
        // The reference is explicit about this: flagging a live feed before
        // parsing would let one malformed line stop the fallback dot pulse for
        // the rest of the session while the meter stayed frozen.
        assert_eq!(Command::parse("level"), None);
        assert_eq!(Command::parse("level abc"), None);
        assert_eq!(
            Command::parse("level nan"),
            None,
            "NaN would poison the meter"
        );
        assert_eq!(Command::parse("level inf"), None);
    }

    #[test]
    fn a_caption_keeps_its_own_spacing_and_a_bare_one_clears() {
        assert_eq!(
            Command::parse("caption hello world"),
            Some(Command::Caption("hello world".to_owned()))
        );
        assert_eq!(
            Command::parse("caption"),
            Some(Command::Caption(String::new()))
        );
        // Trimming the argument would silently edit the user's transcript;
        // only the line's own trailing newline is removed.
        assert_eq!(
            Command::parse("caption  leading space"),
            Some(Command::Caption(" leading space".to_owned()))
        );
    }

    #[test]
    fn a_caption_may_contain_anything_that_is_not_a_newline() {
        // The protocol is newline-delimited and the sender strips newlines, so
        // everything else — including the words of other commands — is text.
        assert_eq!(
            Command::parse("caption quit hide level 3"),
            Some(Command::Caption("quit hide level 3".to_owned()))
        );
    }

    #[test]
    fn flags_are_one_or_not_one() {
        assert_eq!(Command::parse("compact 1"), Some(Command::Compact(true)));
        assert_eq!(Command::parse("compact 0"), Some(Command::Compact(false)));
        assert_eq!(
            Command::parse("caret-marker 1"),
            Some(Command::CaretMarker(true))
        );
        assert_eq!(
            Command::parse("caret-marker 0"),
            Some(Command::CaretMarker(false))
        );
    }

    #[test]
    fn a_mode_is_one_bare_word_and_a_bare_line_clears_it() {
        assert_eq!(
            Command::parse("mode command"),
            Some(Command::Mode(Some("command".to_owned())))
        );
        assert_eq!(Command::parse("mode"), Some(Command::Mode(None)));
    }

    #[test]
    fn a_mode_this_build_does_not_know_is_still_a_mode() {
        // Forward compatibility in the direction that matters: the daemon only
        // sends a name when it is *not* dictating, so decoding an unknown name
        // as "no mode" would assert the opposite of what was sent.
        assert_eq!(
            Command::parse("mode telepathy"),
            Some(Command::Mode(Some("telepathy".to_owned())))
        );
    }

    #[test]
    fn a_malformed_mode_clears_rather_than_guesses() {
        for line in ["mode two words", "mode com-mand", "mode 7", "mode   "] {
            assert_eq!(Command::parse(line), Some(Command::Mode(None)), "{line}");
        }
    }

    #[test]
    fn unknown_lines_are_ignored_rather_than_fatal() {
        // Forward compatibility: a newer daemon driving an older helper must
        // degrade to a HUD that ignores what it does not know, not to no HUD.
        assert_eq!(Command::parse("teleport 3"), None);
        assert_eq!(Command::parse(""), None);
        assert_eq!(Command::parse("   "), None);
        // Close-but-wrong arities, too.
        assert_eq!(Command::parse("show now"), None);
        assert_eq!(Command::parse("compact"), None);
    }
}
