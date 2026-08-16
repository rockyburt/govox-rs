//! Ported from `govox-py`'s `tests/test_daemon.py`.
//!
//! Every dependency is a recording fake, so the whole recognise → correct →
//! route → inject path runs with no microphone, no model and no desktop. That
//! is the same property the `Protocol` definitions give the reference, and it
//! is why this is where the routing rules are pinned.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use govox_core::config::{Config, Environment};
use govox_core::domain::{
    AudioBuffer, EditAction, EditOp, FieldSnapshot, GovoxError, Injector, InsertionAction,
    PersonalDictionary, PipelineAction, Utterance,
};
use govox_core::textmodel::DictationBuffer;
use govox_daemon::daemon::{Announcer, Daemon, Transcriber};
use govox_daemon::state::SharedState;

/// Returns a canned transcript, or an error, and records what it was asked.
struct FakeTranscriber {
    result: Mutex<Result<String, String>>,
    calls: Mutex<usize>,
}

impl FakeTranscriber {
    fn saying(text: &str) -> Self {
        Self {
            result: Mutex::new(Ok(text.to_owned())),
            calls: Mutex::new(0),
        }
    }

    fn failing() -> Self {
        Self {
            result: Mutex::new(Err("model exploded".to_owned())),
            calls: Mutex::new(0),
        }
    }
}

impl Transcriber for FakeTranscriber {
    async fn transcribe(&self, _audio: &AudioBuffer) -> Result<String, GovoxError> {
        *self.calls.lock().unwrap() += 1;
        self.result
            .lock()
            .unwrap()
            .clone()
            .map_err(GovoxError::InjectionRejected)
    }
}

#[derive(Default)]
struct RecordingInjector {
    actions: Mutex<Vec<InsertionAction>>,
    fail: bool,
}

impl RecordingInjector {
    fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            actions: Mutex::new(Vec::new()),
            fail: true,
        })
    }

    fn actions(&self) -> Vec<InsertionAction> {
        self.actions.lock().unwrap().clone()
    }

    fn texts(&self) -> Vec<String> {
        self.actions()
            .into_iter()
            .filter_map(|a| match a {
                InsertionAction::Text(text) => Some(text),
                _ => None,
            })
            .collect()
    }
}

impl Injector for RecordingInjector {
    fn insert(&self, action: &InsertionAction) -> Result<(), GovoxError> {
        self.actions.lock().unwrap().push(action.clone());
        if self.fail {
            return Err(GovoxError::InjectionRejected("no ydotool".to_owned()));
        }
        Ok(())
    }
}

/// So the daemon and the test can both hold the injector.
struct SharedInjector(Arc<RecordingInjector>);

impl Injector for SharedInjector {
    fn insert(&self, action: &InsertionAction) -> Result<(), GovoxError> {
        self.0.insert(action)
    }
}

#[derive(Default)]
struct RecordingAnnouncer {
    states: Mutex<Vec<String>>,
    captions: Mutex<Vec<String>>,
    notifications: Mutex<Vec<(String, String)>>,
}

impl RecordingAnnouncer {
    fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn states(&self) -> Vec<String> {
        self.states.lock().unwrap().clone()
    }
    fn captions(&self) -> Vec<String> {
        self.captions.lock().unwrap().clone()
    }
    fn notifications(&self) -> Vec<(String, String)> {
        self.notifications.lock().unwrap().clone()
    }
}

impl Announcer for RecordingAnnouncer {
    fn set_state(&self, state: &str) {
        self.states.lock().unwrap().push(state.to_owned());
    }
    fn caption(&self, text: &str) {
        self.captions.lock().unwrap().push(text.to_owned());
    }
    fn notify(&self, title: &str, body: &str) {
        self.notifications
            .lock()
            .unwrap()
            .push((title.to_owned(), body.to_owned()));
    }
}

struct SharedAnnouncer(Arc<RecordingAnnouncer>);

impl Announcer for SharedAnnouncer {
    fn set_state(&self, state: &str) {
        self.0.set_state(state);
    }
    fn caption(&self, text: &str) {
        self.0.caption(text);
    }
    fn notify(&self, title: &str, body: &str) {
        self.0.notify(title, body);
    }
}

