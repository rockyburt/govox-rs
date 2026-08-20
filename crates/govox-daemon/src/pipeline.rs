//! Wiring: microphone → VAD → Whisper → correction → injection.
//!
//! The routing itself lives in [`crate::daemon`]; this module is the plumbing
//! that feeds it. Long-lived tasks: a keyboard supervisor with an evdev reader
//! per keyboard, the capture supervisor, the event loop, and the utterance
//! consumer.
//!
//! **The consumer is a separate task on purpose.** Processing an utterance
//! inline would block the event loop for the whole of transcription, so the
//! daemon would stop seeing key events — and
//! [`Daemon::await_modifiers_released`](crate::Daemon::await_modifiers_released)
//! would then wait on a modifier set frozen at the moment recognition started,
//! which is exactly the state it exists to observe changing. Modifier tracking
//! is therefore done by the keyboard readers straight into [`SharedState`],
//! ahead of the loop, so it stays live no matter what the loop is doing.
//!
//! Still to come: streaming (M9's processor is built but not yet driven from
//! here) and AT-SPI field reading (M11).

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use govox_asr::whisper::{WhisperHandle, WhisperRecognizer};
use govox_audio::{Backoff, CaptureSupervisor};
use govox_core::activation::{ActivationController, KeyEvent, Transition};
use govox_core::config::Config;
use govox_core::domain::{AudioFrame, Utterance};
use govox_core::domain::{PreeditSink, TextModel};
use govox_core::feedback::{LevelSmoother, SilenceMonitor};
use govox_core::textmodel::DictationBuffer;
use govox_core::vad::VadSegmenter;
use govox_ime::IbusSession;
use govox_input::evdev_listener::{find_keyboard_devices, open_device, to_key_event};
use govox_input::runner::ProcessRunner;
use govox_input::selector::SilentNotify;
use govox_ui::chime::{Chime, RodioSink};
use govox_ui::{DesktopNotifier, OverlayClient, Tray, TrayCommand};
use govox_vad::{SileroVad, SpeechProbability};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::daemon::{Announcer, Daemon, ReloadTrigger, Transcriber};
use crate::feedback::FeedbackChannel;
use crate::state::SharedState;

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error(
        "no keyboard can emit the activation key {key:?}. Run `govox keys` to \
         find your key's name, or check that this user is in the 'input' group."
    )]
    NoKeyboard { key: String },
    #[error("speech recognition could not start: {0}")]
    Asr(#[from] govox_asr::AsrError),
    #[error("the voice activity detector could not start: {0}")]
    Vad(#[from] govox_vad::VadError),
    #[error(transparent)]
    Dictionary(#[from] crate::DictionaryLoadError),
}

/// The Whisper thread-actor, as the daemon sees it.
struct WhisperTranscriber(WhisperHandle);

impl Transcriber for WhisperTranscriber {
    fn set_bias_terms(&self, terms: &[String]) {
        self.0.set_bias_terms(terms);
    }

    async fn transcribe(
        &self,
        audio: &govox_core::domain::AudioBuffer,
    ) -> Result<String, govox_core::domain::GovoxError> {
        self.0.transcribe(audio).await.map_err(Into::into)
    }
}

/// Run dictation until `cancel` fires.
///
/// `config_path` is the `--config` file this run was started from, if any. It
/// is carried rather than re-derived so a reload re-reads the same file the
/// daemon started from, and so the watcher watches it.
///
/// # Errors
/// If the dictionary will not load, no keyboard emits the activation key, or a
/// subsystem fails to start.
pub async fn run(
    config: Config,
    config_path: Option<std::path::PathBuf>,
    cancel: CancellationToken,
) -> Result<(), PipelineError> {
    let dictionary = crate::load_dictionary(&config)?;

    // Resolved here, while `config` is still owned by this function and before
    // anything can swap it: these are the files *this* run was configured from,
    // and the watch has to name them even when they do not exist yet.
    let watched = crate::watch::watched_paths(
        &config,
        config_path.as_deref(),
        &govox_core::config::Environment::from_process(),
    );

    let mut controller = ActivationController::from_config(&config.activation);
    // Which keyboards to open, versus which key must exist somewhere. The stop
    // key is watched but is not proof of a usable keyboard — see `watched_keys`.
    let keys = controller.watched_keys();
    let activation_keys = controller.active_keys().names().to_vec();

    // Any one keyboard emitting any one of the keys is enough: a split or
    // external keyboard may carry only the left Control.
    let devices = find_keyboard_devices(&activation_keys);
    if devices.is_empty() {
        return Err(PipelineError::NoKeyboard {
            key: controller.active_keys().describe(),
        });
    }
    let key = keys.join(", ");
    tracing::info!(
        key = %key,
        mode = %config.activation.mode,
        keyboards = devices.len(),
        "watching for the activation key"
    );

    let queue_size = config.activation.queue_size.max(1) as usize;
    let activation_mode = config.activation.mode;
    let feedback_config = config.feedback.clone();
    let ime_config = config.ime.clone();
    let streaming_config = config.streaming.clone();
    let silence_timeout_s = config.feedback.silence_timeout_s;
    let ttl_s = config.editing.last_insertion_ttl_s;
    let read_focused_field = config.editing.read_focused_field;
    let sample_rate = config.audio.sample_rate;
    let frame_ms = config.audio.frame_ms;
    let device = config.audio.device.clone();
    let vad_config = config.vad.clone();
    let recognition_config = config.recognition.clone();

    // Probed once and shared. `select_injector` needs it to choose a backend,
    // and the About submenu needs to report the same choice — deriving that
    // from a second probe would let the two disagree.
    let caps = probe_capabilities();

    // Shared with the injector so the About menu can report the backend that
    // actually carried the text, not merely the one chosen at startup.
    let injection_report = govox_input::InjectionReport::new();

    let recognizer = WhisperRecognizer::start(&config.recognition, &dictionary, queue_size)?;
    let asr = recognizer.handle();
    let asr_handle = recognizer.handle();

    // Load the model before the first utterance rather than during it: a
    // multi-second cold start on the user's first phrase reads as a hang.
    tracing::info!("loading the speech model…");
    asr.warm_up().await?;

    let shared = Arc::new(SharedState::new(config, dictionary));

    // Every surface is optional and degrades on its own. A desktop with no
    // tray, no notification daemon or no sound card still dictates.
    let (tray, tray_commands) = match Tray::start().await {
        Ok((tray, commands)) => (Some(Arc::new(tray)), Some(commands)),
        Err(error) => {
            tracing::info!(%error, "continuing without a tray icon");
            (None, None)
        }
    };
    let chime = match RodioSink::open() {
        Ok(sink) => Some(Arc::new(Chime::new(sink, 44_100))),
        Err(error) => {
            tracing::info!(%error, "continuing without audio cues");
            None
        }
    };
    // Constructed only when the overlay is configured; started just below.
    // Push-to-talk is exempt: there is no latched session for a click to flip
    // off, and the helper only claims an X11 input region when told
    // click-to-stop is on — withholding the flag keeps the card click-through.
    let click_to_stop = feedback_config.overlay_click_to_stop
        && activation_mode != govox_core::config::ActivationMode::PushToTalk;
    let overlay: Option<Arc<dyn govox_ui::OverlaySink>> = feedback_config.overlay.then(|| {
        Arc::new(OverlayClient::new(
            feedback_config.overlay_position.to_string(),
            click_to_stop,
        )) as Arc<dyn govox_ui::OverlaySink>
    });
    let stop_watcher = click_to_stop.then(|| overlay.clone()).flatten();

    // The input method, when the desktop has one and the user asked for it.
    // A failure here is a degrade, never a stop: without it dictation behaves
    // exactly as it did before preedit existed.
    // Also the stop-key surface: an active engine is the only place govox can
    // *consume* a key, so a single Escape ends a session here where the evdev
    // path needs two. `ime_state` is None when there is no engine, which is
    // what makes the double tap the thing that always works.
    let mut ime_state: Option<Arc<govox_ime::FieldState>> = None;
    let preedit: Option<Arc<dyn PreeditSink>> = if ime_config.enabled {
        match IbusSession::start(&ime_config).await {
            Ok(session) => {
                ime_state = Some(session.state());
                Some(Arc::new(session))
            }
            Err(error) => {
                tracing::info!(%error, "continuing without preedit dictation");
                None
            }
        }
    } else {
        None
    };

    // Reading the focused field is an enhancement, never a dependency: an
    // accessibility bus that will not answer leaves the dictation buffer,
    // which is a complete implementation of the default configuration.
    // The flag records which of the two was actually built. Asking the trait
    // object afterwards is not possible, and `read_focused_field` alone would
    // claim AT-SPI even when the connection failed and the buffer was used —
    // which is exactly the "configured but not in effect" case the About
    // submenu exists to expose.
    let (text_model, field_reading): (Arc<dyn TextModel>, bool) = if read_focused_field {
        match govox_a11y::AtspiTextModel::connect(ttl_s).await {
            Ok(model) => (Arc::new(model), true),
            Err(error) => {
                tracing::info!(%error, "continuing without field reading");
                (Arc::new(DictationBuffer::new(ttl_s)), false)
            }
        }
    } else {
        (Arc::new(DictationBuffer::new(ttl_s)), false)
    };

    // One closure, called now and again at the end of every session. The facts
    // it reads are not all fixed: the injector's is only known once something
    // has been injected, so publishing once at startup would freeze a value
    // that is still "unused" at the time.
    let about: crate::feedback::AboutRefresh = {
        let recognition = recognition_config.clone();
        let caps = caps.clone();
        let method = shared.config.load().injection.method;
        let report = injection_report.clone();
        let preedit_active = preedit.is_some();
        let streaming_enabled = streaming_config.enabled;
        Arc::new(move || {
            about_facts(
                &recognition,
                &caps,
                method,
                report.last(),
                preedit_active,
                field_reading,
                streaming_enabled,
            )
        })
    };

    // Published here rather than at `Tray::start`, because half of it is not
    // known until now: the accessibility bus has only just answered, and the
    // input method either registered or did not.
    if let Some(tray) = tray.as_ref() {
        tray.set_about(about());
    }

    // Started now rather than on the first `show`, so the first session does
    // not wait for a process launch it can see.
    if let Some(overlay) = overlay.as_ref() {
        overlay.prewarm();
    }

    let loop_feedback = feedback_config.clone();
    let announcer = Arc::new(
        FeedbackChannel::new(
            feedback_config,
            tray,
            chime,
            overlay,
            Box::new(DesktopNotifier::new()),
        )
        .with_about(about),
    );

    let (utterances_tx, utterances) = mpsc::channel::<Job>(queue_size);
    let (events_tx, events) = mpsc::channel::<Event>(256);

    // After `prewarm`, which is what guarantees a helper to read from, and
    // after the channel exists to deliver into.
    if let Some(overlay) = stop_watcher {
        let stops = events_tx.clone();
        overlay.watch_stops(Box::new(move || {
            // `try_send`, not `blocking_send`: this runs on the helper's stdout
            // reader thread, and a full queue means the loop is already behind
            // — the user re-clicks long before a blocking send would return.
            let _ = stops.try_send(Event::StopRequested("overlay clicked"));
        }));
    }
    // An Escape the engine consumed arrives as the same event the overlay's
    // stop button sends, so there is one way to stop and one place it happens.
    if let Some(state) = &ime_state {
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel::<()>();
        state.set_stop_channel(stop_tx);
        let stops = events_tx.clone();
        tokio::spawn(async move {
            while stop_rx.recv().await.is_some() {
                if stops
                    .send(Event::StopRequested("escape in a preedit field"))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
    }

    // Reload is a *message*, not a direct call: the swap then happens on the
    // task that owns the daemon rather than on whichever thread the tray menu
    // was clicked from. That is what removes govox-py's cross-thread rebinding.
    let (reloads_tx, reloads) = mpsc::unbounded_channel::<ReloadTrigger>();

    // Held for the life of the run: dropping the watcher stops the watch, and a
    // daemon that has stopped watching looks exactly like one that is.
    let _config_watcher = crate::watch::spawn(&watched, reloads_tx.clone(), &cancel);

    if let Some(mut commands) = tray_commands {
        let events = events_tx.clone();
        tokio::spawn(async move {
            while let Some(command) = commands.recv().await {
                if events.send(Event::Tray(command)).await.is_err() {
                    return;
                }
            }
        });
    }

    spawn_keyboard_supervisor(keys.clone(), &events_tx, &shared, &cancel);
    spawn_capture(&device, sample_rate, frame_ms, &events_tx, &cancel);

    let consumer = tokio::spawn(consume(
        Daemon {
            shared: Arc::clone(&shared),
            transcriber: WhisperTranscriber(asr),
            injector: govox_input::select_injector(
                &caps,
                &shared.config.load(),
                Arc::new(ProcessRunner),
                SilentNotify,
                injection_report.clone(),
            ),
            text_model: Arc::clone(&text_model),
            announcer: Box::new(SharedAnnouncer(Arc::clone(&announcer))),
            preedit: preedit.clone(),
            config_path: config_path.clone(),
            listening: false,
        },
        utterances,
        reloads,
        cancel.clone(),
    ));

    tracing::info!("ready — press your activation key and speak");

    // Streaming turns dictation into a live surface: words appear as
    // provisional text while you speak, not in one block at the end. Without
    // it `[streaming] enabled` is a key that does nothing.
    let streaming = streaming_config
        .enabled
        .then(|| govox_asr::OnlineProcessor::new(asr_handle, &streaming_config, sample_rate));
    if streaming.is_some() {
        tracing::info!(
            min_chunk_s = streaming_config.min_chunk_size_s,
            "streaming enabled — dictation shows as provisional text"
        );
    }

    let mut loop_state = EventLoop {
        controller: &mut controller,
        segmenter: VadSegmenter::from_config(&vad_config),
        vad: SileroVad::new(sample_rate)?,
        utterances: utterances_tx,
        announcer: Arc::clone(&announcer),
        ime_state,
        silence: SilenceMonitor::new(silence_timeout_s),
        level: LevelSmoother::default(),
        reloads: reloads_tx,
        preedit,
        text_model,
        shared: Arc::clone(&shared),
        streaming,
        session_text: String::new(),
        streaming_active: false,
        heard_voice: false,
        voiced_s: 0.0,
        voiced_since_decode: 0.0,
        app_rule: None,
        feedback: loop_feedback,
        last_anchor: None,
        last_compact: None,
        last_decode: None,
        cancel: cancel.clone(),
    };
    loop_state.run(events, &cancel).await;

    cancel.cancel();
    announcer.shutdown();
    // The consumer owns the injector and the model handle; letting it finish
    // means an utterance already in flight lands rather than being cut off
    // half-injected.
    let _ = consumer.await;
    Ok(())
}

/// Lets the daemon and the pipeline share one set of feedback surfaces.
struct SharedAnnouncer<A: Announcer>(Arc<A>);

impl<A: Announcer> Announcer for SharedAnnouncer<A> {
    fn set_state(&self, state: &str) {
        self.0.set_state(state);
    }
    fn caption(&self, text: &str) {
        self.0.caption(text);
    }
    fn notify(&self, title: &str, body: &str) {
        self.0.notify(title, body);
    }
    fn level(&self, value: f32) {
        self.0.level(value);
    }
    // Forwarded rather than defaulted. The trait defaults these to no-ops for
    // log-only announcers, so a wrapper that forgets one fails silently — the
    // bug this overlay has already produced twice.
    fn anchor(&self, caret: Option<govox_core::domain::CaretRect>) {
        self.0.anchor(caret);
    }
    fn compact(&self, compact: bool) {
        self.0.compact(compact);
    }
    fn expect_anchor(&self) {
        self.0.expect_anchor();
    }
    fn caret_marker(&self, enabled: bool) {
        self.0.caret_marker(enabled);
    }
}

/// How much speech to hear before the first streaming decode.
///
/// Not the same thing as `[streaming] min_chunk_size_s`, which bounds the
/// *window*: a window can be long and still hold almost no voice, and that is
/// the case Whisper hallucinates on.
///
/// Kept short deliberately. Transcription itself is 44 ms on this hardware, so
/// every millisecond here is latency the user waits through before their first
/// word appears — and this gate is no longer the only thing standing between a
/// thin first window and the caret. `is_silence_artifact` withholds a stock
/// phrase that slips through, and LocalAgreement will not commit anything a
/// second decode does not confirm. Three tenths of a second is a syllable or
/// two: enough that the model is looking at speech, not at room tone.
const MIN_VOICED_S: f64 = 0.3;

/// How much speech must be sitting in the undecoded tail to decode it.
///
/// A session ends a moment after the last word, so what is left over is
/// usually the silence between finishing speaking and reaching for the key.
/// Decoding that appends Whisper's answer to silence to the end of the user's
/// sentence. Small, because the case worth catching is a genuine final
/// syllable clipped by the stop.
const TAIL_MIN_VOICED_S: f64 = 0.1;

/// How much audio to keep from before the VAD noticed speech.
///
/// Silero reports an onset a frame or two after it actually happens, and the
/// first consonant is the part of an utterance a recogniser can least afford
/// to lose. 300 ms is ten frames at the 30 ms default — generous against that
/// latency while still leaving the first decode overwhelmingly speech.
const PREROLL_S: f64 = 0.3;

/// How often to check whether the set of input devices has changed.
///
/// Only a directory listing, not a device probe, so this can be brisk. It is
/// the delay between plugging a keyboard in and being able to dictate with it,
/// and a second of that is not worth noticing.
const KEYBOARD_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// What the consumer task is handed.
///
/// Two shapes because the two modes decide *when* recognition happens, not what
/// happens after it: utterance mode sends audio and the consumer transcribes
/// it; streaming mode has already transcribed, a few words at a time, and sends
/// the finished text. Both then go through the same correction pass, which is
/// what keeps commands and punctuation identical between them.
enum Job {
    Utterance(Utterance),
    Text(String),
    /// Tear the session down, once everything before it has been committed.
    ///
    /// Queued rather than run on the event loop because the work it ends is
    /// queued: recognition and injection happen on the daemon's own task, so
    /// ending the session the moment the key is released clears the preedit
    /// out from under a commit that has not happened yet. The commit then
    /// finds no live input method and falls back to typing the text one
    /// keystroke at a time — visibly slower, and the whole point of preedit
    /// is that it does not do that.
    EndSession,
}

/// What the event loop reacts to.
enum Event {
    Key(KeyEvent),
    Frame(AudioFrame),
    Tray(TrayCommand),
    /// The user clicked the overlay card.
    ///
    /// An event rather than a direct call because it arrives on the helper's
    /// stdout reader thread, and the activation state belongs to the loop.
    ///
    /// Carries why, because there are now two senders — the overlay's stop
    /// button and an Escape the IBus engine consumed — and "nothing happened"
    /// is only diagnosable if the log says which one asked.
    StopRequested(&'static str),
}

struct EventLoop<'a, A: Announcer> {
    controller: &'a mut ActivationController,
    segmenter: VadSegmenter,
    vad: SileroVad,
    utterances: mpsc::Sender<Job>,
    announcer: Arc<A>,
    /// The IBus engine's view of whether a session is running.
    ///
    /// It consumes an Escape only while one is, so this must track the session
    /// and not merely the engine's existence — otherwise Escape would be eaten
    /// in every preedit-capable field whether govox was listening or not.
    ime_state: Option<Arc<govox_ime::FieldState>>,
    /// Auto-stop a latched session that has gone quiet.
    silence: SilenceMonitor,
    /// Drives the overlay's level meter from capture amplitude.
    level: LevelSmoother,
    /// Asks the daemon to re-read its files.
    reloads: mpsc::UnboundedSender<ReloadTrigger>,
    /// The input method, when one registered.
    preedit: Option<Arc<dyn PreeditSink>>,
    /// What govox believes is in the focused field, for the context read at
    /// session start when the input method cannot supply it.
    text_model: Arc<dyn TextModel>,
    /// Where the session's captured field context is published.
    shared: Arc<SharedState>,
    /// The live-hypothesis processor, when `[streaming] enabled`.
    streaming: Option<govox_asr::OnlineProcessor>,
    /// Everything committed by the streaming recognizer this session.
    session_text: String,
    /// Whether a streaming session is currently running.
    streaming_active: bool,
    /// Whether the VAD has reported speech at any point this session.
    ///
    /// Gates the pre-roll trim; see [`Self::feed_streaming`].
    heard_voice: bool,
    /// Seconds of voiced audio heard this session.
    ///
    /// Gates the streaming decode; see [`Self::feed_streaming`].
    voiced_s: f64,
    /// Seconds of voiced audio since the last decode consumed the buffer.
    ///
    /// Decides whether the leftover tail is worth decoding when the session
    /// ends; see [`TAIL_MIN_VOICED_S`].
    voiced_since_decode: f64,
    /// The per-application overlay override for this session, resolved once
    /// at the start: the focused window does not change mid-session, and
    /// re-resolving it per update would be an AT-SPI round trip per frame.
    app_rule: Option<govox_core::config::OverlayAppRule>,
    /// The overlay's own settings, for the anchoring decisions.
    feedback: govox_core::config::FeedbackConfig,
    /// The last anchor sent, so an unmoved caret is not re-sent.
    ///
    /// `None` is a meaningful value here — "released to the corner" — so the
    /// outer `Option` distinguishes it from "nothing sent yet".
    last_anchor: Option<Option<govox_core::domain::CaretRect>>,
    /// The last compact state sent, deduplicated for the same reason.
    last_compact: Option<bool>,
    /// When the last streaming decode finished, and how long it took.
    ///
    /// Decoding happens on this loop, so a decode that runs longer than the
    /// audio it consumed leaves key events waiting behind the frames that
    /// piled up while it ran. Held so the next decode can be deferred until
    /// the loop has had at least as long to breathe — see [`Self::feed_streaming`].
    last_decode: Option<(std::time::Instant, std::time::Duration)>,
    cancel: CancellationToken,
}

impl<A: Announcer> EventLoop<'_, A> {
    /// Feed one captured frame to the live recognizer, and show what it hears.
    ///
    /// The whole hypothesis is re-corrected every poll rather than diffed
    /// against the last one. `UpdatePreeditText` replaces the entire string
    /// atomically, so a revision from "hello wor" to "Hello, world." is one
    /// call — no divergent-suffix computation, no backspace budget, no rewrite
    /// cap. **Do not reintroduce diffing here**: not needing it is the whole
    /// reason this route is simpler than rewriting text already injected.
    ///
    /// What is shown is what would commit, which is why the correction runs
    /// now — the user watches the finished sentence form, not the raw
    /// transcript.
    async fn feed_streaming(&mut self, frame: &AudioFrame, speech: bool) {
        let speech_began = speech && !self.heard_voice;
        self.heard_voice |= speech;
        if speech {
            let seconds = f64::from(u32::try_from(frame.samples.len()).unwrap_or(u32::MAX))
                / f64::from(frame.sample_rate.max(1));
            self.voiced_s += seconds;
            self.voiced_since_decode += seconds;
        }
        let Some(processor) = self.streaming.as_mut() else {
            return;
        };
        processor.push(&frame.samples);
        // After the frame, so the one that carries the onset survives.
        if speech_began {
            processor.keep_only_last(PREROLL_S);
        }
        if !processor.ready() {
            return;
        }
        // Given near-silence Whisper answers "www.github.com" or
        // "Thank you for watching!", not "nothing". The VAD gate and the
        // pre-roll drop were each necessary and neither sufficient:
        // `min_chunk_size_s` measures the window, not the speech in it — at
        // 0.5 s the first decode saw a 0.3 s pre-roll and 0.2 s of voice. So
        // gate on voice actually heard. Nothing is lost: the audio stays
        // buffered, and a shorter session is decoded in full by `finish`.
        if self.voiced_s < MIN_VOICED_S {
            return;
        }
        // Keep the decode duty cycle at roughly half. The decode is awaited on
        // this loop, so decodes running longer than the audio they consume
        // starve every other event — including the key that ends the session.
        // Yielding for the last decode's duration keeps the daemon answerable.
        if let Some((finished, took)) = self.last_decode
            && finished.elapsed() < took
        {
            return;
        }
        let started = std::time::Instant::now();
        self.voiced_since_decode = 0.0;
        let result = processor.process().await;
        // Recorded for the failure path too: a decode that errors slowly is
        // just as capable of starving the loop as one that succeeds slowly.
        self.last_decode = Some((std::time::Instant::now(), started.elapsed()));
        let update = match result {
            Ok(update) => update,
            Err(error) => {
                // Degrade to the caption rather than taking the session down:
                // a recognizer that stumbles once must not cost the user the
                // words they already said.
                tracing::warn!(%error, "streaming decode failed; keeping the session");
                return;
            }
        };
        if update.is_empty() {
            return;
        }
        let first_words = self.session_text.is_empty() && update.committed.is_empty();
        self.session_text.push_str(&update.committed);
        let hypothesis = format!("{}{}", self.session_text, update.pending);
        // Hold back a session's opening words only when they look like
        // Whisper's answer to silence. LocalAgreement discards such a decode
        // next pass anyway, but `pending` is shown before that check runs,
        // which is how a stock phrase still reached the caret. Withholding
        // *every* first hypothesis was the obvious version and was wrong:
        // behind the voiced gate, a decode and the pacing pause it put the
        // first visible text seconds late, and short utterances showed none.
        if first_words && govox_core::streaming::is_silence_artifact(&hypothesis) {
            tracing::debug!(hypothesis, "withholding an unconfirmed opening hypothesis");
            return;
        }
        self.show_hypothesis(&hypothesis);
    }

    /// Drain the last words, then hand the whole session over to be corrected.
    ///
    /// The preedit is **not** cleared here. The consumer commits through it,
    /// and clearing first would take the provisional text off screen for the
    /// few milliseconds the correction pass takes — a visible flicker at the
    /// exact moment the user is watching for their words to land.
    async fn finish_streaming(&mut self) {
        self.streaming_active = false;
        let heard_voice = self.heard_voice;
        // The final decode is where a session too short to trip `MIN_VOICED_S`
        // gets transcribed at all, so skip it only for holding no speech —
        // never for being brief.
        let decode_tail = self.voiced_since_decode >= TAIL_MIN_VOICED_S;
        let mut tail = match self.streaming.as_mut() {
            Some(processor) if heard_voice => processor.finish(decode_tail).await,
            Some(processor) => {
                processor.reset();
                String::new()
            }
            None => String::new(),
        };
        // Second line of defence, on the tail alone: once the leading edge is
        // guarded a stock phrase arrives as the last words of a sentence, and
        // unlike a provisional hypothesis it would be committed.
        if govox_core::streaming::is_silence_artifact(&tail) {
            tracing::debug!(tail, "dropping a silence artifact from the session tail");
            tail = String::new();
        }
        let text = format!("{}{tail}", self.session_text);
        self.session_text.clear();
        self.announcer.caption("");
        if text.trim().is_empty() {
            // Nothing was heard, so there is nothing to commit — but the
            // provisional text has to go, or an empty session would leave the
            // last hypothesis sitting under the caret.
            if let Some(sink) = self.preedit.as_ref() {
                sink.clear();
            }
            return;
        }
        if self.utterances.try_send(Job::Text(text)).is_err() {
            tracing::warn!("streaming session dropped: the consumer is backlogged");
        }
    }

    /// Put the running hypothesis on both live surfaces.
    ///
    /// One correction pass feeds both, so the HUD and the field can never
    /// disagree about what you just said — they used to, the HUD showing the
    /// words "exclamation mark" while the field showed "!".
    fn show_hypothesis(&mut self, hypothesis: &str) {
        if hypothesis.trim().is_empty() {
            return;
        }
        // Not even as provisional text. A password rendered under the caret is
        // exactly what the refusal exists to prevent, and preedit is on screen.
        if self
            .preedit
            .as_ref()
            .and_then(|sink| sink.field_purpose())
            .as_deref()
            == Some("PASSWORD")
        {
            if let Some(sink) = self.preedit.as_ref() {
                sink.clear();
            }
            self.announcer.caption("");
            return;
        }

        let display = {
            let corrector = self.shared.corrector.load();
            let context = govox_core::correction::Context {
                command_mode: self.shared.command_mode(),
                preceding_text: self.shared.preceding(),
                field_purpose: self.preedit.as_ref().and_then(|sink| sink.field_purpose()),
            };
            let result = corrector.correct(hypothesis, &context);
            // Raw when it is not text: a half-heard "delete tha…" is a command
            // that has not been decided yet, and showing its resolved form
            // would be showing a decision govox has not taken.
            match result.action {
                govox_core::domain::PipelineAction::Text(text) => text,
                _ => hypothesis.to_owned(),
            }
        };

        if let Some(sink) = self.preedit.as_ref() {
            sink.preedit(&display);
        }
        self.announcer.caption(&display);
        // After the preedit, not before: the caret the client reports is the
        // one it has after laying out this text, so asking first would place
        // the card against the previous update's position.
        self.update_anchor();
    }

    async fn run(&mut self, mut events: mpsc::Receiver<Event>, cancel: &CancellationToken) {
        loop {
            let event = tokio::select! {
                () = cancel.cancelled() => break,
                event = events.recv() => match event {
                    Some(event) => event,
                    None => break,
                },
            };
            match event {
                Event::Key(key) => self.on_key(&key).await,
                Event::Frame(frame) => self.on_frame(&frame).await,
                Event::StopRequested(reason) => {
                    // Logged on arrival as well as on effect: a request while
                    // idle is a no-op, and "nothing happened" otherwise cannot
                    // be told apart from "the request never arrived".
                    tracing::debug!(
                        listening = self.controller.listening(),
                        reason,
                        "stop asked"
                    );
                    self.auto_stop(reason).await;
                }
                Event::Tray(command) => match command {
                    TrayCommand::Reload => {
                        let _ = self.reloads.send(ReloadTrigger::Requested);
                    }
                    TrayCommand::Quit => {
                        tracing::info!("quit requested from the tray");
                        self.cancel.cancel();
                        break;
                    }
                },
            }
        }
        tracing::debug!("event loop exited");
    }

    async fn on_key(&mut self, key: &KeyEvent) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        let Some(transition) = self.controller.handle_event_at(key, now) else {
            return;
        };
        tracing::info!(state = transition.state(), "activation");

        self.announcer.set_state(transition.state());

        match transition {
            // A fresh session must not inherit half a phrase from the last one.
            Transition::StartListening => {
                self.set_ime_session(true);
                crate::daemon::begin_session(
                    self.preedit.as_ref(),
                    self.text_model.as_ref(),
                    &self.shared,
                );
                self.segmenter.reset();
                self.silence.reset();
                self.level.reset();
                // A fresh session must not inherit the last one's words, and
                // the recognizer's own buffer holds the audio context that
                // produced them — so both are dropped together.
                self.session_text.clear();
                // The pacing measurement belongs to the session that took it:
                // a slow decode at the end of the last one must not make this
                // one skip its first hypothesis.
                self.last_decode = None;
                // The previous session's speech says nothing about this one,
                // which starts on a keypress and is silent until the user
                // begins.
                self.heard_voice = false;
                self.voiced_s = 0.0;
                self.voiced_since_decode = 0.0;
                // Resolved once: the focused window does not change mid-session,
                // and this is an AT-SPI round trip.
                let window = self.text_model.active_window();
                self.app_rule =
                    govox_core::caret::match_app_rule(window.as_deref(), &self.feedback.app_rules)
                        .cloned();
                // Logged with the label whether or not it matched: a line only
                // on success cannot tell "could not name the window" from
                // "named it, no rule covers it", and those need opposite fixes.
                if !self.feedback.app_rules.is_empty() {
                    tracing::debug!(
                        window = window.as_deref().unwrap_or("<unnamed>"),
                        rule = self.app_rule.as_ref().map(|r| r.match_.as_str()),
                        "resolved the overlay app rule"
                    );
                }
                // Tell the overlay a caret is coming before it settles into a
                // corner it would then have to slide out of. The client reports
                // one a moment after the engine is created, not immediately.
                self.announcer.expect_anchor();
                if self.feedback.overlay_caret_debug {
                    self.announcer.caret_marker(true);
                }
                if let Some(processor) = self.streaming.as_mut() {
                    processor.reset();
                    self.streaming_active = true;
                }
            }
            // Push-to-talk release dispatches immediately rather than waiting
            // out the hangover, which is what `flush` is for.
            Transition::StopListening => {
                self.stop_session().await;
            }
        }
    }

    /// End a latched session that has gone quiet, through the normal stop path.
    ///
    /// Deliberately indistinguishable from a manual stop: it runs the same
    /// transition and the same cues, so the user is not left guessing whether
    /// govox is still listening.
    async fn auto_stop(&mut self, reason: &'static str) {
        let Some(transition) = self.controller.auto_stop() else {
            return;
        };
        tracing::info!(reason, "stopping the session");
        self.announcer.set_state(transition.state());
        // Down the same path as the hotkey. Flushing the segmenter alone left
        // a streaming session running and the input method activated, so a
        // timed-out session never ended the way a hand-stopped one does.
        self.stop_session().await;
    }

    async fn on_frame(&mut self, frame: &AudioFrame) {
        // Frames keep arriving while idle — the stream is not stopped and
        // restarted per session, because that costs hundreds of milliseconds
        // and drops the start of the first word. They are simply ignored.
        if !self.controller.listening() {
            return;
        }
        let probability = match self.vad.probability(frame) {
            Ok(probability) => probability,
            Err(error) => {
                tracing::warn!(%error, "VAD scoring failed; treating the frame as silence");
                0.0
            }
        };

        // Raw amplitude, not the VAD probability: the meter should show the
        // microphone working even for sounds the VAD is sure are not speech,
        // which is what makes it useful for diagnosing a muted input. And
        // sent, not just computed — smoothing it and dropping it on the floor
        // left the card static for a whole session.
        let meter = self
            .level
            .update(govox_core::feedback::compute_rms(&frame.samples));
        self.announcer.level(meter);

        // Streaming owns the session, so the VAD only feeds the silence
        // auto-stop below; the segmenter still runs in utterance mode, which
        // is what `[streaming] enabled = false` gets. Computed separately from
        // `voice`, which folds in the segmenter's mid-phrase state — never fed
        // in streaming mode, so reordering would change what the timer sees.
        let speech = f64::from(probability) >= self.segmenter.speech_threshold;

        if self.streaming_active {
            self.feed_streaming(frame, speech).await;
        } else if let Some(utterance) = self.segmenter.process(frame, f64::from(probability)) {
            self.dispatch(utterance).await;
        }

        // Mid-phrase counts as voice even below the speech threshold: the
        // segmenter is still holding buffered audio, so the speaker has not
        // finished and the auto-stop timer must not advance.
        let voice =
            f64::from(probability) >= self.segmenter.speech_threshold || self.segmenter.in_speech();
        // Cheap: the caret is state the input method pushed to us, so this is a
        // lock and a comparison, not a round trip, and deduplicated. Done here
        // rather than when text arrives because the client reports its caret
        // within milliseconds — the card should be placed before the words are.
        self.update_anchor();

        if self.silence.observe(frame.timestamp, voice) {
            self.auto_stop("silence").await;
        }
    }

    /// Put the card under the caret, when the client reports one worth using.
    ///
    /// Driven from the hypothesis updates rather than a timer: the caret only
    /// moves as text is inserted, so the two happen together, and a timer
    /// would poll a D-Bus property many times a second for a rectangle that
    /// changes a few times a sentence.
    ///
    /// Both sends are deduplicated. A caret that has not moved would otherwise
    /// mean a write down the overlay's pipe on every update for no visible
    /// change.
    fn update_anchor(&mut self) {
        let Some(sink) = self.preedit.as_ref() else {
            return;
        };
        let location = sink.cursor_location();

        // Whether the card *moves* is a preference; whether the field shows the
        // dictation is a fact, and `compact` depends on the fact. A client that
        // reports a caret is rendering the preedit, so the caption stands down
        // even with following off — or the user reads the same words twice.
        let following = self
            .app_rule
            .as_ref()
            .and_then(|rule| rule.follow_caret)
            .unwrap_or(self.feedback.overlay_follow_caret);

        if following {
            let mut follow =
                govox_core::caret::apply_caret_offset(location, self.app_rule.as_ref());
            // Distrusting the *position* must not change the answer to "is
            // this field showing the dictation" — only the anchor is withheld.
            if self.feedback.overlay_require_caret_width
                && !govox_core::caret::caret_is_trustworthy(location)
            {
                follow = None;
            }
            if self.last_anchor != Some(follow) {
                self.announcer.anchor(follow);
                self.last_anchor = Some(follow);
            }
        }

        let compact = location.is_some();
        if self.last_compact != Some(compact) {
            self.announcer.compact(compact);
            self.last_compact = Some(compact);
        }
    }

    /// Flush whatever is still held, then queue the session teardown behind it.
    ///
    /// The one stop path, shared by the hotkey and the silence timer, so an
    /// automatic stop really is indistinguishable from a manual one.
    /// Tell the IBus engine whether a session is running.
    ///
    /// A no-op without an engine, which is the common case.
    fn set_ime_session(&self, active: bool) {
        if let Some(state) = &self.ime_state {
            state.set_session_active(active);
        }
    }

    async fn stop_session(&mut self) {
        // First, so an Escape arriving while the commit is still in flight is
        // passed to the application rather than consumed by an engine that is
        // about to have nothing to stop.
        self.set_ime_session(false);
        // The next session re-sends its anchor rather than being deduplicated
        // against this one's last position, which would leave the card where
        // the previous field's caret happened to be.
        self.last_anchor = None;
        self.last_compact = None;
        self.app_rule = None;
        if self.streaming_active {
            self.finish_streaming().await;
        } else if let Some(utterance) = self.segmenter.flush() {
            self.dispatch(utterance).await;
        }
        // Behind the work, never in front of it — see `Job::EndSession`.
        if self.utterances.try_send(Job::EndSession).is_err() {
            // A full queue must not leave the input method activated and the
            // keyboard held; end it here and accept that the last commit
            // types instead of landing through the preedit.
            tracing::warn!("recognition is backlogged; ending the session without waiting");
            crate::daemon::end_session(self.preedit.as_ref(), &self.shared);
        }
    }

    async fn dispatch(&self, utterance: Utterance) {
        // try_send, not send: awaiting a full queue would stall the event loop
        // and stop modifier releases being seen. A full queue means recognition
        // is past the configured depth; govox-py's backlog path is dead code.
        if self.utterances.try_send(Job::Utterance(utterance)).is_err() {
            tracing::warn!("utterance dropped: recognition is backlogged");
        }
    }
}

/// Drain utterances one at a time, in order.
///
/// Serial by design: two utterances injected concurrently would interleave
/// their keystrokes in the user's document.
async fn consume<T: Transcriber>(
    mut daemon: Daemon<T>,
    mut utterances: mpsc::Receiver<Job>,
    mut reloads: mpsc::UnboundedReceiver<ReloadTrigger>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            // Handled here rather than on the event loop so the swap happens on
            // the one task that owns the daemon, and never mid-utterance.
            Some(trigger) = reloads.recv() => {
                daemon.reload_from(trigger);
            }
            job = utterances.recv() => match job {
                Some(Job::Utterance(utterance)) => {
                    daemon.listening = false;
                    daemon.process_utterance(&utterance).await;
                }
                Some(Job::Text(text)) => {
                    daemon.listening = false;
                    daemon.process_text(&text).await;
                }
                // Last in the queue, so everything it ends has landed.
                Some(Job::EndSession) => {
                    crate::daemon::end_session(daemon.preedit.as_ref(), &daemon.shared);
                }
                None => break,
            },
        }
    }
    tracing::debug!("utterance consumer exited");
}

/// Which of `found` are not being read yet.
///
/// Split out so the bookkeeping that decides whether a keyboard gets a reader
/// is testable without a `/dev/input` node to plug and unplug.
fn unwatched(found: &[PathBuf], watched: &HashSet<PathBuf>) -> Vec<PathBuf> {
    found
        .iter()
        .filter(|path| !watched.contains(*path))
        .cloned()
        .collect()
}

/// A cheap fingerprint of which event nodes exist.
///
/// [`find_keyboard_devices`] has to *open* every node to ask which keys it
/// supports, which is too much to repeat on a timer. Reading the directory does
/// not, so this is what decides when the expensive scan is worth running.
fn input_nodes() -> BTreeSet<std::ffi::OsString> {
    std::fs::read_dir("/dev/input")
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name())
                .filter(|name| name.as_encoded_bytes().starts_with(b"event"))
                .collect()
        })
        .unwrap_or_default()
}

