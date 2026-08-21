//! Domain types and the traits the rest of the workspace implements.
//!
//! Ported from `govox-py`'s `src/govox/domain.py`. The frozen dataclasses
//! become plain structs and the `Protocol`s become traits; the two unions
//! (`InsertionAction`, `PipelineAction`) become real enums, which is the one
//! place where Rust expresses the original intent better than the Python did.

use std::sync::Arc;

/// A block of samples as captured, always mono and `f32`.
///
/// `govox-py` carries these as `tuple[float, ...]` of boxed Python floats
/// through a frozen dataclass, re-chunked per 512-sample VAD window. `Arc<[f32]>`
/// keeps the cheap-clone property that made the frozen dataclass convenient
/// without copying the samples on every hand-off between tasks.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFrame {
    pub samples: Arc<[f32]>,
    pub sample_rate: u32,
    /// Monotonic seconds since capture start.
    pub timestamp: f64,
}

/// A contiguous span of captured audio with its start and end timestamps.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBuffer {
    pub samples: Arc<[f32]>,
    pub sample_rate: u32,
    pub start_ts: f64,
    pub end_ts: f64,
}

/// One segmented utterance, ready to be recognised.
#[derive(Debug, Clone, PartialEq)]
pub struct Utterance {
    pub audio: AudioBuffer,
    pub speech_end_ts: f64,
}

/// An incremental streaming result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingDelta {
    pub text: String,
    pub is_final: bool,
}

/// The structural unit an editing command operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    Character,
    Word,
    Sentence,
    Paragraph,
    Line,
    /// Only meaningful as a caret destination ("move to end of document");
    /// there is no "delete previous document", and the editor reports as much.
    Document,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Previous,
    Next,
}

/// The editing operations the grammar can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditOp {
    Undo,
    Redo,
    DeleteLast,
    DeleteUnit,
    DeleteAll,
    Cut,
    Copy,
    Paste,
    SelectAll,
    SelectLast,
    Deselect,
    /// `DeleteUnit`, `SelectUnit` and `MoveUnit` share the same
    /// (unit, direction, count) slots and differ only in the chords they
    /// compile to.
    SelectUnit,
    MoveUnit,
    /// "move to beginning/end of <unit>": the unit names the structure, the
    /// direction names which end of it.
    MoveToEdge,
    /// Case transforms on the last dictated utterance. No toolkit binds a case
    /// change, so these retype what govox typed rather than pressing a key —
    /// which is only possible for text govox itself produced.
    UppercaseLast,
    LowercaseLast,
    CapitalizeLast,
    /// "press enter", "press escape": one key, named by the speaker.
    ///
    /// The key travels in `phrase` as an already-resolved *chord name* rather
    /// than in a slot of its own, so the recorded shape of an `EditAction` is
    /// unchanged and the golden corpus keeps its ~239k records. The grammar
    /// only ever puts a name from its own table there, so what reaches the
    /// injector is a string `keycodes::parse_chord` is known to accept.
    PressKey,
    /// Tier 2: targets named by their content rather than by structure. These
    /// take a free-form slot, so they only ever fire in command mode — "delete
    /// the old file" is a sentence, not an instruction.
    SelectPhrase,
    DeletePhrase,
    ReplacePhrase,
    MoveBeforePhrase,
    MoveAfterPhrase,
}

/// An editing *intent* parsed from speech, with its slots resolved.
///
/// Deliberately not an [`InsertionAction`]: intent is compiled into concrete
/// keystrokes by the editor before any injector sees it, so the actuator layer
/// never has to know what a "sentence" is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditAction {
    pub op: EditOp,
    pub unit: Option<Unit>,
    pub direction: Option<Direction>,
    pub count: i64,
    /// The text to find in the field.
    pub phrase: Option<String>,
    /// What "replace X with Y" puts there.
    pub replacement: Option<String>,
}

