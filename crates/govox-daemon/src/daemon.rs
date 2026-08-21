//! Routing one utterance from audio to the focused window.
//!
//! Everything here is deliberately independent of *how* the audio arrived, so
//! the whole path — recognise, correct, route, inject — is testable with fakes
//! and no hardware. That is the same split `govox-py` gets from its `Protocol`
//! definitions, and it is why `test_daemon.py` is the largest test file in the
//! project.

use std::sync::Arc;
use std::time::Duration;

use govox_core::domain::{
    EditOp, GovoxError, Injector, InsertionAction, PipelineAction, PreeditSink, TextModel,
    Utterance,
};
use govox_core::editing::compile_edit;
use govox_core::reload::{ReloadOutcome, restart_required};

use crate::state::SharedState;

/// How often to re-check whether the modifiers have come up.
const MODIFIER_POLL: Duration = Duration::from_millis(20);

/// A release event and the compositor's view of it are not simultaneous. This
/// settle is the difference between "Ctrl is up" and "Ctrl is up as far as the
/// application receiving the next keystroke is concerned".
const MODIFIER_SETTLE: Duration = Duration::from_millis(60);

/// Bounded rather than unconditional: someone genuinely resting a finger on
/// Shift must not lose their utterance.
const MODIFIER_TIMEOUT: Duration = Duration::from_millis(1500);

/// Turns audio into text. Implemented by the Whisper thread-actor, and by a
/// canned transcript in tests.
#[allow(async_fn_in_trait)]
pub trait Transcriber: Send + Sync {
    fn transcribe(
        &self,
        audio: &govox_core::domain::AudioBuffer,
    ) -> impl std::future::Future<Output = Result<String, GovoxError>> + Send;

    /// Adopt a reloaded personal dictionary's bias terms.
    ///
    /// Defaulted, in the house style for an optional capability: a recogniser
    /// that cannot be re-biased ignores it rather than being asked whether it
    /// can. Whisper's implementation swaps the initial prompt used by the next
    /// decode.
    fn set_bias_terms(&self, _terms: &[String]) {}
}

/// Where the daemon says things the user should see.
///
/// `govox-py` defines this and then hardcodes a null implementation, so every
/// notification in it today is a no-op. M7 supplies a real one.
pub trait Announcer: Send + Sync {
    /// A transient state name: `idle`, `listening`, `transcribing`.
    fn set_state(&self, state: &str);
    /// A persistent line of text, or empty to clear it.
    fn caption(&self, text: &str);
    /// A desktop notification.
    fn notify(&self, title: &str, body: &str);
    /// Microphone level, 0..1, for the overlay's meter.
    ///
    /// Defaulted because it is called once per audio frame — roughly 33 times
    /// a second — and most implementations have nothing to do with it.
    fn level(&self, _value: f32) {}
    /// Put the card under this caret rectangle, or `None` to release it back
    /// to its configured corner.
    ///
    /// Defaulted for the same reason as `level`: only the overlay cares.
    fn anchor(&self, _caret: Option<govox_core::domain::CaretRect>) {}
    /// Whether the field being dictated into is showing the text itself.
    ///
    /// The card shrinks when it is, because repeating the words the user can
    /// already read under their caret is noise.
    fn compact(&self, _compact: bool) {}
    /// Ask the overlay to hold its corner briefly while a caret is awaited.
    fn expect_anchor(&self) {}
    /// Draw the reported caret rectangle, for calibrating an app rule.
    fn caret_marker(&self, _enabled: bool) {}
    /// Enter or leave a sustained mode; `None` is plain dictation.
    ///
    /// Defaulted, like `level` and `anchor`: only the surfaces that can hold a
    /// standing indicator have anything to do with it. Distinct from
    /// `set_state` on purpose — a mode outlives the utterance, and announcing
    /// it once through a notification that fades is what let someone sit in
    /// command mode believing it had done nothing.
    fn mode(&self, _mode: Option<&str>) {}
}

/// Logs instead of showing anything. The default until M7.
pub struct LogAnnouncer;

impl Announcer for LogAnnouncer {
    fn set_state(&self, state: &str) {
        tracing::debug!(state, "indicator");
    }
    fn caption(&self, text: &str) {
        if !text.is_empty() {
            tracing::info!(text, "caption");
        }
    }
    fn notify(&self, title: &str, body: &str) {
        tracing::info!(title, body, "notification");
    }
}