fn defaults() -> Config {
    Config::load_from(None, &Environment::default()).expect("defaults are valid")
}

fn utterance() -> Utterance {
    let samples: Arc<[f32]> = Arc::from(vec![0.0_f32; 16_000]);
    Utterance {
        audio: AudioBuffer {
            samples,
            sample_rate: 16_000,
            start_ts: 0.0,
            end_ts: 1.0,
        },
        speech_end_ts: 1.0,
    }
}

/// A `PreeditSink` that records what it was asked to do and answers a fixed
/// field purpose. Preedit is entirely fire-and-forget, so recording the calls
/// is the only way to assert on it — which is exactly the recording-fake style
/// `govox-py` uses across every Protocol boundary.
#[derive(Default)]
struct RecordingPreedit {
    purpose: Option<String>,
    surrounding: Option<String>,
    calls: Mutex<Vec<String>>,
}

impl RecordingPreedit {
    fn in_a(purpose: &str) -> Arc<Self> {
        Arc::new(Self {
            purpose: Some(purpose.to_owned()),
            ..Self::default()
        })
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn note(&self, call: &str) {
        self.calls.lock().unwrap().push(call.to_owned());
    }
}

impl govox_core::domain::PreeditSink for RecordingPreedit {
    fn activate(&self) {
        self.note("activate");
    }
    fn deactivate(&self) {
        self.note("deactivate");
    }
    fn preedit(&self, text: &str) {
        self.note(&format!("preedit {text}"));
    }
    fn commit(&self, text: &str) {
        self.note(&format!("commit {text}"));
    }
    fn clear(&self) {
        self.note("clear");
    }
    fn field_purpose(&self) -> Option<String> {
        self.purpose.clone()
    }
    fn surrounding_text(&self) -> Option<String> {
        self.surrounding.clone()
    }
}

struct Harness {
    daemon: Daemon<FakeTranscriber>,
    injector: Arc<RecordingInjector>,
    announcer: Arc<RecordingAnnouncer>,
    shared: Arc<SharedState>,
}

fn harness_with(
    config: Config,
    transcriber: FakeTranscriber,
    injector: Arc<RecordingInjector>,
) -> Harness {
    let announcer = RecordingAnnouncer::shared();
    let shared = Arc::new(SharedState::new(config, PersonalDictionary::default()));
    Harness {
        daemon: Daemon {
            shared: Arc::clone(&shared),
            transcriber,
            injector: Box::new(SharedInjector(Arc::clone(&injector))),
            text_model: Arc::new(DictationBuffer::new(30.0)),
            announcer: Box::new(SharedAnnouncer(Arc::clone(&announcer))),
            preedit: None,
            config_path: None,
            listening: false,
        },
        injector,
        announcer,
        shared,
    }
}

fn harness(said: &str) -> Harness {
    harness_with(
        defaults(),
        FakeTranscriber::saying(said),
        RecordingInjector::shared(),
    )
}

#[tokio::test]
async fn a_spoken_phrase_is_corrected_and_typed() {
    let mut h = harness("hello world period");
    h.daemon.process_utterance(&utterance()).await;

    assert_eq!(h.injector.texts(), ["Hello world."]);
}

#[tokio::test]
async fn the_indicator_reports_transcribing_then_idle() {
    let mut h = harness("hello world");
    h.daemon.process_utterance(&utterance()).await;

    assert_eq!(h.announcer.states(), ["transcribing", "idle"]);
}

#[tokio::test]
async fn a_toggle_session_still_running_returns_to_listening() {
    let mut h = harness("hello world");
    h.daemon.listening = true;
    h.daemon.process_utterance(&utterance()).await;

    assert_eq!(h.announcer.states(), ["transcribing", "listening"]);
}

#[tokio::test]
async fn what_was_typed_is_remembered_for_delete_that() {
    let mut h = harness("hello world");
    h.daemon.process_utterance(&utterance()).await;

    // Sentence-final punctuation is added by the correction pipeline, so what
    // is remembered is what was *typed* — not what was said. A "delete that"
    // must backspace over the period too.
    assert_eq!(
        h.daemon.text_model.last_insertion().as_deref(),
        Some("Hello world.")
    );
}

#[tokio::test]
async fn a_transcription_failure_is_survived() {
    let mut h = harness_with(
        defaults(),
        FakeTranscriber::failing(),
        RecordingInjector::shared(),
    );
    // Must not panic and must not propagate: one bad utterance ending the
    // consumer task would collapse the whole daemon.
    h.daemon.process_utterance(&utterance()).await;

    assert!(h.injector.actions().is_empty());
    // The indicator still returns to rest, or it would sit on "transcribing".
    assert_eq!(h.announcer.states(), ["transcribing", "idle"]);
}

#[tokio::test]
async fn an_injection_failure_is_survived() {
    let mut h = harness_with(
        defaults(),
        FakeTranscriber::saying("hello world"),
        RecordingInjector::failing(),
    );
    h.daemon.process_utterance(&utterance()).await;

    assert_eq!(h.announcer.states(), ["transcribing", "idle"]);
}

#[tokio::test]
async fn entering_command_mode_is_loud() {
    let mut config = defaults();
    config.editing.command_mode = true;
    let mut h = harness_with(
        config,
        FakeTranscriber::saying("command mode"),
        RecordingInjector::shared(),
    );

    h.daemon.process_utterance(&utterance()).await;

    assert!(h.shared.command_mode(), "the mode is on");
    assert!(
        h.injector.actions().is_empty(),
        "switching modes types nothing"
    );
    // The objection to a mode is that it is an invisible state to get wrong.
    assert!(
        h.announcer
            .captions()
            .iter()
            .any(|c| c.contains("command mode")),
        "a persistent caption must say the mode is on"
    );
    assert!(
        h.announcer
            .notifications()
            .iter()
            .any(|(title, _)| title.contains("command mode"))
    );
}

#[tokio::test]
async fn re_entering_command_mode_does_not_re_announce() {
    let mut config = defaults();
    config.editing.command_mode = true;
    let mut h = harness_with(
        config,
        FakeTranscriber::saying("command mode"),
        RecordingInjector::shared(),
    );

    h.daemon.process_utterance(&utterance()).await;
    let after_first = h.announcer.notifications().len();
    h.daemon.process_utterance(&utterance()).await;

    assert!(h.shared.command_mode());
    assert_eq!(
        h.announcer.notifications().len(),
        after_first,
        "re-entering is what a user does when unsure which mode they are in"
    );
}

#[tokio::test]
async fn a_non_command_in_command_mode_is_discarded_not_typed() {
    // The failure the mode exists to prevent: half-heard command words
    // scattered through the document.
    let mut config = defaults();
    config.editing.command_mode = true;
    let mut h = harness_with(
        config,
        FakeTranscriber::saying("the quick brown fox"),
        RecordingInjector::shared(),
    );
    h.shared.set_command_mode(true);

    h.daemon.process_utterance(&utterance()).await;

    assert!(
        h.injector.actions().is_empty(),
        "nothing may be typed in command mode unless it matched a command"
    );
    // And the user is told what was dropped, so it can be repeated.
    assert!(
        h.announcer
            .captions()
            .iter()
            .any(|c| c.contains("not a command")),
        "captions were {:?}",
        h.announcer.captions()
    );
}

#[tokio::test]
async fn delete_that_backspaces_the_remembered_span_then_forgets_it() {
    let mut h = harness("hello");
    h.daemon.text_model.record_insertion("Hello world");

    h.daemon
        .apply_action(PipelineAction::Edit(EditAction::simple(EditOp::DeleteLast)))
        .expect("the edit applies");

    // One backspace per *character*, and the span is forgotten so a second
    // "delete that" cannot eat text govox never typed.
    assert!(!h.injector.actions().is_empty());
    assert_eq!(h.daemon.text_model.last_insertion(), None);
}

#[tokio::test]
async fn an_unsatisfiable_edit_notifies_rather_than_typing_the_phrase() {
    // The rule that matters: an edit that cannot run must never fall through
    // to typing the command words as literal text.
    let mut h = harness("hello");
    h.daemon.text_model.reset();

    h.daemon
        .apply_action(PipelineAction::Edit(EditAction::simple(EditOp::DeleteLast)))
        .expect("an unavailable edit is not an error");

    assert!(
        h.injector.actions().is_empty(),
        "nothing may be typed for an edit that could not run"
    );
    assert!(!h.announcer.notifications().is_empty(), "the user is told");
}

#[tokio::test]
async fn a_retyping_edit_records_what_it_left_on_screen() {
    // A case transform leaves different characters than the buffer remembers;
    // recording them keeps a following "delete that" the right length.
    let mut h = harness("hello");
    h.daemon.text_model.record_insertion("hello world");

    h.daemon
        .apply_action(PipelineAction::Edit(EditAction::simple(
            EditOp::UppercaseLast,
        )))
        .expect("the edit applies");

    let retyped = h.injector.texts().concat();
    assert_eq!(retyped, "HELLO WORLD", "the span is retyped in upper case");
    assert_eq!(
        h.daemon.text_model.last_insertion().as_deref(),
        Some(retyped.as_str()),
        "a following 'delete that' must backspace the new length, not the old"
    );
}

#[tokio::test]
async fn a_named_command_is_injected_without_being_remembered() {
    let mut h = harness("hello");
    h.daemon
        .apply_action(PipelineAction::Command("newline".to_owned()))
        .expect("the command applies");

    assert_eq!(
        h.injector.actions(),
        [InsertionAction::Command("newline".to_owned())]
    );
    // A newline is not text govox can later backspace over meaningfully.
    assert_eq!(h.daemon.text_model.last_insertion(), None);
}

#[tokio::test]
async fn injection_does_not_wait_when_no_modifier_is_held() {
    let h = harness("hello");
    let started = std::time::Instant::now();
    h.daemon
        .await_modifiers_released(Duration::from_secs(5))
        .await;
    assert!(started.elapsed() < Duration::from_millis(100));
}

#[tokio::test]
async fn injection_waits_for_a_held_modifier_to_come_up() {
    // The Ctrl+W bug: in double-tap mode the session stops on the second key
    // *down*, so injection would otherwise start with Ctrl still held and
    // "www" would close the browser tab.
    let h = harness("hello");
    h.shared.note_modifier("KEY_RIGHTCTRL", true);

    let shared = Arc::clone(&h.shared);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(80)).await;
        shared.note_modifier("KEY_RIGHTCTRL", false);
    });

    let started = std::time::Instant::now();
    h.daemon
        .await_modifiers_released(Duration::from_secs(5))
        .await;

    assert!(
        started.elapsed() >= Duration::from_millis(80),
        "it must actually have waited"
    );
    assert!(!h.shared.modifiers_held());
}