impl EditAction {
    /// A slot-less operation such as `Undo` or `Paste`.
    #[must_use]
    pub fn simple(op: EditOp) -> Self {
        Self {
            op,
            unit: None,
            direction: None,
            count: 1,
            phrase: None,
            replacement: None,
        }
    }
}

/// What an injector can execute. All intent has already been resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertionAction {
    /// Type this text verbatim.
    Text(String),
    /// A named formatting command, e.g. `newline`, `new_paragraph`.
    Command(String),
    /// A literal keystroke sequence, e.g. `["ctrl+shift+left", "backspace"]`.
    ///
    /// Injectors execute this verbatim — by the time one exists, every decision
    /// has been made.
    Keys(Vec<String>),
}

/// What the correction pipeline may emit.
///
/// `Edit` is routed through the editor by the daemon and `Mode` is consumed by
/// it; everything else goes straight to the injector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineAction {
    Text(String),
    Command(String),
    Edit(EditAction),
    /// Suspend or resume listening, without ending the session.
    ///
    /// A third state beside dictating and commanding, taken from macOS Voice
    /// Control. Distinct from stopping: the session, its preedit and its
    /// context survive, so waking resumes where you left off rather than
    /// starting again. While asleep **nothing else is honoured** — that is the
    /// point of it, and the reason it cannot simply be a mode flag on `Mode`.
    Sleep {
        asleep: bool,
    },
    /// Switch between dictating text and issuing commands.
    ///
    /// Types nothing: it changes how the *next* utterance is interpreted, which
    /// is why it is the one action whose whole effect is on the daemon rather
    /// than on the focused field.
    Mode {
        command_mode: bool,
    },
}

/// The outcome of one pass of the correction pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionResult {
    pub raw_text: String,
    pub corrected_text: String,
    pub action: PipelineAction,
}

/// The focused field's real contents at one instant, when it can be read.
///
/// Only produced by a backend that can actually see the widget. A snapshot is
/// valid for exactly as long as the user does not touch the keyboard, so it is
/// taken per utterance and thrown away — never cached across commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSnapshot {
    pub text: String,
    /// Caret position, in characters.
    pub caret: usize,
}

impl FieldSnapshot {
    /// The `length` characters immediately before the caret.
    ///
    /// Shorter than requested when the caret is closer to the start than that,
    /// which is itself a useful signal: it cannot match a longer remembered
    /// insertion, so the comparison fails as it should.
    #[must_use]
    pub fn preceding(&self, length: usize) -> String {
        let start = self.caret.saturating_sub(length);
        self.text
            .chars()
            .skip(start)
            .take(self.caret - start)
            .collect()
    }
}

/// Terms biased into recognition, and literal replacements applied after it.
///
/// Loaded from a TOML file named by `[correction] dictionary_path`:
///
/// ```toml
/// [dictionary]
/// bias = ["Rentals.ca", "ydotool"]
///
/// [[dictionary.replace]]
/// from = "rentals api"
/// to = "Rentals-API"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonalDictionary {
    pub bias_terms: Vec<String>,
    /// Ordered `(from, to)` pairs. Order matters: they are applied in sequence.
    pub replacements: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum DictionaryError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("invalid TOML in {path}: {source}")]
    Toml {
        path: std::path::PathBuf,
        source: toml::de::Error,
    },
    #[error("[dictionary] must be a TOML table")]
    NotATable,
    #[error("[dictionary].bias must be a list of strings")]
    BadBias,
    #[error("[[dictionary.replace]] entries must be TOML tables")]
    BadReplaceShape,
    #[error("dictionary replacements require string \"from\" and \"to\"")]
    BadReplaceEntry,
}

impl PersonalDictionary {
    /// Load from a path, expanding a leading `~`.
    ///
    /// The expansion is load-bearing rather than a nicety: the documented
    /// example is `~/.config/govox/dictionary.toml`, and without it that path is
    /// taken literally, the open fails, and the daemon refuses to start — so
    /// following the documentation broke startup.
    pub fn load(
        path: &std::path::Path,
        home: Option<&std::path::Path>,
    ) -> Result<Self, DictionaryError> {
        let expanded = expand_user(path, home);
        let text = std::fs::read_to_string(&expanded).map_err(|source| DictionaryError::Io {
            path: expanded.clone(),
            source,
        })?;
        let table: toml::Table = text.parse().map_err(|source| DictionaryError::Toml {
            path: expanded,
            source,
        })?;
        Self::from_table(&table)
    }

