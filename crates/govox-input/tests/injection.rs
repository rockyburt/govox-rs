//! Ported from `govox-py`'s `tests/test_injection.py`.
//!
//! Every assertion here is about *exact argv*. That is not pedantry: the two
//! bugs this module exists to prevent — `ydotool key enter` pressing nothing,
//! and a newline being typed as a literal character — both produce a runner
//! call that succeeds. Only the argv distinguishes working from broken.

use std::sync::Arc;

use govox_core::config::{Config, Environment};
use govox_core::domain::{Capabilities, GovoxError, Injector, InsertionAction};
use govox_input::{
    ClipboardInjector, RecordingRunner, YdotoolInjector, select_injector, selector::SilentNotify,
};

/// `(argv, stdin)` as plain strings, so assertions read like the Python.
type Call = (Vec<String>, Option<String>);

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

fn call(parts: &[&str], stdin: Option<&str>) -> Call {
    (argv(parts), stdin.map(ToOwned::to_owned))
}

fn capabilities(primary: &str, strategies: &[&str]) -> Capabilities {
    Capabilities {
        session_type: "wayland".to_owned(),
        desktop: "GNOME".to_owned(),
        supported: true,
        primary_injection: Some(primary.to_owned()),
        injection_strategies: strategies.iter().map(|s| (*s).to_owned()).collect(),
        hotkey_strategies: Vec::new(),
        reasons: Vec::new(),
        ime_available: false,
    }
}

/// Defaults only, with no user config file and a scrubbed environment — the
/// Rust equivalent of the Python test pointing `XDG_CONFIG_HOME` at `tmp_path`.
fn default_config() -> Config {
    Config::load_from(None, &Environment::default()).expect("defaults are valid")
}

#[test]
fn ydotool_types_text() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = YdotoolInjector::new(Arc::clone(&runner));

    injector
        .insert(&InsertionAction::Text("Hello world.".to_owned()))
        .expect("typing succeeds");

    assert_eq!(
        runner.calls(),
        vec![call(&["ydotool", "type", "Hello world."], None)]
    );
}

#[test]
fn ydotool_performs_command_action() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = YdotoolInjector::new(Arc::clone(&runner));

    injector
        .insert(&InsertionAction::Command("newline".to_owned()))
        .expect("newline succeeds");

    // Raw keycodes (KEY_ENTER = 28), not the name "enter": `ydotool key enter`
    // exits 0 and presses nothing, so the old govox-py assertion passed against
    // a fake runner while "new line" did nothing in production.
    assert_eq!(
        runner.calls(),
        vec![call(&["ydotool", "key", "28:1", "28:0"], None)]
    );
}

/// The negative test. This is the guard, and it is deliberately broad: it
/// sweeps every injectable action and asserts that no `ydotool key` argv ever
/// contains a key *name*, whatever the spelling of the chord that produced it.
#[test]
fn ydotool_key_is_never_passed_a_key_name() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = YdotoolInjector::new(Arc::clone(&runner));

    let actions = [
        InsertionAction::Command("newline".to_owned()),
        InsertionAction::Command("new_paragraph".to_owned()),
        InsertionAction::Keys(vec!["ctrl+shift+left".to_owned(), "backspace".to_owned()]),
        InsertionAction::Keys(vec!["ctrl+a".to_owned()]),
        InsertionAction::Keys(vec![" CTRL + V ".to_owned()]),
        InsertionAction::Text("first\nsecond".to_owned()),
    ];
    for action in &actions {
        injector.insert(action).expect("every action is injectable");
    }

    // Everything the keycode table can name. Not one may appear in an argv.
    let names = [
        "esc",
        "backspace",
        "tab",
        "y",
        "enter",
        "ctrl",
        "a",
        "shift",
        "z",
        "x",
        "c",
        "v",
        "alt",
        "home",
        "up",
        "pageup",
        "left",
        "right",
        "end",
        "down",
        "pagedown",
        "insert",
        "delete",
        "meta",
    ];

    let mut key_calls = 0;
    for (command, _) in runner.calls() {
        if command.get(1).map(String::as_str) != Some("key") {
            continue;
        }
        key_calls += 1;
        for arg in &command[2..] {
            assert!(
                !names.contains(&arg.as_str()),
                "ydotool key was handed the name {arg:?}; it would exit 0 and press nothing"
            );
            // Every argument must be exactly `<digits>:<0|1>`.
            let (code, pressed) = arg.split_once(':').expect("argv is <keycode>:<pressed>");
            assert!(
                code.chars().all(|c| c.is_ascii_digit()) && !code.is_empty(),
                "keycode {code:?} is not numeric"
            );
            assert!(
                pressed == "0" || pressed == "1",
                "state {pressed:?} is not 0 or 1"
            );
        }
    }
    assert!(
        key_calls > 0,
        "the sweep proved nothing if no keys were pressed"
    );
}