#[tokio::test]
async fn a_finger_resting_on_shift_does_not_cost_the_utterance() {
    // Bounded rather than unconditional: losing the text is the worse failure.
    let h = harness("hello");
    h.shared.note_modifier("KEY_LEFTSHIFT", true);

    let started = std::time::Instant::now();
    h.daemon
        .await_modifiers_released(Duration::from_millis(120))
        .await;

    assert!(started.elapsed() >= Duration::from_millis(120), "it waited");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "but it gave up and injected anyway"
    );
    assert!(h.shared.modifiers_held(), "shift is still down");
}

#[tokio::test]
async fn a_full_utterance_waits_for_modifiers_before_typing() {
    // The end-to-end version: the wait is inside process_utterance, not only
    // available as a method nobody calls.
    let mut h = harness("hello world");
    h.shared.note_modifier("KEY_RIGHTCTRL", true);

    let shared = Arc::clone(&h.shared);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        shared.note_modifier("KEY_RIGHTCTRL", false);
    });

    let started = std::time::Instant::now();
    h.daemon.process_utterance(&utterance()).await;

    assert_eq!(h.injector.texts(), ["Hello world."]);
    assert!(
        started.elapsed() >= Duration::from_millis(60),
        "injection must not have run while Ctrl was down"
    );
}