    fn from_table(table: &toml::Table) -> Result<Self, DictionaryError> {
        let Some(value) = table.get("dictionary") else {
            return Ok(Self::default());
        };
        let toml::Value::Table(dictionary) = value else {
            return Err(DictionaryError::NotATable);
        };

        let mut bias_terms = Vec::new();
        if let Some(bias) = dictionary.get("bias") {
            let toml::Value::Array(items) = bias else {
                return Err(DictionaryError::BadBias);
            };
            for item in items {
                let toml::Value::String(term) = item else {
                    return Err(DictionaryError::BadBias);
                };
                bias_terms.push(term.clone());
            }
        }

        let mut replacements = Vec::new();
        if let Some(replace) = dictionary.get("replace") {
            let toml::Value::Array(entries) = replace else {
                return Err(DictionaryError::BadReplaceShape);
            };
            for entry in entries {
                let toml::Value::Table(entry) = entry else {
                    return Err(DictionaryError::BadReplaceShape);
                };
                match (entry.get("from"), entry.get("to")) {
                    (Some(toml::Value::String(from)), Some(toml::Value::String(to))) => {
                        replacements.push((from.clone(), to.clone()));
                    }
                    _ => return Err(DictionaryError::BadReplaceEntry),
                }
            }
        }

        Ok(Self {
            bias_terms,
            replacements,
        })
    }
}

/// Resolve a configured path, expanding a leading `~/` against `home`.
///
/// Public because the daemon has to watch the same file this resolves to. A
/// watcher on the unexpanded `~/.config/govox/dictionary.toml` watches a
/// directory that does not exist, and the failure is silent: no error, no
/// events, edits simply never noticed.
#[must_use]
pub fn expand_user(path: &std::path::Path, home: Option<&std::path::Path>) -> std::path::PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix("~/") else {
        return path.to_path_buf();
    };
    match home {
        Some(home) => home.join(rest),
        None => path.to_path_buf(),
    }
}

/// What this desktop session can actually do.
///
/// Pure data, deliberately: the *probe* that fills it in has to read
/// `/dev/uinput`, `$WAYLAND_DISPLAY` and `$PATH`, but everything that consumes
/// the answer — injector selection above all — is decidable from these fields
/// alone. Keeping the struct in `govox-core` is what lets the selector be
/// tested with no desktop session at all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub session_type: String,
    pub desktop: String,
    pub supported: bool,
    pub primary_injection: Option<String>,
    /// Injection backends that could work here, best first.
    pub injection_strategies: Vec<String>,
    pub hotkey_strategies: Vec<String>,
    /// Human-readable notes on why something is unavailable.
    pub reasons: Vec<String>,
    /// Whether IBus is installed, so `[ime]` preedit dictation has something to
    /// register with. Only an availability signal: whether the engine actually
    /// loads is settled at runtime, and degrades to the HUD caption on its own.
    pub ime_available: bool,
}

impl Capabilities {
    /// Whether `name` appears among the workable injection backends.
    #[must_use]
    pub fn supports_injection(&self, name: &str) -> bool {
        self.injection_strategies.iter().any(|s| s == name)
    }
}

/// Errors that cross a component boundary.
#[derive(Debug, thiserror::Error)]
pub enum GovoxError {
    #[error("microphone capture cannot start: {0}")]
    MicrophoneUnavailable(String),
    #[error("injection rejected: {0}")]
    InjectionRejected(String),
    /// Recognition failed. Distinct from [`Self::InjectionRejected`] because a
    /// decode that fails has produced no text to inject — reporting it as an
    /// injection failure sends a reader looking at `ydotool` for a fault that
    /// is in the model.
    #[error("speech could not be recognised: {0}")]
    RecognitionFailed(String),
    #[error("this desktop cannot support dictation injection: {0}")]
    UnsupportedCompositor(String),
}