/// Read every keyboard that can send an activation key — now, and later.
///
/// Keyboards are not a fixed set, and this used to assume they were. Unplugging
/// one ended its reader thread with a warning and nothing else; plugging one in
/// was never noticed at all. Swapping keyboards therefore left the daemon
/// running, healthy, and deaf, with no route back but a restart — which is
/// exactly what happened on 2026-08-18 when a wireless keyboard was replaced
/// with a wired one.
///
/// Rescans when a reader dies, and when the contents of `/dev/input` change.
/// Polling that directory is cheap; opening every node to ask what keys it
/// supports is not, so the listing is what gates the real scan.
fn spawn_keyboard_supervisor(
    keys: Vec<String>,
    events: &mpsc::Sender<Event>,
    shared: &Arc<SharedState>,
    cancel: &CancellationToken,
) {
    let events = events.clone();
    let shared = Arc::clone(shared);
    let cancel = cancel.clone();

    tokio::spawn(async move {
        let mut watched: HashSet<PathBuf> = HashSet::new();
        let mut nodes = input_nodes();
        // Reader threads report their own death here, so a disconnect is acted
        // on immediately rather than at the next tick.
        let (died_tx, mut died_rx) = mpsc::unbounded_channel::<PathBuf>();
        let mut rescan = true;

        loop {
            if rescan {
                for path in unwatched(&find_keyboard_devices(&keys), &watched) {
                    let Ok(mut device) = open_device(&path) else {
                        tracing::warn!(path = %path.display(), "cannot read keyboard; skipping");
                        continue;
                    };
                    tracing::info!(path = %path.display(), "reading keyboard");
                    watched.insert(path.clone());

                    let events = events.clone();
                    let shared = Arc::clone(&shared);
                    let cancel = cancel.clone();
                    let died = died_tx.clone();
                    // A blocking thread per keyboard rather than AsyncFd: there
                    // are two or three, almost always idle, and this keeps
                    // evdev's blocking read out of the async machinery.
                    std::thread::spawn(move || {
                        while !cancel.is_cancelled() {
                            let Ok(batch) = device.fetch_events() else {
                                tracing::warn!(path = %path.display(), "keyboard disconnected");
                                let _ = died.send(path);
                                return;
                            };
                            for event in batch {
                                let Some(key) = to_key_event(&event) else {
                                    continue;
                                };
                                // Recorded here, ahead of the event loop, so the
                                // modifier set stays live while the loop is
                                // busy. See the module docs — this is the whole
                                // reason the split exists.
                                shared.note_modifier(key.key(), matches!(key, KeyEvent::Down(_)));
                                if events.blocking_send(Event::Key(key)).is_err() {
                                    return;
                                }
                            }
                        }
                        let _ = died.send(path);
                    });
                }
                if watched.is_empty() {
                    // Not fatal here, unlike at startup: a keyboard that goes
                    // away can come back, and the daemon is worth keeping alive
                    // to notice that it did.
                    tracing::warn!(
                        "no keyboard can send the activation key; waiting for one to appear"
                    );
                }
                rescan = false;
            }

            tokio::select! {
                () = cancel.cancelled() => break,
                Some(path) = died_rx.recv() => {
                    watched.remove(&path);
                    rescan = true;
                }
                () = tokio::time::sleep(KEYBOARD_POLL) => {
                    let now = input_nodes();
                    if now != nodes {
                        nodes = now;
                        rescan = true;
                    }
                }
            }
        }
        tracing::debug!("keyboard supervisor exited");
    });
}