/// Take the input method, and read the field context once.
///
/// Both halves are best-effort. Activation is asynchronous — it is queued
/// here and lands a few milliseconds later — which is exactly why the
/// field's *purpose* is read live at correction time rather than now: no
/// client reports a content type until govox's engine is active for its
/// field, so a purpose read at this instant is empty on the first session
/// and stale afterwards.
///
/// The surrounding text is the opposite case and is read now, because the
/// preedit govox is about to show would otherwise be part of it.
///
/// Two sources for that text, in order of coverage. The input method is asked
/// first: a client that provides surrounding text does so wherever preedit
/// works, which includes applications AT-SPI reports as readable but *not*
/// writable — Chrome among them. AT-SPI is the fallback for everything else,
/// and `None` is an ordinary answer that simply restores the standalone-
/// sentence behaviour.
pub fn begin_session(
    preedit: Option<&Arc<dyn PreeditSink>>,
    text_model: &dyn TextModel,
    shared: &SharedState,
) {
    shared.set_preedit_active(preedit.is_some());
    if let Some(preedit) = preedit {
        preedit.activate();
        // Empty is the same as absent here: a client that reports "" is telling
        // us the caret is at the start of the field, which is no context at
        // all, and the correction pipeline treats `None` as "assume nothing".
        if let Some(surrounding) = preedit.surrounding_text().filter(|text| !text.is_empty()) {
            shared.set_preceding(Some(surrounding));
            return;
        }
    }
    let snapshot = text_model
        .read_field()
        .map(|field| field.preceding(PRECEDING_CHARS));
    shared.set_preceding(snapshot.filter(|text| !text.is_empty()));
}

/// How much of the document before the caret is worth reading.
///
/// Enough to decide whether an utterance continues a sentence, and no more:
/// this is the user's document, and keeping less of it than necessary is the
/// right default for something that sees everything they type.
const PRECEDING_CHARS: usize = 200;

/// Hand the keyboard back, and drop any preedit still showing.
///
/// Clearing before deactivating is belt and braces: the engine is in CLEAR
/// focus mode, so a preedit left standing would be discarded anyway. Doing
/// it explicitly means the client is told, rather than left to infer it
/// from a focus change it may never see.
pub fn end_session(preedit: Option<&Arc<dyn PreeditSink>>, shared: &SharedState) {
    // Cleared unconditionally and first: an engine left marked active after
    // the session ends would have the next commit go through a sink that is no
    // longer holding anything.
    shared.set_preedit_active(false);
    let Some(preedit) = preedit else {
        return;
    };
    preedit.clear();
    preedit.deactivate();
}

/// What asked for a reload.
///
/// Not bookkeeping: it decides whether the outcome is announced. A reload the
/// user requested from the tray always answers, including "nothing changed" —
/// a menu item that appears to do nothing is worse than a redundant
/// notification. A reload the filesystem triggered fires on every save, so a
/// save with no effect says nothing at all, leaving the notification to mean
/// what it should: something changed, or something needs a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadTrigger {
    /// The tray's Reload item, or anything else a person clicked.
    Requested,
    /// A watched file changed on disk.
    FileChanged,
}

/// Owns the pipeline state. Driven by exactly one task, so nothing is locked.
pub struct Daemon<T: Transcriber> {
    pub shared: Arc<SharedState>,
    pub transcriber: T,
    pub injector: Box<dyn Injector>,
    pub text_model: Arc<dyn TextModel>,
    pub announcer: Box<dyn Announcer>,
    /// The input method, when one registered. `None` is the default and the
    /// common case, and every read through it is optional by design.
    pub preedit: Option<Arc<dyn PreeditSink>>,
    /// Where the running config was loaded from, so a reload re-reads the same
    /// file this run started from rather than the default location.
    pub config_path: Option<std::path::PathBuf>,
    /// Whether a toggle session is still active, for the state returned to
    /// after an utterance.
    pub listening: bool,
}