/// Turns a window of audio into words with timings.
///
/// This is the seam the streaming processor decodes through, so that its window
/// management, trimming and offset arithmetic can be tested against a scripted
/// recognizer instead of a loaded model — and so that a second engine is an
/// added implementation rather than an edit to `OnlineProcessor`.
///
/// Takes `&[f32]` rather than an [`AudioBuffer`]: the streaming window is a
/// slice of a ring buffer that is re-decoded whole on every chunk, and it
/// carries no timestamps of its own — the caller owns session time and shifts
/// the returned spans by its own offset.
///
/// The future is `Send` because the daemon drives the processor from a spawned
/// task. Written as an explicit `impl Future` for that bound rather than as
/// `async fn`, which is the same shape `govox_daemon::Transcriber` uses, and
/// keeps `govox-core` free of an async runtime or an `async-trait` dependency.
#[allow(async_fn_in_trait)]
pub trait WordRecognizer: Send + Sync {
    /// Decode one window. Spans are relative to the *window*, not the session.
    fn transcribe_words(
        &self,
        audio: &[f32],
    ) -> impl std::future::Future<Output = Result<Vec<crate::streaming::TimedWord>, GovoxError>> + Send;

    /// Load and warm the model so the first real utterance is not slow.
    ///
    /// A default no-op, which is what replaces `govox-py`'s
    /// `getattr(recognizer, "warm_up", None)` duck-typing.
    fn warm_up(&self) -> impl std::future::Future<Output = Result<(), GovoxError>> + Send {
        async { Ok(()) }
    }
}

/// Cleans up recognised text and classifies what the user meant by it.
pub trait Corrector: Send + Sync {
    fn correct(&self, text: &str) -> CorrectionResult;
}

/// Puts text or keystrokes into the focused application.
pub trait Injector: Send + Sync {
    fn insert(&self, action: &InsertionAction) -> Result<(), GovoxError>;
}

/// What govox believes is in the focused field.
///
/// Implementations differ in how much they actually know, and **callers must
/// treat every read as best-effort**. The dictation buffer knows only what
/// govox itself injected, and only for as long as that record can still be
/// trusted. An AT-SPI backend can read the real widget, but only for some
/// applications: coverage is a property of the focused element, not of the
/// desktop, so the same backend answers differently from one utterance to the
/// next.
///
/// That is why [`TextModel::read_field`] returns `None` rather than erroring,
/// and why no command may *require* it. Field access is an enhancement, never a
/// dependency.
pub trait TextModel: Send + Sync {
    /// Text injected by the most recent dictated utterance, if still known.
    fn last_insertion(&self) -> Option<String>;

    /// Note that `text` was just injected at the caret.
    fn record_insertion(&self, text: &str);

    /// Take the last insertion and forget it.
    ///
    /// Called after a delete, so a second "delete that" cannot eat the same
    /// span twice over.
    fn consume_last(&self) -> Option<String>;

    /// The focused field right now, or `None` when it cannot be read.
    ///
    /// `None` is the ordinary answer, not an error: it is what every backend
    /// returns for a terminal, for an application whose toolkit exposes no
    /// accessible text, and for the dictation buffer always.
    fn read_field(&self) -> Option<FieldSnapshot> {
        None
    }

    /// A label for the focused application, for matching overlay app rules.
    ///
    /// `None` for every backend that cannot name the window — the dictation
    /// buffer always, and AT-SPI when nothing has taken focus yet. A rule that
    /// cannot be shown to apply is not applied, so `None` is a safe answer
    /// rather than a degraded one.
    fn active_window(&self) -> Option<String> {
        None
    }

    /// Drop all state — the focused field changed, so offsets are void.
    fn reset(&self);
}