#[tokio::test]
async fn a_failed_reload_keeps_the_running_configuration() {
    // Unlike a failed startup, a failed reload is not fatal: there is a
    // known-good configuration already running.
    let mut h = harness("hello");
    h.daemon.config_path = Some(std::path::PathBuf::from("/nonexistent/govox.toml"));
    let before = h.shared.config.load_full();

    let outcome = h.daemon.reload();

    assert!(!outcome.ok);
    assert_eq!(
        h.shared.config.load_full().recognition.model,
        before.recognition.model,
        "the previous configuration must survive"
    );
    // And the failure is loud, never swallowed.
    assert!(
        h.announcer
            .notifications()
            .iter()
            .any(|(_, body)| body.contains("Reload failed"))
    );
}

#[test]
fn publishing_a_reload_swaps_every_dictionary_consumer_at_once() {
    // The silent half-reload this guards against: replacements changing while
    // recognition stays biased by the old word list.
    let shared = SharedState::new(defaults(), PersonalDictionary::default());

    let mut config = defaults();
    config.correction.enabled = false;
    let dictionary = PersonalDictionary {
        bias_terms: vec!["Kubernetes".to_owned()],
        replacements: vec![("rentals api".to_owned(), "Rentals-API".to_owned())],
    };
    shared.publish(config, dictionary);

    assert_eq!(shared.dictionary.load().bias_terms, ["Kubernetes"]);
    assert_eq!(
        shared.corrector.load().dictionary.replacements.len(),
        1,
        "the corrector must be rebuilt around the new dictionary"
    );
    assert!(!shared.corrector.load().config.enabled);
}

