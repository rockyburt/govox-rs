//! Fanning state transitions out to the tray, the chime and the notifier.
//!
//! Two rules carry over from `govox-py`, and both are the kind of thing that
//! looks like an implementation detail until it is wrong:
//!
//! 1. **State reaches the wrapped indicator first**, then fans out. The tray is
//!    the authoritative surface; the extras must not be able to reorder it.
//! 2. **Session-scoped layers fire on the session edge**, not on a bare
//!    `listening` transition. `process_utterance` oscillates
//!    `listening`/`transcribing` within one session, so per-transition cues
//!    would chime on every utterance.
//!
//! Each layer call is isolated: one failing surface can never block the others.

use std::sync::Arc;
use std::sync::Mutex;

use govox_core::config::FeedbackConfig;
use govox_core::feedback::{SessionEdge, SessionTracker};
use govox_ui::chime::PlaySink;
use govox_ui::overlay::{OverlayCommand, OverlaySink};
use govox_ui::{Chime, Notifier, Tray};

use crate::daemon::Announcer;

/// Drives every feedback surface from the daemon's state changes.
pub struct FeedbackChannel<S: PlaySink> {
    config: FeedbackConfig,
    tray: Option<Arc<Tray>>,
    chime: Option<Arc<Chime<S>>>,
    overlay: Option<Arc<dyn OverlaySink>>,
    notifier: Box<dyn Notifier>,
    session: Mutex<SessionTracker>,
    tick: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl<S: PlaySink + 'static> FeedbackChannel<S> {
    #[must_use]
    pub fn new(
        config: FeedbackConfig,
        tray: Option<Arc<Tray>>,
        chime: Option<Arc<Chime<S>>>,
        overlay: Option<Arc<dyn OverlaySink>>,
        notifier: Box<dyn Notifier>,
    ) -> Self {
        Self {
            config,
            tray,
            chime,
            overlay,
            notifier,
            session: Mutex::new(SessionTracker::default()),
            tick: Mutex::new(None),
        }
    }

    fn start_layers(&self) {
        if self.config.chime
            && let Some(chime) = &self.chime
        {
            chime.start();
        }
        // Without this the helper is spawned by the first caption and then
        // never mapped, so the HUD can receive a whole session's text and
        // still be invisible.
        if self.config.overlay
            && let Some(overlay) = &self.overlay
        {
            overlay.send(&OverlayCommand::Show);
        }
        if self.config.tray_pulse
            && let Some(tray) = &self.tray
        {
            tray.start_pulse();
        }
        if self.config.tick && self.chime.is_some() {
            self.arm_tick();
        }
    }

    fn stop_layers(&self) {
        self.cancel_tick();
        if self.config.chime
            && let Some(chime) = &self.chime
        {
            chime.stop();
        }
        if self.config.overlay
            && let Some(overlay) = &self.overlay
        {
            overlay.send(&OverlayCommand::Hide);
        }
        if self.config.tray_pulse
            && let Some(tray) = &self.tray
        {
            tray.stop_pulse();
        }
    }

    /// The periodic "still listening" cue.
    ///
    /// A repeating interval rather than a self-rearming one-shot: the reference
    /// re-arms from inside its own callback, so every tick is late by however
    /// long the callback took and the cadence drifts over a long session.
    /// `tokio::interval` holds it.
    fn arm_tick(&self) {
        let Some(chime) = self.chime.clone() else {
            return;
        };
        let mut tick = self.tick.lock().expect("tick poisoned");
        if tick.is_some() {
            return;
        }

        // Clamped: a tiny or zero interval set by hand would otherwise be a
        // continuous tone rather than a reminder.
        let interval = std::time::Duration::from_secs_f64(self.config.tick_interval_s.max(1.0));
        *tick = Some(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // `interval` fires immediately on its first tick, and the session
            // has just played its start cue — so drop that one.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                chime.tick();
            }
        }));
    }

    fn cancel_tick(&self) {
        if let Some(task) = self.tick.lock().expect("tick poisoned").take() {
            task.abort();
        }
    }

    /// End any running session, for shutdown.
    pub fn shutdown(&self) {
        let edge = self.session.lock().expect("session poisoned").finish();
        if edge.is_some() {
            self.stop_layers();
        }
        self.cancel_tick();
        if let Some(overlay) = &self.overlay {
            overlay.shutdown();
        }
        if let Some(tray) = &self.tray {
            tray.shutdown();
        }
    }
}