fn spawn_capture(
    device: &str,
    sample_rate: u32,
    frame_ms: u32,
    events: &mpsc::Sender<Event>,
    cancel: &CancellationToken,
) {
    let events = events.clone();
    let cancel = cancel.clone();
    let device = device.to_owned();

    tokio::spawn(async move {
        let supervisor =
            CaptureSupervisor::new(device, sample_rate, frame_ms, 64, Backoff::default());
        let result = supervisor
            .run(&cancel, (), |frame| {
                // A dropped frame is 30 ms of audio; blocking the capture pump
                // would be worse.
                let _ = events.try_send(Event::Frame(frame));
            })
            .await;
        if let Err(error) = result {
            tracing::error!(%error, "audio capture stopped");
            cancel.cancel();
        }
    });
}

/// The facts the tray's About submenu reports.
///
/// Pure, so it can be checked without a desktop, a tray or a model — which is
/// the only way the interesting cases get tested at all. Each row answers a
/// question that currently requires reading the journal, and every one of them
/// distinguishes *configured* from *in effect*: a GPU build running on the
/// integrated card, an IBus engine that never registered, an AT-SPI connection
/// that failed and silently left the dictation buffer in charge.
fn about_facts(
    recognition: &govox_core::config::RecognitionConfig,
    caps: &govox_core::domain::Capabilities,
    injection: govox_core::config::InjectionMethod,
    used: govox_input::UsedBackend,
    preedit: bool,
    field_reading: bool,
    streaming: bool,
) -> govox_ui::AboutFacts {
    use govox_core::config::InjectionMethod;
    use govox_input::UsedBackend;

    let backend = govox_asr::Backend::compiled();
    // `unwrap_or(false)` covers the one error case — `device = "cuda"` on a CPU
    // build — which the daemon has already refused to start on, so reporting
    // "not on a GPU" here is unreachable rather than wrong.
    let on_gpu = govox_asr::whisper::resolve_gpu(recognition.device, backend).unwrap_or(false);
    let backend = if on_gpu {
        // "requested", not a bare index. Whether ggml honoured it cannot be
        // asked: whisper.cpp takes `gpu_device` and reports nothing back, and
        // the only evidence is the `ggml_vulkan: N = <name>` lines it prints at
        // startup. Naming the index without that qualifier would claim a
        // verification this cannot perform — the exact overstatement this
        // change exists to remove.
        format!(
            "{} · GPU {} requested",
            backend.name(),
            recognition.gpu_device
        )
    } else {
        backend.name().to_owned()
    };

    // Mirrors `select_injector`'s own rule rather than restating it loosely:
    // ydotool is used when it is preferred *and* available, else the clipboard.
    let prefers_ydotool = matches!(injection, InjectionMethod::Ydotool | InjectionMethod::Auto);
    let selected = if prefers_ydotool && caps.supports_injection("ydotool") {
        "ydotool"
    } else {
        "clipboard"
    };
    // What was chosen and what has actually run are different facts, and the
    // interesting case is when they disagree: `ydotool` selected, rejecting
    // every call, and the fallback quietly carrying the text over the
    // clipboard. Before this, the menu reported the choice and called it truth.
    //
    // The absent clipboard is named too. It is not a detail: with no `wl-copy`
    // there is nothing behind ydotool if it starts rejecting calls, and emoji
    // are dropped rather than pasted — so the row would otherwise read exactly
    // like a machine where both of those still work.
    let no_clipboard = !caps.supports_injection("clipboard");
    let injection = match used {
        _ if selected == "clipboard" && no_clipboard => {
            "nothing available (no ydotool, no wl-copy)".to_owned()
        }
        UsedBackend::NotYet if no_clipboard => {
            format!("{selected} (selected, unused; no clipboard fallback)")
        }
        UsedBackend::NotYet => format!("{selected} (selected, unused)"),
        UsedBackend::Ydotool if no_clipboard => "ydotool (no clipboard fallback)".to_owned(),
        UsedBackend::Ydotool => "ydotool".to_owned(),
        UsedBackend::Clipboard if selected == "ydotool" => {
            "clipboard (ydotool did not carry it)".to_owned()
        }
        UsedBackend::Clipboard => "clipboard".to_owned(),
    };

    let yes_no = |on: bool, name: &str| {
        if on {
            name.to_owned()
        } else {
            "off".to_owned()
        }
    };

    govox_ui::AboutFacts {
        // The build, not the manifest. `CARGO_PKG_VERSION` is "0.1.0" for every
        // commit since the tag, so it cannot answer the question the menu is
        // opened to answer. See this crate's `build.rs`.
        version: env!("GOVOX_BUILD_VERSION").to_owned(),
        // Read from the manifest rather than written out again here, so
        // relicensing cannot leave the menu asserting the old one.
        licence: env!("CARGO_PKG_LICENSE").to_owned(),
        rows: vec![
            ("Model".to_owned(), recognition.model.clone()),
            ("Backend".to_owned(), backend),
            ("Injection".to_owned(), injection),
            ("Preedit".to_owned(), yes_no(preedit, "IBus")),
            ("Field reading".to_owned(), yes_no(field_reading, "AT-SPI")),
            ("Streaming".to_owned(), yes_no(streaming, "on")),
        ],
    }
}