/// The caret's rectangle in screen coordinates: `(x, y, width, height)`.
///
/// On Wayland this is the only way to learn where the caret is without being
/// the compositor, and it arrives because an input method is *entitled* to ask
/// — a candidate window has to sit under the text being typed. govox uses it to
/// place the HUD.
pub type CaretRect = (i32, i32, i32, i32);

/// Shows dictation as provisional text inside the focused field.
///
/// Preedit is rendered by the application but is **not** in its document, so
/// revising it is a whole-string replace with nothing to verify and nothing to
/// clobber; one commit lands at the end. That is the mechanism macOS Dictation
/// uses, and it is why streaming can revise text without racing the user's
/// typing.
///
/// Every method is best-effort and infallible by design. An input method that
/// goes away mid-session means dictation loses its preedit, not that the daemon
/// fails — the caller's fallback is "streaming behaves as it did before this
/// existed", which is a perfectly good outcome. Calls are fire-and-forget: the
/// implementation queues the work and returns, so nothing on the dictation path
/// ever waits on a desktop service.
pub trait PreeditSink: Send + Sync {
    /// Make govox the active input method for the session.
    fn activate(&self);

    /// Hand the keyboard back to the baseline engine.
    fn deactivate(&self);

    /// Replace the whole preedit with `text`.
    fn preedit(&self, text: &str);

    /// Clear the preedit and commit `text` into the document.
    fn commit(&self, text: &str);

    /// Discard the preedit without committing it.
    fn clear(&self);

    /// What kind of field has focus, by name (`"URL"`, `"TERMINAL"`, ...).
    ///
    /// `None` means "assume nothing", not "assume plain text", and it is the
    /// common answer: clients report a content type only when they choose to.
    fn field_purpose(&self) -> Option<String> {
        None
    }

    /// The document text immediately before the caret, if the client pushes it.
    fn surrounding_text(&self) -> Option<String> {
        None
    }

    /// Where the caret is on screen, if the focused client reports it.
    fn cursor_location(&self) -> Option<CaretRect> {
        None
    }
}

/// A [`WordRecognizer`] that returns canned hypotheses instead of decoding.
///
/// Lives in the library rather than in `tests/` so that `govox-asr` and the
/// daemon's own tests can share it, exactly as [`crate::domain`]'s sibling
/// fakes are shared elsewhere in the workspace.
///
/// Each call to [`WordRecognizer::transcribe_words`] pops the next scripted
/// hypothesis. Running past the end yields an empty hypothesis rather than
/// panicking — a processor that decodes more often than the test scripted for
/// is a legitimate thing to assert about, not a crash.
#[derive(Debug, Default)]
pub struct ScriptedWordRecognizer {
    /// Hypotheses to hand out, in order. Reversed on construction so a pop is
    /// from the front.
    script: std::sync::Mutex<Vec<Vec<crate::streaming::TimedWord>>>,
    /// The length of every window handed to us, so a test can assert on what
    /// the processor actually decoded rather than only on what came back.
    windows: std::sync::Mutex<Vec<usize>>,
    /// Make the Nth call (1-based) fail, to drive the degrade-not-die path.
    fail_nth: Option<usize>,
    calls: std::sync::atomic::AtomicUsize,
}

impl ScriptedWordRecognizer {
    /// A recognizer that returns these hypotheses, one per decode.
    #[must_use]
    pub fn saying(script: Vec<Vec<crate::streaming::TimedWord>>) -> Self {
        let mut script = script;
        script.reverse();
        Self {
            script: std::sync::Mutex::new(script),
            ..Self::default()
        }
    }

    /// As [`Self::saying`], but the Nth decode (1-based) returns an error.
    #[must_use]
    pub fn failing_nth(script: Vec<Vec<crate::streaming::TimedWord>>, nth: usize) -> Self {
        Self {
            fail_nth: Some(nth),
            ..Self::saying(script)
        }
    }