/// The whole point of knowing a field is a password field.
///
/// A password is the one thing where transcribing is worse than doing nothing:
/// it is not meant to exist outside the user's head, and dictating it would put
/// it on screen as preedit before committing it anywhere.
#[tokio::test]
async fn a_password_field_swallows_the_utterance_entirely() {
    let mut harness = harness("hunter two");
    let preedit = RecordingPreedit::in_a("PASSWORD");
    harness.daemon.preedit = Some(Arc::clone(&preedit) as Arc<dyn govox_core::domain::PreeditSink>);

    harness.daemon.process_utterance(&utterance()).await;

    assert!(
        harness.injector.texts().is_empty(),
        "nothing may be typed into a password field"
    );
    assert_eq!(
        harness.announcer.notifications(),
        [(
            "govox".to_owned(),
            "Password field — nothing was dictated.".to_owned()
        )],
        "the user has to be told, or the silence looks like a crash"
    );
}

/// The refusal comes *before* the action is routed, not after.
///
/// A mode switch is the case that makes the ordering visible: routing first
/// would let "command mode" spoken into a password box change govox's state,
/// which is a decision taken on text that was never meant to be heard.
#[tokio::test]
async fn a_password_field_is_checked_before_the_action_is_routed() {
    let mut config = defaults();
    config.editing.command_mode = true;
    let mut harness = harness_with(
        config,
        FakeTranscriber::saying("command mode"),
        RecordingInjector::shared(),
    );
    harness.daemon.preedit =
        Some(RecordingPreedit::in_a("PASSWORD") as Arc<dyn govox_core::domain::PreeditSink>);

    harness.daemon.process_utterance(&utterance()).await;

    assert!(
        !harness.shared.command_mode(),
        "a password field must not be able to change govox's mode"
    );
}

#[tokio::test]
async fn an_ordinary_field_dictates_normally() {
    // The refusal is specific to PASSWORD; every other purpose only steers the
    // prose rules, and none of them stops dictation.
    let mut harness = harness("hello world");
    harness.daemon.preedit =
        Some(RecordingPreedit::in_a("FREE_FORM") as Arc<dyn govox_core::domain::PreeditSink>);

    harness.daemon.process_utterance(&utterance()).await;

    assert_eq!(harness.injector.texts().len(), 1);
}

/// Differential: the *same* words into two different fields.
///
/// Asserting only that a terminal gets no full stop would pass just as well if
/// the purpose never reached the pipeline and nothing ever added one. The
/// FREE_FORM arm is what makes the TERMINAL arm mean something.
#[tokio::test]
async fn the_field_purpose_reaches_the_correction_pipeline() {
    async fn dictated_into(purpose: &str) -> String {
        let mut harness = harness("list the files");
        harness.daemon.preedit =
            Some(RecordingPreedit::in_a(purpose) as Arc<dyn govox_core::domain::PreeditSink>);
        harness.daemon.process_utterance(&utterance()).await;
        harness.injector.texts().pop().expect("one insertion")
    }

    let prose = dictated_into("FREE_FORM").await;
    let terminal = dictated_into("TERMINAL").await;

    assert!(
        prose.ends_with('.'),
        "prose gets a full stop, got {prose:?}"
    );
    // A full stop breaks a shell command, and a capital breaks the command name.
    assert!(
        !terminal.ends_with('.'),
        "prose rules must stand down in a terminal, got {terminal:?}"
    );
    assert_ne!(prose, terminal);
}