/// What this session can do, as far as the pipeline needs to know.
///
/// The real probe — `/dev/uinput`, `$WAYLAND_DISPLAY`, `$PATH` — is `doctor`'s
/// job in M10. Until then, offer both strategies and let the fallback wrapper
/// discover the truth at runtime, which it does correctly on its own.
fn probe_capabilities() -> govox_core::domain::Capabilities {
    // The same probe `govox doctor` runs, so what the daemon selects and what
    // the diagnostic reports cannot disagree — which was the whole point of
    // there being a diagnostic.
    crate::diagnostics::capabilities(&crate::diagnostics::Probes::default())
}

#[cfg(test)]
mod keyboard_tests {
    use super::{input_nodes, unwatched};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_newly_appeared_keyboard_is_picked_up() {
        let watched: HashSet<PathBuf> = paths(&["/dev/input/event3"]).into_iter().collect();
        let found = paths(&["/dev/input/event3", "/dev/input/event26"]);
        assert_eq!(unwatched(&found, &watched), paths(&["/dev/input/event26"]));
    }

    #[test]
    fn a_keyboard_already_being_read_is_not_opened_twice() {
        // The rescan runs on a timer, so without this every tick would stack
        // another reader thread on the same device.
        let watched: HashSet<PathBuf> = paths(&["/dev/input/event3"]).into_iter().collect();
        assert!(unwatched(&paths(&["/dev/input/event3"]), &watched).is_empty());
    }

