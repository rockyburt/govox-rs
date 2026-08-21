//! State shared between the daemon's tasks.
//!
//! This is what replaces `govox-py`'s `mode_holder: list[Daemon]` — a
//! one-element list used as a mutable cell so the tray's reload callback and
//! the correction pipeline's closures could reach a `Daemon` that did not exist
//! yet. Here the shared state is built *first* and handed to everyone, so
//! nothing needs a back-reference to the daemon and the cycle disappears rather
//! than being emulated with a weak reference.
//!
//! Reloadable data lives in `ArcSwap`: readers are wait-free and each utterance
//! sees one coherent snapshot. `govox-py` rebinds attributes from the GLib tray
//! thread with no synchronisation at all, which is sound only because of the
//! GIL.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use govox_core::config::Config;
use govox_core::correction::CorrectionPipeline;
use govox_core::domain::PersonalDictionary;
use std::sync::Arc;

/// Everything more than one task needs to see.
pub struct SharedState {
    /// The live configuration. Swapped wholesale on reload.
    pub config: ArcSwap<Config>,
    /// The live personal dictionary.
    pub dictionary: ArcSwap<PersonalDictionary>,
    /// The correction pipeline, rebuilt on reload because it compiles the
    /// dictionary's patterns once at construction.
    pub corrector: ArcSwap<CorrectionPipeline>,
    /// Whether command mode is active.
    command_mode: AtomicBool,
    /// Listening is suspended; only "wake up" is honoured.
    ///
    /// Beside `command_mode` rather than folded into it: they are independent.
    /// Falling asleep in command mode and waking must land back in command
    /// mode, not silently in dictation.
    asleep: AtomicBool,
    /// Whether the input method is holding this session's provisional text.
    ///
    /// Written by the event loop as a session starts and ends, read by the
    /// consumer task when it routes the finished text — so it cannot live on
    /// the `Daemon`, which only the consumer owns. Separate from "a preedit
    /// sink exists": the sink outlives every session, and committing through
    /// it outside one would put text into a field govox never activated for.
    preedit_active: AtomicBool,
    /// Modifier keys physically down.
    ///
    /// Written by the keyboard readers and read by the injection path, which
    /// runs on a different task. That separation is the point: while an
    /// utterance is being transcribed the daemon must still see Ctrl come up,
    /// or [`crate::daemon::Daemon::await_modifiers_released`] would wait on a
    /// value frozen at the moment recognition started.
    held_modifiers: Mutex<BTreeSet<String>>,
    /// The document text before the caret, read once at session start.
    ///
    /// Captured by the event loop when a session begins and read by the
    /// consumer task, so it needs to be shared. Reading it *once*, at the
    /// start, is deliberate: by the time an utterance is being corrected the
    /// field may already be showing govox's own preedit, and a "preceding
    /// text" that includes what govox just said is worse than none.
    preceding: Mutex<Option<String>>,
}