#[tokio::test]
async fn without_an_input_method_nothing_changes() {
    // `[ime] enabled` is off by default, so this is the ordinary path: no
    // purpose, no refusal, prose rules as before.
    let mut harness = harness("hello world");
    assert!(harness.daemon.preedit.is_none());
    assert!(!harness.daemon.refuses_to_dictate());

    harness.daemon.process_utterance(&utterance()).await;

    assert_eq!(harness.injector.texts().len(), 1);
}

/// The session lifecycle, in order.
///
/// Activation has to be first — nothing else works until govox is the engine —
/// and the clear has to come before the deactivate so the client is *told* to
/// drop the preedit rather than left to infer it from a focus change it may
/// never receive.
#[test]
fn a_session_takes_the_input_method_and_hands_it_back() {
    let preedit = Arc::new(RecordingPreedit {
        surrounding: Some("Good morning, ".to_owned()),
        ..RecordingPreedit::default()
    });
    let sink: Arc<dyn govox_core::domain::PreeditSink> = Arc::clone(&preedit) as _;
    let shared = Arc::new(SharedState::new(defaults(), PersonalDictionary::default()));

    govox_daemon::begin_session(Some(&sink), &DictationBuffer::new(30.0), &shared);
    assert_eq!(
        shared.preceding().as_deref(),
        Some("Good morning, "),
        "the text before the caret is read once, at the start"
    );

    govox_daemon::end_session(Some(&sink), &shared);
    assert_eq!(preedit.calls(), ["activate", "clear", "deactivate"]);
}

#[test]
fn a_client_reporting_no_context_leaves_none_behind_from_the_last_session() {
    let preedit: Arc<dyn govox_core::domain::PreeditSink> = Arc::new(RecordingPreedit::default());
    let shared = Arc::new(SharedState::new(defaults(), PersonalDictionary::default()));
    shared.set_preceding(Some("stale".to_owned()));

    govox_daemon::begin_session(Some(&preedit), &DictationBuffer::new(30.0), &shared);

    // Not "keep what we had": the previous session's context describes a field
    // that no longer has focus, and prose that continues it would be wrong.
    assert_eq!(shared.preceding(), None);
}

#[test]
fn an_empty_surrounding_text_is_the_same_as_no_context() {
    // A client reporting "" says the caret is at the start of the field, which
    // is no context at all — and the correction pipeline reads `None` as
    // "assume nothing" while `Some("")` would look like a real answer.
    let preedit: Arc<dyn govox_core::domain::PreeditSink> = Arc::new(RecordingPreedit {
        surrounding: Some(String::new()),
        ..RecordingPreedit::default()
    });
    let shared = Arc::new(SharedState::new(defaults(), PersonalDictionary::default()));

    govox_daemon::begin_session(Some(&preedit), &DictationBuffer::new(30.0), &shared);

    assert_eq!(shared.preceding(), None);
}

/// A field-reading `TextModel`, standing in for AT-SPI.
struct ReadableField(FieldSnapshot);

impl govox_core::domain::TextModel for ReadableField {
    fn last_insertion(&self) -> Option<String> {
        None
    }
    fn record_insertion(&self, _text: &str) {}
    fn consume_last(&self) -> Option<String> {
        None
    }
    fn read_field(&self) -> Option<FieldSnapshot> {
        Some(self.0.clone())
    }
    fn reset(&self) {}
}