impl<T: Transcriber> Daemon<T> {
    /// Recognise, correct and act on one utterance.
    ///
    /// Never returns an error: one bad utterance — a transcription glitch, a
    /// failed injection — must not end the consumer task, which would collapse
    /// the whole daemon. It is logged and the next utterance proceeds.
    pub async fn process_utterance(&mut self, utterance: &Utterance) {
        let audio = &utterance.audio;
        let span = tracing::info_span!(
            "govox.process_utterance",
            duration_s = audio.end_ts - audio.start_ts,
            sample_rate = audio.sample_rate,
        );
        let _guard = span.enter();

        self.announcer.set_state("transcribing");
        let outcome = self.transcribe_and_act(utterance).await;
        if let Err(error) = outcome {
            tracing::error!(%error, "utterance processing failed; continuing");
        }

        // Return to "listening" if a toggle session is still active, otherwise
        // idle. Push-to-talk has already released by now.
        self.announcer
            .set_state(if self.listening { "listening" } else { "idle" });
    }

    /// Correct and act on text that is already recognised.
    ///
    /// The tail of [`Self::transcribe_and_act`], reached directly by a
    /// streaming session: the words arrived a few at a time and were shown as
    /// provisional text, but **only this pass decides what enters the
    /// document**, and it is the same pass utterance mode runs. Spoken
    /// punctuation, editing commands, filler dropping, sentence casing and the
    /// personal dictionary therefore apply identically in both modes.
    pub async fn process_text(&mut self, raw_text: &str) {
        if raw_text.trim().is_empty() {
            self.announcer
                .set_state(if self.listening { "listening" } else { "idle" });
            return;
        }
        if let Err(error) = self.correct_and_act(raw_text).await {
            tracing::error!(%error, "streaming finalize failed; continuing");
        }
        self.announcer
            .set_state(if self.listening { "listening" } else { "idle" });
    }

    async fn transcribe_and_act(&mut self, utterance: &Utterance) -> Result<(), GovoxError> {
        let raw_text = self.transcriber.transcribe(&utterance.audio).await?;
        tracing::debug!(chars = raw_text.len(), "recognised");
        self.correct_and_act(&raw_text).await
    }

    async fn correct_and_act(&mut self, raw_text: &str) -> Result<(), GovoxError> {
        let result = {
            // The snapshot is taken once and dropped before the await below, so
            // an utterance is corrected against one coherent configuration even
            // if a reload lands mid-flight.
            let corrector = self.shared.corrector.load();
            let context = govox_core::correction::Context {
                command_mode: self.shared.command_mode(),
                // Captured at session start; see `SharedState::preceding`.
                // AT-SPI is the other source and arrives in M11.
                preceding_text: self.shared.preceding(),
                field_purpose: self.field_purpose(),
            };
            corrector.correct(raw_text, &context)
        };
        tracing::debug!(
            chars = result.corrected_text.len(),
            action = ?std::mem::discriminant(&result.action),
            "corrected"
        );

        self.await_modifiers_released(MODIFIER_TIMEOUT).await;

        // A command said after other words. Under streaming the whole session
        // arrives as one string, so this is the ordinary case rather than the
        // exotic one — see `split_trailing_command`.
        if let PipelineAction::Text(text) = &result.action
            && let Some((prefix, action)) = govox_core::correction::commands::split_trailing_command(
                text,
                self.shared.config.load().editing.command_mode,
            )
        {
            tracing::info!(?action, "a command was said after other words");
            if self.shared.command_mode() {
                // In command mode the words in front are not dictation; they
                // are what was said while nothing was being typed, and typing
                // them now is the failure the mode exists to prevent.
                tracing::info!(%prefix, "command mode: discarded the words before the command");
            } else {
                self.apply_action(PipelineAction::Text(prefix))?;
            }
            return self.apply_action(action);
        }

        self.apply_action(result.action)
    }

    /// What kind of field has focus, read **live** rather than cached.
    ///
    /// Deliberately not captured with the preceding text. Engine activation is
    /// asynchronous, and a client only reports its content type once govox's
    /// engine is active for that field — so anything read at session start is
    /// stale at best and empty on the very first session, which silently
    /// restored prose rules in URL bars. Reading live costs a lock.
    #[must_use]
    pub fn field_purpose(&self) -> Option<String> {
        self.preedit.as_ref()?.field_purpose()
    }