impl SharedState {
    #[must_use]
    pub fn new(config: Config, dictionary: PersonalDictionary) -> Self {
        let corrector = CorrectionPipeline::new(
            config.correction.clone(),
            dictionary.clone(),
            config.editing.command_mode,
        );
        Self {
            config: ArcSwap::from_pointee(config),
            dictionary: ArcSwap::from_pointee(dictionary),
            corrector: ArcSwap::from_pointee(corrector),
            command_mode: AtomicBool::new(false),
            asleep: AtomicBool::new(false),
            preedit_active: AtomicBool::new(false),
            held_modifiers: Mutex::new(BTreeSet::new()),
            preceding: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn command_mode(&self) -> bool {
        self.command_mode.load(Ordering::Relaxed)
    }

    /// Set command mode, returning whether it actually changed.
    ///
    /// Re-entering is not an error — it is what a user does when unsure which
    /// mode they are in — but it must not re-announce.
    pub fn set_command_mode(&self, enabled: bool) -> bool {
        self.command_mode.swap(enabled, Ordering::Relaxed) != enabled
    }

    #[must_use]
    pub fn asleep(&self) -> bool {
        self.asleep.load(Ordering::Relaxed)
    }

    /// Returns whether this changed anything, so a repeat says nothing.
    pub fn set_asleep(&self, asleep: bool) -> bool {
        self.asleep.swap(asleep, Ordering::Relaxed) != asleep
    }

    /// Record a modifier going down or coming up.
    ///
    /// Only modifier *names* are ever stored: this sees every keystroke, and
    /// recording anything else would make it a keylogger.
    pub fn note_modifier(&self, key: &str, down: bool) {
        if !govox_core::activation::is_modifier(key) {
            return;
        }
        let mut held = self.held_modifiers.lock().expect("modifier set poisoned");
        if down {
            held.insert(key.to_owned());
        } else {
            held.remove(key);
        }
    }

    #[must_use]
    pub fn modifiers_held(&self) -> bool {
        !self
            .held_modifiers
            .lock()
            .expect("modifier set poisoned")
            .is_empty()
    }

    /// The held modifiers, for a log line naming what is blocking injection.
    #[must_use]
    pub fn held_modifiers(&self) -> Vec<String> {
        self.held_modifiers
            .lock()
            .expect("modifier set poisoned")
            .iter()
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn preedit_active(&self) -> bool {
        self.preedit_active.load(Ordering::Relaxed)
    }

    pub fn set_preedit_active(&self, active: bool) {
        self.preedit_active.store(active, Ordering::Relaxed);
    }

    /// Record the text before the caret as a session begins.
    pub fn set_preceding(&self, text: Option<String>) {
        *self.preceding.lock().expect("preceding text poisoned") = text;
    }

    /// The text before the caret, as captured at session start.
    #[must_use]
    pub fn preceding(&self) -> Option<String> {
        self.preceding
            .lock()
            .expect("preceding text poisoned")
            .clone()
    }

    /// Publish a new configuration and dictionary in one step.
    ///
    /// Both swaps happen before any reader can observe either, so an utterance
    /// cannot see a new dictionary against an old correction config.
    pub fn publish(&self, config: Config, dictionary: PersonalDictionary) {
        let corrector = CorrectionPipeline::new(
            config.correction.clone(),
            dictionary.clone(),
            config.editing.command_mode,
        );
        self.corrector.store(Arc::new(corrector));
        self.dictionary.store(Arc::new(dictionary));
        self.config.store(Arc::new(config));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use govox_core::config::Environment;

    fn state() -> SharedState {
        let config = Config::load_from(None, &Environment::default()).expect("defaults");
        SharedState::new(config, PersonalDictionary::default())
    }

    #[test]
    fn command_mode_starts_off_and_reports_real_changes_only() {
        let state = state();
        assert!(!state.command_mode());

        assert!(state.set_command_mode(true), "a real change");
        assert!(state.command_mode());
        assert!(!state.set_command_mode(true), "re-entering is not a change");
        assert!(state.set_command_mode(false), "leaving is a change");
    }

    #[test]
    fn modifiers_are_tracked_by_name() {
        let state = state();
        assert!(!state.modifiers_held());

        state.note_modifier("KEY_LEFTCTRL", true);
        assert!(state.modifiers_held());
        assert_eq!(state.held_modifiers(), ["KEY_LEFTCTRL"]);

        state.note_modifier("KEY_LEFTCTRL", false);
        assert!(!state.modifiers_held());
    }

    #[test]
    fn two_modifiers_both_have_to_be_released() {
        let state = state();
        state.note_modifier("KEY_LEFTCTRL", true);
        state.note_modifier("KEY_LEFTSHIFT", true);
        state.note_modifier("KEY_LEFTCTRL", false);
        assert!(state.modifiers_held(), "shift is still down");
        state.note_modifier("KEY_LEFTSHIFT", false);
        assert!(!state.modifiers_held());
    }

    #[test]
    fn ordinary_keys_are_never_recorded() {
        // This sees every keystroke the user types.
        let state = state();
        for key in ["KEY_H", "KEY_U", "KEY_N", "KEY_T", "KEY_2"] {
            state.note_modifier(key, true);
        }
        assert!(!state.modifiers_held());
        assert!(state.held_modifiers().is_empty());
    }

    #[test]
    fn publishing_swaps_config_and_dictionary_together() {
        let state = state();
        assert!(state.dictionary.load().bias_terms.is_empty());

        let mut config = Config::load_from(None, &Environment::default()).expect("defaults");
        config.correction.enabled = false;
        let dictionary = PersonalDictionary {
            bias_terms: vec!["Kubernetes".to_owned()],
            replacements: Vec::new(),
        };
        state.publish(config, dictionary);

        assert!(!state.config.load().correction.enabled);
        assert_eq!(state.dictionary.load().bias_terms, ["Kubernetes"]);
        // The corrector must be rebuilt, not left pointing at the old config:
        // it compiles the dictionary's patterns once at construction.
        assert!(!state.corrector.load().config.enabled);
    }
}