    #[test]
    fn a_keyboard_that_went_away_is_simply_absent() {
        // Disconnection is handled by the reader reporting its own death; the
        // scan says nothing about devices that are no longer there.
        let watched: HashSet<PathBuf> = paths(&["/dev/input/event3"]).into_iter().collect();
        assert!(unwatched(&[], &watched).is_empty());
    }

    #[test]
    fn everything_is_new_when_nothing_is_watched() {
        let found = paths(&["/dev/input/event3", "/dev/input/event26"]);
        assert_eq!(unwatched(&found, &HashSet::new()), found);
    }

    #[test]
    fn the_node_listing_holds_only_event_nodes() {
        // Runs on whatever machine builds this, including one with no
        // /dev/input at all, so it asserts the shape rather than the contents.
        for name in input_nodes() {
            assert!(
                name.to_string_lossy().starts_with("event"),
                "{name:?} is not an event node"
            );
        }
    }
}

#[cfg(test)]
mod about_tests {
    use super::about_facts;
    use govox_core::config::{Config, Environment, InjectionMethod};
    use govox_core::domain::Capabilities;
    use govox_input::UsedBackend;

    fn recognition() -> govox_core::config::RecognitionConfig {
        Config::load_from(None, &Environment::default())
            .expect("defaults load")
            .recognition
    }