    /// How many decodes were asked for.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The sample count of every window handed over, in order.
    #[must_use]
    pub fn windows(&self) -> Vec<usize> {
        self.windows.lock().unwrap().clone()
    }
}

impl WordRecognizer for ScriptedWordRecognizer {
    fn transcribe_words(
        &self,
        audio: &[f32],
    ) -> impl std::future::Future<Output = Result<Vec<crate::streaming::TimedWord>, GovoxError>> + Send
    {
        let nth = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.windows.lock().unwrap().push(audio.len());
        let result = if self.fail_nth == Some(nth) {
            Err(GovoxError::RecognitionFailed("scripted failure".to_owned()))
        } else {
            Ok(self.script.lock().unwrap().pop().unwrap_or_default())
        };
        async move { result }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preceding_counts_characters_not_bytes() {
        let snap = FieldSnapshot {
            text: "café au lait".into(),
            caret: 6,
        };
        assert_eq!(snap.preceding(6), "café a");
    }

    #[test]
    fn preceding_clamps_at_the_start_of_the_field() {
        let snap = FieldSnapshot {
            text: "hi".into(),
            caret: 2,
        };
        // Shorter than requested, so it cannot match a longer remembered
        // insertion — the comparison fails, which is the intended signal.
        assert_eq!(snap.preceding(10), "hi");
    }

    #[test]
    fn preceding_of_zero_length_is_empty() {
        let snap = FieldSnapshot {
            text: "hi".into(),
            caret: 1,
        };
        assert_eq!(snap.preceding(0), "");
    }

    fn dictionary(body: &str) -> Result<PersonalDictionary, DictionaryError> {
        PersonalDictionary::from_table(&body.parse::<toml::Table>().unwrap())
    }

    #[test]
    fn loads_bias_terms_and_ordered_replacements() {
        let dict = dictionary(
            r#"
[dictionary]
bias = ["Rentals.ca", "ydotool"]

[[dictionary.replace]]
from = "rentals api"
to = "Rentals-API"

[[dictionary.replace]]
from = "see plus plus"
to = "C++"
"#,
        )
        .unwrap();

        assert_eq!(dict.bias_terms, ["Rentals.ca", "ydotool"]);
        assert_eq!(
            dict.replacements,
            [
                ("rentals api".to_owned(), "Rentals-API".to_owned()),
                ("see plus plus".to_owned(), "C++".to_owned()),
            ],
            "order is preserved; replacements apply in sequence"
        );
    }

    #[test]
    fn a_file_without_a_dictionary_table_is_empty_not_an_error() {
        assert_eq!(dictionary("").unwrap(), PersonalDictionary::default());
    }

    #[test]
    fn rejects_malformed_dictionaries() {
        assert!(matches!(
            dictionary("dictionary = 1\n"),
            Err(DictionaryError::NotATable)
        ));
        assert!(matches!(
            dictionary("[dictionary]\nbias = \"nope\"\n"),
            Err(DictionaryError::BadBias)
        ));
        assert!(matches!(
            dictionary("[dictionary]\nbias = [1]\n"),
            Err(DictionaryError::BadBias)
        ));
        assert!(matches!(
            dictionary("[dictionary]\nreplace = [1]\n"),
            Err(DictionaryError::BadReplaceShape)
        ));
        assert!(matches!(
            dictionary("[[dictionary.replace]]\nfrom = \"a\"\n"),
            Err(DictionaryError::BadReplaceEntry)
        ));
    }

    #[test]
    fn expands_a_leading_tilde() {
        let home = std::path::Path::new("/home/example");
        assert_eq!(
            expand_user(
                std::path::Path::new("~/.config/govox/dictionary.toml"),
                Some(home)
            ),
            std::path::PathBuf::from("/home/example/.config/govox/dictionary.toml")
        );
        // Only a leading "~/" expands; a tilde elsewhere is a real filename.
        assert_eq!(
            expand_user(std::path::Path::new("/tmp/a~b.toml"), Some(home)),
            std::path::PathBuf::from("/tmp/a~b.toml")
        );
    }
}