#[test]
fn ydotool_presses_enter_rather_than_typing_a_newline() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = YdotoolInjector::new(Arc::clone(&runner));

    injector
        .insert(&InsertionAction::Text("first\nsecond".to_owned()))
        .expect("typing succeeds");

    assert_eq!(
        runner.calls(),
        vec![
            call(&["ydotool", "type", "first"], None),
            call(&["ydotool", "key", "28:1", "28:0"], None),
            call(&["ydotool", "type", "second"], None),
        ]
    );
}

#[test]
fn ydotool_skips_empty_segments_around_a_newline() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = YdotoolInjector::new(Arc::clone(&runner));

    injector
        .insert(&InsertionAction::Text("\n".to_owned()))
        .expect("typing succeeds");

    // Both segments are empty, so only the Enter press survives.
    assert_eq!(
        runner.calls(),
        vec![call(&["ydotool", "key", "28:1", "28:0"], None)]
    );
}

#[test]
fn ydotool_rejects_an_untranslatable_chord_without_running_anything() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = YdotoolInjector::new(Arc::clone(&runner));

    let error = injector
        .insert(&InsertionAction::Keys(vec!["ctrl+f13".to_owned()]))
        .expect_err("f13 is not in the keycode table");

    assert!(matches!(error, GovoxError::InjectionRejected(_)));
    assert!(
        runner.calls().is_empty(),
        "a chord that cannot be compiled must not reach ydotool at all"
    );
}

#[test]
fn ydotool_ignores_an_unknown_command_name() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = YdotoolInjector::new(Arc::clone(&runner));

    injector
        .insert(&InsertionAction::Command("not_a_command".to_owned()))
        .expect("unknown commands are a no-op, as in govox-py");

    assert!(runner.calls().is_empty());
}

#[test]
fn ydotool_rejection_raises_a_typed_error() {
    let runner = Arc::new(RecordingRunner::failing_first());
    let injector = YdotoolInjector::new(runner);

    let error = injector
        .insert(&InsertionAction::Text("Hello world.".to_owned()))
        .expect_err("a non-zero exit is a rejection");

    match error {
        GovoxError::InjectionRejected(detail) => assert_eq!(detail, "rejected"),
        other => panic!("expected InjectionRejected, got {other:?}"),
    }
}

#[test]
fn clipboard_copies_and_pastes_with_raw_keycodes() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = ClipboardInjector::new(Arc::clone(&runner), true);

    injector
        .insert(&InsertionAction::Text("Hello world.".to_owned()))
        .expect("copy and paste succeed");

    assert_eq!(
        runner.calls(),
        vec![
            call(&["wl-copy"], Some("Hello world.")),
            // KEY_LEFTCTRL = 29, KEY_V = 47.
            call(&["ydotool", "key", "29:1", "47:1", "47:0", "29:0"], None),
        ]
    );
}

#[test]
fn clipboard_cannot_emit_keystrokes() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = ClipboardInjector::new(Arc::clone(&runner), true);

    let error = injector
        .insert(&InsertionAction::Keys(vec!["ctrl+a".to_owned()]))
        .expect_err("the clipboard has no way to press a key");

    assert!(matches!(error, GovoxError::InjectionRejected(_)));
    assert!(runner.calls().is_empty());
}

#[test]
fn clipboard_renders_commands_as_whitespace() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = ClipboardInjector::new(Arc::clone(&runner), false);

    injector
        .insert(&InsertionAction::Command("new_paragraph".to_owned()))
        .expect("copy succeeds");

    assert_eq!(runner.calls(), vec![call(&["wl-copy"], Some("\n\n"))]);
}