    /// Is this a field govox must not put anything into at all?
    ///
    /// A password field is the one place where transcribing is worse than doing
    /// nothing: the text is not meant to exist outside the user's head, and
    /// govox would put it on screen as preedit before committing it.
    #[must_use]
    pub fn refuses_to_dictate(&self) -> bool {
        self.field_purpose().as_deref() == Some("PASSWORD")
    }

    /// Wait until no modifier key is physically held, then let injection run.
    ///
    /// In double-tap mode the session stops on the second *key down* of the
    /// toggle key, so injection would otherwise begin with Ctrl still down and
    /// the first typed letter would be a shortcut rather than a character.
    /// Dictating a URL starting "www" sent Ctrl+W to the browser, closed the
    /// tab, and typed the rest into whatever window focus fell through to.
    ///
    /// Bounded: after `timeout` it injects anyway and says so, because losing
    /// the text is the worse failure.
    pub async fn await_modifiers_released(&self, timeout: Duration) {
        if !self.shared.modifiers_held() {
            return;
        }

        let deadline = tokio::time::Instant::now() + timeout;
        while self.shared.modifiers_held() {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    held = ?self.shared.held_modifiers(),
                    "injecting with modifiers still held; the first characters may act as shortcuts"
                );
                return;
            }
            tokio::time::sleep(MODIFIER_POLL).await;
        }
        tokio::time::sleep(MODIFIER_SETTLE).await;
    }

    /// Route one pipeline action: edits through the editor, the rest injected.
    ///
    /// An edit that cannot be satisfied tells the user and stops. It must never
    /// fall through to typing the command phrase as literal text.
    pub fn apply_action(&mut self, action: PipelineAction) -> Result<(), GovoxError> {
        // Before everything, including the password check: while asleep the
        // only thing that exists is waking up. Anything else — text, a command,
        // an edit — is discarded unread, which is what "asleep" has to mean if
        // it is to be trusted as a way of shutting govox up.
        if self.shared.asleep() {
            return match action {
                PipelineAction::Sleep { asleep: false } => {
                    self.set_asleep(false);
                    Ok(())
                }
                other => {
                    tracing::debug!(action = ?std::mem::discriminant(&other), "asleep; discarded");
                    Ok(())
                }
            };
        }

        // Checked before anything else, including a mode switch: whatever was
        // said into a password field, the answer is to do nothing with it.
        if self.refuses_to_dictate() {
            tracing::info!("password field: discarded an utterance without acting on it");
            self.announcer
                .notify("govox", "Password field — nothing was dictated.");
            return Ok(());
        }

        match action {
            PipelineAction::Sleep { asleep } => {
                self.set_asleep(asleep);
                Ok(())
            }

            PipelineAction::Spelling { enabled } => {
                self.set_spelling(enabled);
                Ok(())
            }

            // Spelled before the command-mode guard below, because spelling is
            // a way of *entering text* and command mode is a way of not.
            PipelineAction::Text(text) if self.shared.spelling() => self.spell_out(&text),

            PipelineAction::Mode { command_mode } => {
                self.set_command_mode(command_mode);
                Ok(())
            }

            // Nothing matched, and in command mode that is a misrecognition,
            // not something to dictate: typing it would scatter half-heard
            // command words through the document, the failure the mode exists
            // to prevent. Say what was dropped so it can be repeated.
            PipelineAction::Text(text) if self.shared.command_mode() => {
                tracing::info!(%text, "command mode: discarded a non-command utterance");
                self.announcer
                    .caption(&format!("command mode — not a command: {text}"));
                self.announcer.notify(
                    "govox command mode",
                    &format!("Not a command, discarded: {text}"),
                );
                Ok(())
            }

            PipelineAction::Edit(action) => self.apply_edit(&action),

            PipelineAction::Text(text) => self.inject_text(&text),

            PipelineAction::Command(name) => self.injector.insert(&InsertionAction::Command(name)),
        }
    }

    fn apply_edit(&mut self, action: &govox_core::domain::EditAction) -> Result<(), GovoxError> {
        let plan = compile_edit(action, self.text_model.as_ref());
        if !plan.ok() {
            let reason = plan.unsupported.as_deref().unwrap_or("command unavailable");
            tracing::warn!(reason, "edit command unavailable");
            self.announcer.notify("govox", reason);
            return Ok(());
        }

        for step in &plan.actions {
            self.injector.insert(step)?;
        }

        if action.op == EditOp::DeleteLast {
            // Forget the span so a second "delete that" cannot eat text govox
            // never typed.
            self.text_model.consume_last();
        } else {
            // A plan that retypes text (a case transform) leaves different
            // characters on screen than the buffer remembers. Recording them
            // keeps a following "delete that" at the right length.
            let retyped: String = plan
                .actions
                .iter()
                .filter_map(|step| match step {
                    InsertionAction::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if !retyped.is_empty() {
                self.text_model.record_insertion(&retyped);
            }
        }
        Ok(())
    }

    /// Enter or leave command mode, making the change impossible to miss.
    ///
    /// The standing objection to a mode is "an invisible state to get wrong".
    /// The answer is that entering is loud — a persistent caption plus a
    /// notification — and the caption stays up for as long as the mode does,
    /// so the state is never inferred from memory.
    /// Suspend or resume listening.
    ///
    /// The indicator matters more here than anywhere else: asleep, govox looks
    /// exactly like govox that has stopped working. Waking is deliberately
    /// reported even when nothing was said in between, because "did it hear
    /// me?" is the only question a sleeping daemon raises.
    /// Put text into the focused field, through whichever actuator is live.
    ///
    /// Extracted so spelled output takes exactly the same route as dictated
    /// text: the engine is live and holding this session's provisional text, so
    /// the words land as one IBus commit rather than a stream of synthetic
    /// keystrokes. Only the actuator differs between the two paths.
    fn inject_text(&mut self, text: &str) -> Result<(), GovoxError> {
        if let Some(preedit) = self.preedit.as_ref()
            && self.shared.preedit_active()
        {
            preedit.commit(text);
        } else {
            self.injector
                .insert(&InsertionAction::Text(text.to_owned()))?;
        }
        self.text_model.record_insertion(text);
        Ok(())
    }

    /// Type one spelled utterance, or say what could not be spelled.
    ///
    /// Injected as text rather than through the correction pipeline: every
    /// stage in it — casing, spacing, punctuation, the dictionary — exists to
    /// turn speech into prose, and this is the mode for strings that are not
    /// prose. Running them would undo the reason for spelling in the first
    /// place.
    fn spell_out(&mut self, text: &str) -> Result<(), GovoxError> {
        let normalized = govox_core::correction::commands::normalize_command_text(text);
        let Some(spelled) = govox_core::correction::spelling::spell(&normalized) else {
            // Nothing spellable at all is a misrecognition, not dictation: this
            // mode is chosen for strings that must be exact.
            tracing::info!(%text, "spelling: nothing spellable, discarded");
            self.announcer
                .caption(&format!("spelling — not letters: {text}"));
            return Ok(());
        };
        if !spelled.unrecognised.is_empty() {
            // Reported, never guessed at. A wrong character in an identifier is
            // worse than a missing one, because it looks right.
            let missed = spelled.unrecognised.join(", ");
            tracing::info!(missed, "spelling: some tokens named no letter");
            self.announcer
                .caption(&format!("spelling — did not understand: {missed}"));
            self.announcer
                .notify("govox spelling", &format!("Not spelled: {missed}"));
        }
        if spelled.text.is_empty() {
            return Ok(());
        }
        self.inject_text(&spelled.text)
    }

    fn set_spelling(&mut self, enabled: bool) {
        if !self.shared.set_spelling(enabled) {
            return;
        }
        // Exclusive: entering one mode leaves the other.
        if enabled {
            self.shared.set_command_mode(false);
        }
        tracing::info!(enabled, "spelling mode");
        if enabled {
            self.announcer.mode(Some("spelling"));
            self.announcer.caption("spelling — say letters");
            self.announcer
                .notify("govox spelling mode", "On. Say letters, e.g. alpha bravo.");
        } else {
            self.announcer.mode(None);
            self.announcer.caption("");
            self.announcer
                .notify("govox spelling mode", "Off. Dictating again.");
        }
    }

    fn set_asleep(&mut self, asleep: bool) {
        if !self.shared.set_asleep(asleep) {
            return;
        }
        tracing::info!(asleep, "sleep");
        if asleep {
            self.announcer.mode(Some("asleep"));
            self.announcer.caption("asleep — say \"wake up\"");
            self.announcer
                .notify("govox", "Asleep. Say \"wake up\" to resume.");
        } else {
            // Back to whichever mode was in force before, not to nothing: a
            // session that fell asleep in command mode wakes in command mode.
            self.announcer.mode(if self.shared.command_mode() {
                Some("command")
            } else {
                None
            });
            self.announcer.caption("");
            self.announcer.notify("govox", "Awake. Listening again.");
        }
    }

    fn set_command_mode(&mut self, enabled: bool) {
        // "dictation mode" means dictation, whichever mode you were in — so it
        // leaves spelling too. A mode with one exit is one people get stuck in.
        if !enabled && self.shared.set_spelling(false) {
            self.announcer.mode(None);
            self.announcer.caption("");
        }
        if !self.shared.set_command_mode(enabled) {
            return;
        }
        // Exclusive with spelling, the other way round.
        if enabled {
            self.shared.set_spelling(false);
        }
        tracing::info!(enabled, "command mode");
        // The standing indicator first: it is the one that is still there in
        // thirty seconds, when the caption has been overwritten by a transcript
        // and the notification has faded.
        self.announcer
            .mode(if enabled { Some("command") } else { None });
        if enabled {
            self.announcer.caption("command mode — speak a command");
            self.announcer
                .notify("govox command mode", "On. Dictation is suppressed.");
        } else {
            self.announcer.caption("");
            self.announcer
                .notify("govox command mode", "Off. Dictating again.");
        }
    }

    /// Re-read the config and dictionary files and apply what can take effect.
    ///
    /// **A failed reload is not fatal, unlike a failed startup.** Refusing to
    /// start on a broken config is right — govox would otherwise dictate with
    /// settings the user never approved. Here there is a known-good
    /// configuration already loaded and running, so a typo keeps the previous
    /// one rather than killing a live session. What matters is that the failure
    /// is *loud*: captioned, notified and logged, never swallowed.
    pub fn reload(&mut self) -> ReloadOutcome {
        self.reload_from(ReloadTrigger::Requested)
    }

    /// [`Self::reload`], told who asked.
    ///
    /// The reload itself is identical; only how loudly it reports differs. See
    /// [`ReloadTrigger`].
    pub fn reload_from(&mut self, trigger: ReloadTrigger) -> ReloadOutcome {
        let outcome = self.reload_inner();
        if trigger == ReloadTrigger::FileChanged && outcome.is_no_op() {
            tracing::debug!("configuration saved, nothing to apply");
            return outcome;
        }
        let summary = outcome.summary();
        if outcome.ok {
            tracing::info!("{summary}");
        } else {
            tracing::warn!("{summary}");
        }
        self.announcer.caption(&summary);
        self.announcer.notify("govox reload", &summary);
        outcome
    }

    fn reload_inner(&mut self) -> ReloadOutcome {
        let config = match govox_core::config::Config::load(self.config_path.as_deref()) {
            Ok(config) => config,
            Err(error) => return ReloadOutcome::failed(error.to_string()),
        };
        let dictionary = match crate::load_dictionary(&config) {
            Ok(dictionary) => dictionary,
            Err(error) => return ReloadOutcome::failed(error.to_string()),
        };

        let previous = self.shared.config.load_full();
        let mut applied = Vec::new();

        if *self.shared.dictionary.load_full() != dictionary {
            // Re-bias as well as re-publish. The correction pipeline reads the
            // dictionary per utterance, but the recogniser's initial prompt is
            // handed to whisper.cpp per decode, so nothing but this reaches it
            // — and the reload would report "dictionary" while the new word
            // kept coming out wrong.
            self.transcriber.set_bias_terms(&dictionary.bias_terms);
            applied.push("dictionary".to_owned());
        }
        if previous.feedback.app_rules != config.feedback.app_rules {
            applied.push("overlay app rules".to_owned());
        }
        if previous.correction != config.correction {
            applied.push("correction".to_owned());
        }
        if previous.logging != config.logging {
            applied.push("logging".to_owned());
        }

        let needs_restart = restart_required(&previous, &config);
        // One publish, so no reader can see a new dictionary against an old
        // correction config.
        self.shared.publish(config, dictionary);

        ReloadOutcome {
            ok: true,
            error: None,
            applied,
            needs_restart,
        }
    }
}
