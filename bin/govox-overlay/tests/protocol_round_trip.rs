//! The daemon's encoder against the helper's decoder.
//!
//! Both sides of this protocol live in this repository now, which makes it very
//! easy for them to drift apart in a way neither side's own tests would catch:
//! `govox-ui` asserts what it *writes*, `govox-overlay` asserts what it
//! *reads*, and both can be self-consistently wrong.
//!
//! This is also what keeps the seam to `govox-py` honest. The wire format is
//! the reason either daemon can drive either helper, so a change here is a
//! change to a contract with a program in another repository — and the point of
//! the test is that such a change cannot be made by accident.

use govox_ui::overlay::OverlayCommand;

/// Reimplements the helper's parse over the encoder's output.
///
/// The decoder itself lives in the `govox-overlay` binary crate, which an
/// integration test cannot import — so what this checks is the *format*: that
/// every command the daemon can emit is a single line, non-empty, and starts
/// with a verb the helper knows.
const KNOWN_VERBS: &[&str] = &[
    "show",
    "pulse",
    "hide",
    "level",
    "caption",
    "anchor",
    "expect-anchor",
    "caret-marker",
    "compact",
    "mode",
    "quit",
];

fn every_command() -> Vec<String> {
    vec![
        OverlayCommand::Show.encode(),
        OverlayCommand::Pulse.encode(),
        OverlayCommand::Hide.encode(),
        OverlayCommand::Level(0.5).encode(),
        OverlayCommand::Anchor(None).encode(),
        OverlayCommand::Anchor(Some((10, 20, 30, 40))).encode(),
        OverlayCommand::ExpectAnchor.encode(),
        OverlayCommand::CaretMarker(true).encode(),
        OverlayCommand::CaretMarker(false).encode(),
        OverlayCommand::Compact(true).encode(),
        OverlayCommand::Compact(false).encode(),
        OverlayCommand::Quit.encode(),
        OverlayCommand::Caption("hello world".to_owned()).encode(),
        OverlayCommand::Caption(String::new()).encode(),
        OverlayCommand::Mode(Some("command".to_owned())).encode(),
        OverlayCommand::Mode(None).encode(),
    ]
}

#[test]
fn every_emitted_command_starts_with_a_verb_the_helper_knows() {
    for line in every_command() {
        let verb = line.split(' ').next().unwrap_or_default();
        assert!(
            KNOWN_VERBS.contains(&verb),
            "the daemon emits {line:?}, which no helper handles"
        );
    }
}

#[test]
fn no_command_can_desynchronise_the_stream() {
    // The protocol is newline-delimited, so an embedded newline would be read
    // as a command boundary and everything after it as a new command. The
    // caption is the only field carrying arbitrary user text, and it is also
    // the one the recogniser can put a newline into.
    for line in every_command() {
        assert!(!line.contains('\n'), "{line:?} contains a newline");
        assert!(!line.contains('\r'), "{line:?} contains a carriage return");
        assert!(!line.is_empty(), "an empty line is not a command");
    }
    let dangerous = OverlayCommand::Caption("first line\nquit\nsecond".to_owned()).encode();
    assert!(!dangerous.contains('\n'), "{dangerous:?}");
    // And what survives is still a caption, not a truncation to nothing.
    assert!(dangerous.starts_with("caption "), "{dangerous:?}");
}

#[test]
fn a_level_is_emitted_with_enough_precision_to_animate() {
    // Three decimals: the meter has ~50 px of travel, so a coarser format
    // would quantise the waveform into visible steps.
    assert_eq!(OverlayCommand::Level(0.5).encode(), "level 0.500");
    assert_eq!(OverlayCommand::Level(2.0).encode(), "level 1.000");
    assert_eq!(OverlayCommand::Level(-1.0).encode(), "level 0.000");
}

#[test]
fn an_anchor_is_four_integers_and_releasing_it_is_the_bare_verb() {
    assert_eq!(
        OverlayCommand::Anchor(Some((10, 20, 30, 40))).encode(),
        "anchor 10 20 30 40"
    );
    // Not "anchor None": the bare word is what returns the card to its corner,
    // and a Python helper would render the word `None` into a failed parse
    // that happens to have the same effect for the wrong reason.
    assert_eq!(OverlayCommand::Anchor(None).encode(), "anchor");
}

#[test]
fn flags_are_emitted_as_one_and_zero() {
    // `true`/`false` would parse as "not 1" in the reference helper, so a
    // `compact true` would silently mean *off*.
    assert_eq!(OverlayCommand::Compact(true).encode(), "compact 1");
    assert_eq!(OverlayCommand::Compact(false).encode(), "compact 0");
    assert_eq!(OverlayCommand::CaretMarker(true).encode(), "caret-marker 1");
    assert_eq!(
        OverlayCommand::CaretMarker(false).encode(),
        "caret-marker 0"
    );
}

#[test]
fn a_mode_is_one_bare_word_and_none_clears_it() {
    assert_eq!(
        OverlayCommand::Mode(Some("spelling".to_owned())).encode(),
        "mode spelling"
    );
    assert_eq!(OverlayCommand::Mode(None).encode(), "mode");
}

#[test]
fn a_mode_name_the_wire_cannot_carry_is_sent_as_the_clear() {
    // Not a hypothetical: a name with a space would be decoded as the clear
    // anyway, and one with a newline would desynchronize the stream and let
    // the tail be read as a command. Refusing on the sending side means the
    // failure is a missing indicator rather than a HUD taking dictation
    // instructions from a mode name.
    for name in ["two words", "com-mand", "", "quit\nmode"] {
        assert_eq!(
            OverlayCommand::Mode(Some(name.to_owned())).encode(),
            "mode",
            "{name:?}"
        );
    }
}