impl<S: PlaySink + 'static> Announcer for FeedbackChannel<S> {
    fn set_state(&self, state: &str) {
        // The tray sees it first; the extras must not be able to reorder it.
        if let Some(tray) = &self.tray {
            tray.set_state(state);
        }
        let edge = self
            .session
            .lock()
            .expect("session poisoned")
            .observe(state);
        match edge {
            Some(SessionEdge::Started) => self.start_layers(),
            Some(SessionEdge::Stopped) => self.stop_layers(),
            None => {}
        }
    }

    fn caption(&self, text: &str) {
        if self.config.overlay_caption
            && let Some(overlay) = &self.overlay
        {
            overlay.send(&OverlayCommand::Caption(text.to_owned()));
        }
        if !text.is_empty() {
            tracing::info!(text, "caption");
        }
    }

    fn notify(&self, title: &str, body: &str) {
        self.notifier.notify(title, body);
    }

    fn level(&self, value: f32) {
        if self.config.overlay_level
            && let Some(overlay) = &self.overlay
        {
            overlay.send(&OverlayCommand::Level(value));
        }
    }

    fn anchor(&self, caret: Option<govox_core::domain::CaretRect>) {
        if let Some(overlay) = &self.overlay {
            overlay.send(&OverlayCommand::Anchor(caret));
        }
    }

    fn compact(&self, compact: bool) {
        if let Some(overlay) = &self.overlay {
            overlay.send(&OverlayCommand::Compact(compact));
        }
    }

    fn expect_anchor(&self) {
        if self.config.overlay_follow_caret
            && let Some(overlay) = &self.overlay
        {
            overlay.send(&OverlayCommand::ExpectAnchor);
        }
    }

    fn caret_marker(&self, enabled: bool) {
        if let Some(overlay) = &self.overlay {
            overlay.send(&OverlayCommand::CaretMarker(enabled));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use govox_ui::chime::SilentSink;

    /// Records the commands the fan-out sends, in order.
    #[derive(Default)]
    struct RecordingOverlay {
        sent: Mutex<Vec<OverlayCommand>>,
    }

    impl RecordingOverlay {
        fn sent(&self) -> Vec<OverlayCommand> {
            self.sent.lock().expect("sent poisoned").clone()
        }
    }

    impl OverlaySink for RecordingOverlay {
        fn send(&self, command: &OverlayCommand) {
            self.sent
                .lock()
                .expect("sent poisoned")
                .push(command.clone());
        }

        fn shutdown(&self) {}
    }

    struct SilentNotifier;

    impl Notifier for SilentNotifier {
        fn notify(&self, _title: &str, _body: &str) {}
    }

    fn channel(
        config: FeedbackConfig,
        overlay: &Arc<RecordingOverlay>,
    ) -> FeedbackChannel<SilentSink> {
        FeedbackChannel::new(
            config,
            None,
            None,
            Some(Arc::clone(overlay) as Arc<dyn OverlaySink>),
            Box::new(SilentNotifier),
        )
    }

    #[test]
    fn a_session_shows_the_overlay_and_hides_it_again() {
        // The regression this exists for: the fan-out sent captions to a
        // helper it had never told to map its window, so a whole session's
        // text arrived at an overlay nobody could see.
        let overlay = Arc::new(RecordingOverlay::default());
        let channel = channel(FeedbackConfig::default(), &overlay);

        channel.set_state("listening");
        assert_eq!(overlay.sent(), vec![OverlayCommand::Show]);

        channel.set_state("idle");
        assert_eq!(
            overlay.sent(),
            vec![OverlayCommand::Show, OverlayCommand::Hide]
        );
    }

    #[test]
    fn transcribing_within_a_session_does_not_show_the_overlay_twice() {
        // `process_utterance` oscillates listening/transcribing, so a
        // per-transition show would restart the card mid-sentence.
        let overlay = Arc::new(RecordingOverlay::default());
        let channel = channel(FeedbackConfig::default(), &overlay);

        channel.set_state("listening");
        channel.set_state("transcribing");
        channel.set_state("listening");

        assert_eq!(overlay.sent(), vec![OverlayCommand::Show]);
    }

    #[test]
    fn the_microphone_level_reaches_the_overlay() {
        // The one thing on the card that moves while the user speaks. The
        // pipeline smoothed this value and then dropped it, so the card sat
        // completely still for a whole session — which reads as a frozen
        // overlay rather than a listening one.
        let overlay = Arc::new(RecordingOverlay::default());
        let channel = channel(FeedbackConfig::default(), &overlay);

        channel.set_state("listening");
        channel.level(0.4);
        channel.level(0.9);

        assert_eq!(
            overlay.sent(),
            vec![
                OverlayCommand::Show,
                OverlayCommand::Level(0.4),
                OverlayCommand::Level(0.9)
            ]
        );
    }

    #[test]
    fn the_level_is_withheld_when_the_meter_is_turned_off() {
        let config = FeedbackConfig {
            overlay_level: false,
            ..FeedbackConfig::default()
        };
        let overlay = Arc::new(RecordingOverlay::default());
        let channel = channel(config, &overlay);

        channel.set_state("listening");
        channel.level(0.9);

        assert_eq!(overlay.sent(), vec![OverlayCommand::Show]);
    }

    #[test]
    fn the_overlay_stays_dark_when_it_is_turned_off() {
        let config = FeedbackConfig {
            overlay: false,
            ..FeedbackConfig::default()
        };
        let overlay = Arc::new(RecordingOverlay::default());
        let channel = channel(config, &overlay);

        channel.set_state("listening");
        channel.set_state("idle");

        assert!(overlay.sent().is_empty(), "{:?}", overlay.sent());
    }
}