/// The input method is asked first, AT-SPI is the fallback.
///
/// Order, not preference: a client that provides surrounding text does so
/// wherever preedit works, which includes applications AT-SPI reports as
/// readable but *not* writable — Chrome among them.
#[test]
fn the_field_is_read_when_the_input_method_has_nothing_to_say() {
    let field = ReadableField(FieldSnapshot {
        text: "Good morning, world".to_owned(),
        caret: 14,
    });
    let shared = Arc::new(SharedState::new(defaults(), PersonalDictionary::default()));

    // No input method at all.
    govox_daemon::begin_session(None, &field, &shared);
    assert_eq!(shared.preceding().as_deref(), Some("Good morning, "));

    // An input method whose client does not provide surrounding text.
    let silent: Arc<dyn govox_core::domain::PreeditSink> = Arc::new(RecordingPreedit::default());
    shared.set_preceding(None);
    govox_daemon::begin_session(Some(&silent), &field, &shared);
    assert_eq!(shared.preceding().as_deref(), Some("Good morning, "));
}

#[test]
fn the_input_method_wins_when_it_can_answer() {
    let field = ReadableField(FieldSnapshot {
        text: "from at-spi".to_owned(),
        caret: 11,
    });
    let preedit: Arc<dyn govox_core::domain::PreeditSink> = Arc::new(RecordingPreedit {
        surrounding: Some("from the input method".to_owned()),
        ..RecordingPreedit::default()
    });
    let shared = Arc::new(SharedState::new(defaults(), PersonalDictionary::default()));

    govox_daemon::begin_session(Some(&preedit), &field, &shared);

    assert_eq!(shared.preceding().as_deref(), Some("from the input method"));
}

#[test]
fn a_caret_at_the_start_of_a_field_is_no_context_at_all() {
    // `preceding` of a caret at 0 is "", which would look like a real answer
    // to the correction pipeline while meaning the opposite.
    let field = ReadableField(FieldSnapshot {
        text: "anything".to_owned(),
        caret: 0,
    });
    let shared = Arc::new(SharedState::new(defaults(), PersonalDictionary::default()));

    govox_daemon::begin_session(None, &field, &shared);

    assert_eq!(shared.preceding(), None);
}

/// Committing goes through the input method while a session holds it.
///
/// The routing decision is the same as every other action's — only the
/// actuator differs — which is what keeps commands working identically with
/// and without preedit. Injecting here instead would type the words as
/// synthetic keystrokes *on top of* the provisional text the field is already
/// showing, i.e. the sentence twice.
#[tokio::test]
async fn text_commits_through_the_input_method_rather_than_the_keyboard() {
    let mut harness = harness("hello world");
    let preedit = Arc::new(RecordingPreedit::default());
    harness.daemon.preedit = Some(Arc::clone(&preedit) as Arc<dyn govox_core::domain::PreeditSink>);
    harness.shared.set_preedit_active(true);

    harness.daemon.process_utterance(&utterance()).await;

    assert!(
        harness.injector.texts().is_empty(),
        "nothing may be typed while the field is holding provisional text"
    );
    assert_eq!(preedit.calls(), ["commit Hello world."]);
}

/// Without a live session the sink is not used, even though it exists.
#[tokio::test]
async fn text_is_injected_when_no_session_holds_the_input_method() {
    let mut harness = harness("hello world");
    let preedit = Arc::new(RecordingPreedit::default());
    harness.daemon.preedit = Some(Arc::clone(&preedit) as Arc<dyn govox_core::domain::PreeditSink>);
    // The sink outlives every session; committing outside one would put text
    // into a field govox never activated for.
    assert!(!harness.shared.preedit_active());

    harness.daemon.process_utterance(&utterance()).await;

    assert_eq!(harness.injector.texts().len(), 1);
    assert!(preedit.calls().is_empty());
}

/// Already-recognised text takes the same correction pass as audio does.
#[tokio::test]
async fn streaming_text_is_corrected_exactly_as_an_utterance_is() {
    let mut harness = harness("ignored — this path supplies its own text");

    harness.daemon.process_text("hello world").await;

    // Sentence casing and the trailing full stop are the correction pipeline's,
    // not the recognizer's: a streaming session that skipped this pass would
    // commit raw transcript.
    assert_eq!(harness.injector.texts(), ["Hello world."]);
}

#[tokio::test]
async fn an_empty_streaming_session_commits_nothing() {
    let mut harness = harness("unused");
    harness.daemon.process_text("   ").await;
    assert!(harness.injector.texts().is_empty());
}