    fn row<'a>(facts: &'a govox_ui::AboutFacts, label: &str) -> &'a str {
        facts
            .rows
            .iter()
            .find(|(key, _)| key == label)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("no {label} row"))
    }

    fn caps(injection: &[&str]) -> Capabilities {
        Capabilities {
            injection_strategies: injection.iter().map(|s| (*s).to_owned()).collect(),
            ..Default::default()
        }
    }

    /// The common case: ydotool available, and it did the work.
    fn facts(used: UsedBackend) -> govox_ui::AboutFacts {
        about_facts(
            &recognition(),
            &caps(&["ydotool", "clipboard"]),
            InjectionMethod::Auto,
            used,
            false,
            false,
            false,
        )
    }

    // --- version and licence ------------------------------------------------

    /// `CARGO_PKG_VERSION` is "0.1.0" for every commit since the tag, so it
    /// cannot answer "which build is this?". `build.rs` attaches the commit as
    /// semver build metadata, which can.
    ///
    /// The exact value depends on where the build happened — on the tag, past
    /// it, or with no repository at all — so this pins the *shape*: the
    /// manifest version, optionally followed by `+` and the commit.
    #[test]
    fn the_version_is_the_manifest_plus_the_commit() {
        let version = facts(UsedBackend::Ydotool).version;
        let manifest = env!("CARGO_PKG_VERSION");

        let Some(metadata) = version.strip_prefix(manifest) else {
            panic!("{version:?} does not start with the manifest version {manifest:?}");
        };
        match metadata {
            // Standing on the release tag, or built without git.
            "" => {}
            // Anywhere else: `+`, then the commit, and nothing that would make
            // this a *prerelease* — a leading `-` would sort the build below
            // the release it comes after.
            other => {
                let commit = other
                    .strip_prefix('+')
                    .unwrap_or_else(|| panic!("{version:?} must separate metadata with '+'"));
                assert!(
                    !commit.is_empty()
                        && commit
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '.'),
                    "{commit:?} is not valid semver build metadata"
                );
            }
        }
        assert!(
            !version.contains(&format!("{manifest}-")),
            "{version:?} uses a prerelease suffix, which sorts below {manifest}"
        );
    }

    /// Read from the manifest, so relicensing cannot leave this asserting the
    /// old licence.
    #[test]
    fn the_licence_comes_from_the_manifest() {
        assert_eq!(
            facts(UsedBackend::Ydotool).licence,
            env!("CARGO_PKG_LICENSE")
        );
        assert_eq!(facts(UsedBackend::Ydotool).licence, "MIT");
    }

    // --- injection: chosen versus used --------------------------------------

    /// Before anything is injected only the *choice* is known, and the row says
    /// so rather than implying the backend has been exercised.
    #[test]
    fn nothing_injected_yet_is_reported_as_unused() {
        assert_eq!(
            row(&facts(UsedBackend::NotYet), "Injection"),
            "ydotool (selected, unused)"
        );
    }

    #[test]
    fn the_backend_that_did_the_work_is_what_is_reported() {
        assert_eq!(row(&facts(UsedBackend::Ydotool), "Injection"), "ydotool");
    }

    /// The case the whole change exists for: ydotool was chosen, rejected every
    /// call, and the clipboard quietly carried the text. The menu used to
    /// report "ydotool" here.
    #[test]
    fn a_silent_fallback_to_the_clipboard_is_visible() {
        assert_eq!(
            row(&facts(UsedBackend::Clipboard), "Injection"),
            "clipboard (ydotool did not carry it)"
        );
    }

    /// Where the clipboard was the choice, using it is not a fallback and must
    /// not be dressed up as one.
    #[test]
    fn the_clipboard_by_choice_is_not_reported_as_a_fallback() {
        let facts = about_facts(
            &recognition(),
            &caps(&["clipboard"]),
            InjectionMethod::Clipboard,
            UsedBackend::Clipboard,
            false,
            false,
            false,
        );
        assert_eq!(row(&facts, "Injection"), "clipboard");
    }

    /// With no `wl-copy`, ydotool has nothing behind it and emoji are dropped
    /// rather than pasted. The row must not read like a machine where both of
    /// those still work.
    #[test]
    fn a_missing_clipboard_is_named() {
        let facts = about_facts(
            &recognition(),
            &caps(&["ydotool"]),
            InjectionMethod::Auto,
            UsedBackend::Ydotool,
            false,
            false,
            false,
        );
        assert_eq!(row(&facts, "Injection"), "ydotool (no clipboard fallback)");
    }

    /// The session that cannot type at all. Reporting "clipboard" here — which
    /// is what the selector used to record up front — names a working backend
    /// for a machine with none.
    #[test]
    fn no_backend_at_all_says_so() {
        let facts = about_facts(
            &recognition(),
            &caps(&[]),
            InjectionMethod::Auto,
            UsedBackend::NotYet,
            false,
            false,
            false,
        );
        assert_eq!(
            row(&facts, "Injection"),
            "nothing available (no ydotool, no wl-copy)"
        );
    }

    // --- the rows that were already honest ----------------------------------

    #[test]
    fn a_failed_atspi_connection_reads_as_off() {
        let facts = about_facts(
            &recognition(),
            &caps(&["ydotool"]),
            InjectionMethod::Auto,
            UsedBackend::Ydotool,
            false,
            // read_focused_field was true in the config, but connect() failed
            false,
            false,
        );
        assert_eq!(row(&facts, "Field reading"), "off");
    }

    #[test]
    fn active_surfaces_are_named() {
        let facts = about_facts(
            &recognition(),
            &caps(&["ydotool"]),
            InjectionMethod::Auto,
            UsedBackend::Ydotool,
            true,
            true,
            true,
        );
        assert_eq!(row(&facts, "Preedit"), "IBus");
        assert_eq!(row(&facts, "Field reading"), "AT-SPI");
        assert_eq!(row(&facts, "Streaming"), "on");
    }

    /// The GPU index is what was *asked for*. whisper.cpp takes `gpu_device`
    /// and reports nothing back, so claiming it verified would be the same
    /// overstatement this change removes.
    #[test]
    fn the_gpu_index_is_marked_as_requested() {
        let backend = row(&facts(UsedBackend::Ydotool), "Backend").to_owned();
        let compiled = govox_asr::Backend::compiled();
        assert!(backend.starts_with(compiled.name()), "{backend}");
        if compiled.is_gpu() {
            assert!(backend.contains("requested"), "{backend}");
        } else {
            assert_eq!(backend, compiled.name());
        }
    }
}