#[test]
fn fallback_to_clipboard_on_rejection() {
    let runner = Arc::new(RecordingRunner::failing_first());
    let notifications: Arc<std::sync::Mutex<Vec<(String, String)>>> = Arc::default();
    let recorded = Arc::clone(&notifications);

    let injector = select_injector(
        &capabilities("ydotool", &["ydotool", "clipboard"]),
        &default_config(),
        Arc::clone(&runner),
        move |title: &str, body: &str| {
            recorded
                .lock()
                .expect("notifications poisoned")
                .push((title.to_owned(), body.to_owned()));
        },
    );

    injector
        .insert(&InsertionAction::Text("Hello world.".to_owned()))
        .expect("the fallback carries the utterance");

    let calls = runner.calls();
    assert_eq!(calls[0], call(&["ydotool", "type", "Hello world."], None));
    assert_eq!(calls[1], call(&["wl-copy"], Some("Hello world.")));
    // No paste: pasting needs ydotool, which is why we are on this path at all.
    assert_eq!(calls.len(), 2);
    assert_eq!(
        *notifications.lock().expect("notifications poisoned"),
        vec![(
            "govox clipboard fallback".to_owned(),
            "Text copied to clipboard.".to_owned()
        )]
    );
}

#[test]
fn selector_uses_clipboard_only_when_no_uinput() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = select_injector(
        &capabilities("clipboard", &["clipboard"]),
        &default_config(),
        Arc::clone(&runner),
        SilentNotify,
    );

    injector
        .insert(&InsertionAction::Text("Hello world.".to_owned()))
        .expect("copy succeeds");

    // The Python asserts `isinstance(injector, ClipboardInjector)`. Behind a
    // `Box<dyn Injector>` the equivalent observation is the argv: a bare
    // `wl-copy` with no ydotool call before or after it.
    assert_eq!(
        runner.calls(),
        vec![call(&["wl-copy"], Some("Hello world."))]
    );
}

#[test]
fn selector_honours_a_clipboard_preference_even_where_ydotool_works() {
    let mut config = default_config();
    config.injection.method = govox_core::config::InjectionMethod::Clipboard;

    let runner = Arc::new(RecordingRunner::new());
    let injector = select_injector(
        &capabilities("ydotool", &["ydotool", "clipboard"]),
        &config,
        Arc::clone(&runner),
        SilentNotify,
    );

    injector
        .insert(&InsertionAction::Text("Hello world.".to_owned()))
        .expect("copy succeeds");

    assert_eq!(
        runner.calls(),
        vec![call(&["wl-copy"], Some("Hello world."))]
    );
}

/// The bug this routing exists for: `ydotool type 👍` exits 0 and types
/// nothing, so the emoji was silently dropped rather than reported missing.
#[test]
fn an_emoji_is_pasted_rather_than_typed() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = select_injector(
        &capabilities("ydotool", &["ydotool", "clipboard"]),
        &default_config(),
        Arc::clone(&runner),
        SilentNotify,
    );

    injector
        .insert(&InsertionAction::Text("Nice work 👍".to_owned()))
        .expect("the clipboard path carries the emoji");

    let calls = runner.calls();
    // Never offered to ydotool: the check is made from the text, because the
    // failure being avoided is one ydotool does not report.
    assert_eq!(calls[0], call(&["wl-copy"], Some("Nice work 👍")));
    assert_eq!(calls.len(), 2, "expected a copy and a paste, got {calls:?}");
    assert!(
        calls[1]
            .0
            .starts_with(&["ydotool".to_owned(), "key".to_owned()]),
        "the text must be pasted for the user, not left on the clipboard: {:?}",
        calls[1]
    );
}

#[test]
fn ordinary_text_still_goes_to_ydotool() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = select_injector(
        &capabilities("ydotool", &["ydotool", "clipboard"]),
        &default_config(),
        Arc::clone(&runner),
        SilentNotify,
    );

    injector
        .insert(&InsertionAction::Text("Hello world.".to_owned()))
        .expect("typing succeeds");

    assert_eq!(
        runner.calls(),
        vec![call(&["ydotool", "type", "Hello world."], None)]
    );
}

/// Accented and non-Latin text is typed today and must keep being typed.
/// Rerouting it would quietly move every non-English user onto the clipboard.
#[test]
fn non_ascii_letters_are_not_treated_as_untypeable() {
    let runner = Arc::new(RecordingRunner::new());
    let injector = select_injector(
        &capabilities("ydotool", &["ydotool", "clipboard"]),
        &default_config(),
        Arc::clone(&runner),
        SilentNotify,
    );

    for text in ["café", "日本語", "naïve — really…"] {
        injector
            .insert(&InsertionAction::Text(text.to_owned()))
            .expect("typing succeeds");
    }

    let calls = runner.calls();
    assert!(
        calls.iter().all(|(argv, _)| argv[0] == "ydotool"),
        "something was rerouted to the clipboard: {calls:?}"
    );
}
