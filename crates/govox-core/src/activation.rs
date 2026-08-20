//! Deciding when govox is listening, from keyboard events.
//!
//! Three modes — hold a key, tap it, or double-tap it — over one shared piece
//! of state. Kept pure: the controller is handed [`KeyEvent`]s and returns
//! [`Transition`]s, so every mode can be tested without evdev, without root,
//! and without a keyboard.
//!
//! ## What is deliberately *not* here
//!
//! `govox-py`'s controller also owns an `asyncio.Queue` of utterances, with
//! `enqueue_utterance`, `next_utterance` and a "backlogged" tray state. All of
//! it is dead: `daemon.py` builds its own queue and reads only
//! `controller.queue.maxsize` from this one, so no utterance ever passes
//! through here and the backlog notification cannot fire. It is dropped rather
//! than ported. `[activation] queue_size` keeps its meaning — it sizes the
//! daemon's queue — and is read there. See `docs/parity.md`.

use std::collections::BTreeSet;

use crate::config::{ActivationKeys, ActivationMode};

/// Keys that turn typed characters into shortcuts.
///
/// Injecting text while one of these is physically down does not produce that
/// text — it produces commands.
///
/// This is not hypothetical. In double-tap mode the session stops on the second
/// *key down* of the toggle key, so with the default `KEY_RIGHTCTRL` the daemon
/// begins injecting while Ctrl is still held. Dictating a URL beginning "www"
/// sent Ctrl+W to the browser, closed the tab, and typed the rest of the
/// address into whatever window focus fell through to. Any leading letter is a
/// hazard: Ctrl+Q quits, Ctrl+N opens a window, Ctrl+A selects everything about
/// to be overwritten.
pub const MODIFIER_KEYS: &[&str] = &[
    "KEY_LEFTCTRL",
    "KEY_RIGHTCTRL",
    "KEY_LEFTALT",
    "KEY_RIGHTALT",
    "KEY_LEFTSHIFT",
    "KEY_RIGHTSHIFT",
    "KEY_LEFTMETA",
    "KEY_RIGHTMETA",
];

#[must_use]
pub fn is_modifier(key: &str) -> bool {
    MODIFIER_KEYS.contains(&key)
}

/// One key transition, by canonical evdev name (`KEY_RIGHTCTRL`).
///
/// Autorepeat (evdev value 2) is dropped before it gets here, so a held key
/// produces exactly one `Down`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEvent {
    Down(String),
    Up(String),
}

impl KeyEvent {
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Down(key) | Self::Up(key) => key,
        }
    }
}

/// What the daemon should do about a key event. `None` means nothing changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    StartListening,
    StopListening,
    /// Stop, and throw away what this session has not committed.
    ///
    /// Distinct from [`StopListening`](Self::StopListening) because the two
    /// answer different questions. Stopping is "I am finished, take what I
    /// said"; aborting is "this should not be happening", which is what the
    /// stop key is reached for. Committing on an abort is the one outcome that
    /// cannot be undone by pressing it again.
    Abort,
}

impl Transition {
    /// The tray/overlay state name this transition moves to.
    #[must_use]
    pub const fn state(self) -> &'static str {
        match self {
            Self::StartListening => "listening",
            Self::StopListening | Self::Abort => "idle",
        }
    }
}

/// Tracks listening state across the three activation modes.
///
/// `now_s` is passed in rather than read from the clock so double-tap timing is
/// tested deterministically instead of with sleeps.
#[derive(Debug)]
pub struct ActivationController {
    pub mode: ActivationMode,
    pub push_to_talk_key: ActivationKeys,
    pub toggle_key: ActivationKeys,
    /// Ends a session, double-tapped, in every mode.
    pub stop_key: ActivationKeys,
    last_stop_tap_ts: Option<f64>,
    pub double_tap_s: f64,
    listening: bool,
    toggle_active: bool,
    /// Modifier keys physically down. Only modifier *names* are ever stored:
    /// this type sees every keystroke, and recording anything else would make
    /// it a keylogger.
    held_modifiers: BTreeSet<String>,
    last_tap_ts: Option<f64>,
}

impl ActivationController {
    #[must_use]
    pub fn new(
        mode: ActivationMode,
        push_to_talk_key: impl Into<ActivationKeys>,
        toggle_key: impl Into<ActivationKeys>,
        double_tap_s: f64,
    ) -> Self {
        Self {
            mode,
            push_to_talk_key: push_to_talk_key.into(),
            toggle_key: toggle_key.into(),
            double_tap_s,
            listening: false,
            toggle_active: false,
            held_modifiers: BTreeSet::new(),
            last_tap_ts: None,
            stop_key: ActivationKeys::from(Vec::new()),
            last_stop_tap_ts: None,
        }
    }

    /// The stop key, which is off unless one is set.
    #[must_use]
    pub fn with_stop_key(mut self, stop_key: impl Into<ActivationKeys>) -> Self {
        self.stop_key = stop_key.into();
        self
    }

    #[must_use]
    pub fn from_config(config: &crate::config::ActivationConfig) -> Self {
        Self::new(
            config.mode,
            config.push_to_talk_key.clone(),
            config.toggle_key.clone(),
            f64::from(config.double_tap_ms) / 1000.0,
        )
        .with_stop_key(config.stop_key.clone())
    }

    /// The keys this controller acts on, given its mode.
    ///
    /// Only the active mode's keys are live: govox observes evdev events
    /// without grabbing them, so every watched key also reaches the focused
    /// app. Watching only the mode's keys keeps the inactive one from leaking,
    /// and lets the daemon open just the keyboards that emit them.
    #[must_use]
    pub fn active_keys(&self) -> &ActivationKeys {
        match self.mode {
            ActivationMode::PushToTalk => &self.push_to_talk_key,
            _ => &self.toggle_key,
        }
    }

    /// Every key worth opening a keyboard for: the mode's, plus the stop key.
    ///
    /// Separate from [`active_keys`](Self::active_keys), which stays the
    /// *activation* key alone. Startup fails when no keyboard can emit that
    /// one, and folding Escape in would make almost any keyboard satisfy the
    /// check — Escape is on all of them, which is exactly why it is a good stop
    /// key and a bad thing to prove a keyboard by.
    #[must_use]
    pub fn watched_keys(&self) -> Vec<String> {
        let mut keys = self.active_keys().names().to_vec();
        for key in self.stop_key.names() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        keys
    }

    #[must_use]
    pub const fn listening(&self) -> bool {
        self.listening
    }

    #[must_use]
    pub fn modifiers_held(&self) -> bool {
        !self.held_modifiers.is_empty()
    }

    /// Feed one key event.
    ///
    /// Never log the event: this sees every keystroke from the keyboard, so
    /// logging it would capture everything the user types. Log the returned
    /// transition instead — that is only the configured shortcut.
    pub fn handle_event(&mut self, event: &KeyEvent) -> Option<Transition> {
        self.track_modifier(event);
        match self.mode {
            ActivationMode::Toggle => self.handle_toggle(event),
            ActivationMode::DoubleTap => None, // needs a timestamp; see handle_event_at
            ActivationMode::PushToTalk => self.handle_push_to_talk(event),
        }
    }

    /// Feed one key event with the time it happened, in monotonic seconds.
    ///
    /// Required for double-tap; equivalent to [`handle_event`](Self::handle_event)
    /// in the other two modes.
    pub fn handle_event_at(&mut self, event: &KeyEvent, now_s: f64) -> Option<Transition> {
        // Before the mode's own handling, and in every mode: stopping is not a
        // toggle, and a stop key that only worked in one mode would be a
        // footgun rather than an escape hatch.
        if let Some(stop) = self.handle_stop(event, now_s) {
            return Some(stop);
        }
        if self.mode != ActivationMode::DoubleTap {
            return self.handle_event(event);
        }
        self.track_modifier(event);
        self.handle_double_tap(event, now_s)
    }

    /// Double-tapped stop key, in any mode.
    ///
    /// Only ends a session that is running: with nothing to stop there is
    /// nothing to do, and staying silent keeps a double-tapped Escape from
    /// being anything at all when govox is idle.
    ///
    /// The tap window is the toggle's, because it is the same gesture and a
    /// second timing to tune would be two ways to get it wrong.
    fn handle_stop(&mut self, event: &KeyEvent, now_s: f64) -> Option<Transition> {
        if self.stop_key.is_empty() {
            return None;
        }
        let KeyEvent::Down(key) = event else {
            return None;
        };
        if !self.stop_key.matches(key) {
            // Any other key breaks a pending stop tap, exactly as it breaks a
            // pending toggle tap — "Esc x Esc" in vim is not a request to stop.
            if !is_modifier(key) {
                self.last_stop_tap_ts = None;
            }
            return None;
        }
        match self.last_stop_tap_ts {
            Some(previous) if now_s - previous <= self.double_tap_s => {
                self.last_stop_tap_ts = None;
                self.stop()
            }
            _ => {
                self.last_stop_tap_ts = Some(now_s);
                None
            }
        }
    }

    /// End a running session and discard it, from outside the key path.
    ///
    /// The IBus engine consumes its own Escape, so that stop never reaches
    /// `handle_event_at`; this is the same transition by another door.
    pub fn abort(&mut self) -> Option<Transition> {
        self.stop()
    }

    /// End a running session, or report nothing if none is running.
    fn stop(&mut self) -> Option<Transition> {
        if !self.listening {
            return None;
        }
        self.listening = false;
        self.toggle_active = false;
        Some(Transition::Abort)
    }

    fn track_modifier(&mut self, event: &KeyEvent) {
        if !is_modifier(event.key()) {
            return;
        }
        match event {
            KeyEvent::Down(key) => {
                self.held_modifiers.insert(key.clone());
            }
            KeyEvent::Up(key) => {
                self.held_modifiers.remove(key);
            }
        }
    }

    fn handle_toggle(&mut self, event: &KeyEvent) -> Option<Transition> {
        match event {
            KeyEvent::Down(key) if self.toggle_key.matches(key) => self.flip_toggle(),
            _ => None,
        }
    }

    /// Toggle only on two presses of the key within the window.
    ///
    /// A single incidental press (Right Ctrl as part of a real shortcut) is
    /// ignored, so the activation key can be one used in everyday typing.
    ///
    /// Where several keys are configured they share one timer, so left Control
    /// then right Control is a double tap. That is deliberate: the two Controls
    /// are one key to the person pressing them, and requiring the same physical
    /// key twice would make the gesture fail depending on which hand was free.
    fn handle_double_tap(&mut self, event: &KeyEvent, now_s: f64) -> Option<Transition> {
        let KeyEvent::Down(key) = event else {
            return None;
        };
        if !self.toggle_key.matches(key) {
            // A pending tap is cancelled by any ordinary key pressed after it,
            // because that makes the Control a *chord* rather than a tap.
            // Without this, `Ctrl+C` twice in a terminal — two Control presses
            // inside the window, with a C between them — starts dictation. That
            // is not hypothetical: it is how you interrupt a running command.
            //
            // Modifiers do not cancel, so Ctrl+Shift stays a chord in progress
            // rather than a cancelled tap.
            if !is_modifier(key) {
                self.last_tap_ts = None;
            }
            return None;
        }
        match self.last_tap_ts {
            Some(previous) if now_s - previous <= self.double_tap_s => {
                self.last_tap_ts = None;
                self.flip_toggle()
            }
            _ => {
                self.last_tap_ts = Some(now_s);
                None
            }
        }
    }

    fn handle_push_to_talk(&mut self, event: &KeyEvent) -> Option<Transition> {
        match event {
            KeyEvent::Down(key) if self.push_to_talk_key.matches(key) => self.set_listening(true),
            KeyEvent::Up(key) if self.push_to_talk_key.matches(key) => self.set_listening(false),
            _ => None,
        }
    }

    fn flip_toggle(&mut self) -> Option<Transition> {
        self.toggle_active = !self.toggle_active;
        self.set_listening(self.toggle_active)
    }

    /// Flip a toggle/double-tap session off via the normal stop path.
    ///
    /// The silence auto-stop calls this so an automatic stop is
    /// indistinguishable from a manual one in the feedback layers: it runs the
    /// same `idle` transition and stop cues. A no-op for push-to-talk, which
    /// has no latched session, and when not currently listening.
    pub fn auto_stop(&mut self) -> Option<Transition> {
        if self.mode == ActivationMode::PushToTalk || !self.listening {
            return None;
        }
        self.flip_toggle()
    }

    /// Drive the indicator only on real transitions, so the icon flips to the
    /// "listening" presentation while a shortcut is active and reverts when it
    /// stops. Autorepeat can re-send a key down, so guard against no-op churn.
    fn set_listening(&mut self, value: bool) -> Option<Transition> {
        if value == self.listening {
            return None;
        }
        self.listening = value;
        Some(if value {
            Transition::StartListening
        } else {
            Transition::StopListening
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PTT: &str = "KEY_F12";
    const TOGGLE: &str = "KEY_RIGHTCTRL";

    fn down(key: &str) -> KeyEvent {
        KeyEvent::Down(key.to_owned())
    }

    fn up(key: &str) -> KeyEvent {
        KeyEvent::Up(key.to_owned())
    }

    fn controller(mode: ActivationMode) -> ActivationController {
        ActivationController::new(mode, PTT, TOGGLE, 0.4)
    }

    const STOP: &str = "KEY_ESC";

    fn stopping(mode: ActivationMode) -> ActivationController {
        controller(mode).with_stop_key(STOP)
    }

    /// Start a session the way the mode does, so stopping has something to stop.
    fn listening_double_tap() -> ActivationController {
        let mut c = stopping(ActivationMode::DoubleTap);
        c.handle_event_at(&down(TOGGLE), 0.0);
        assert_eq!(
            c.handle_event_at(&down(TOGGLE), 0.1),
            Some(Transition::StartListening)
        );
        c
    }

    #[test]
    fn a_double_tapped_stop_key_ends_a_session() {
        let mut c = listening_double_tap();
        assert_eq!(c.handle_event_at(&down(STOP), 1.0), None, "first tap");
        assert_eq!(c.handle_event_at(&down(STOP), 1.2), Some(Transition::Abort));
        assert!(!c.listening());
    }

    #[test]
    fn a_single_stop_tap_does_nothing() {
        // The whole reason it is a double tap: govox does not grab the key, so
        // the Escape reaches the application either way.
        let mut c = listening_double_tap();
        assert_eq!(c.handle_event_at(&down(STOP), 1.0), None);
        assert!(c.listening());
    }

    #[test]
    fn two_slow_stop_taps_are_not_a_double_tap() {
        let mut c = listening_double_tap();
        assert_eq!(c.handle_event_at(&down(STOP), 1.0), None);
        assert_eq!(c.handle_event_at(&down(STOP), 2.0), None);
        assert!(c.listening());
    }

    #[test]
    fn a_key_between_the_taps_breaks_them() {
        // "Esc x Esc" in vim is not a request to stop dictating.
        let mut c = listening_double_tap();
        assert_eq!(c.handle_event_at(&down(STOP), 1.0), None);
        assert_eq!(c.handle_event_at(&down("KEY_X"), 1.05), None);
        assert_eq!(c.handle_event_at(&down(STOP), 1.1), None);
        assert!(c.listening());
    }

    #[test]
    fn stopping_when_idle_reports_nothing() {
        let mut c = stopping(ActivationMode::DoubleTap);
        assert_eq!(c.handle_event_at(&down(STOP), 1.0), None);
        assert_eq!(c.handle_event_at(&down(STOP), 1.2), None);
        assert!(!c.listening());
    }

    #[test]
    fn the_stop_key_works_in_every_mode() {
        for mode in [ActivationMode::Toggle, ActivationMode::PushToTalk] {
            let mut c = stopping(mode);
            // Start however this mode starts.
            let start = match mode {
                ActivationMode::PushToTalk => down(PTT),
                _ => down(TOGGLE),
            };
            assert_eq!(
                c.handle_event_at(&start, 0.0),
                Some(Transition::StartListening),
                "{mode:?}"
            );
            assert_eq!(c.handle_event_at(&down(STOP), 1.0), None, "{mode:?}");
            assert_eq!(
                c.handle_event_at(&down(STOP), 1.2),
                Some(Transition::Abort),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn no_stop_key_means_no_stop_behaviour() {
        let mut c = controller(ActivationMode::DoubleTap);
        c.handle_event_at(&down(TOGGLE), 0.0);
        c.handle_event_at(&down(TOGGLE), 0.1);
        assert_eq!(c.handle_event_at(&down(STOP), 1.0), None);
        assert_eq!(c.handle_event_at(&down(STOP), 1.2), None);
        assert!(c.listening(), "Escape must be inert with no stop key set");
    }

    #[test]
    fn the_stop_key_is_watched_but_is_not_an_activation_key() {
        let c = stopping(ActivationMode::DoubleTap);
        assert_eq!(c.active_keys().names(), [TOGGLE], "activation is unchanged");
        assert_eq!(c.watched_keys(), vec![TOGGLE.to_owned(), STOP.to_owned()]);
    }

    #[test]
    fn abort_from_outside_the_key_path_ends_the_session() {
        // The IBus engine consumes its own Escape, so that stop never reaches
        // `handle_event_at`.
        let mut c = listening_double_tap();
        assert_eq!(c.abort(), Some(Transition::Abort));
        assert!(!c.listening());
        // And is a no-op the second time, so a duplicate cannot stop the next
        // session before it starts.
        assert_eq!(c.abort(), None);
    }

    #[test]
    fn abort_reports_idle_like_any_stop() {
        assert_eq!(Transition::Abort.state(), Transition::StopListening.state());
    }

    #[test]
    fn active_key_follows_the_mode() {
        assert_eq!(
            controller(ActivationMode::PushToTalk).active_keys().names(),
            [PTT]
        );
        assert_eq!(
            controller(ActivationMode::Toggle).active_keys().names(),
            [TOGGLE]
        );
        assert_eq!(
            controller(ActivationMode::DoubleTap).active_keys().names(),
            [TOGGLE]
        );
    }

    fn both_controls() -> ActivationController {
        ActivationController::new(
            ActivationMode::DoubleTap,
            PTT,
            vec!["KEY_LEFTCTRL".to_owned(), "KEY_RIGHTCTRL".to_owned()],
            0.4,
        )
    }

    /// The point of the change: whichever hand is free works.
    #[test]
    fn either_control_double_taps_on_its_own() {
        for key in ["KEY_LEFTCTRL", "KEY_RIGHTCTRL"] {
            let mut c = both_controls();
            assert_eq!(c.handle_event_at(&down(key), 0.0), None, "{key} first tap");
            assert_eq!(
                c.handle_event_at(&down(key), 0.2),
                Some(Transition::StartListening),
                "{key} second tap"
            );
        }
    }

    /// The two Controls share one timer, because they are one key to the person
    /// pressing them — requiring the same physical key twice would make the
    /// gesture depend on which hand was free.
    #[test]
    fn left_then_right_control_is_one_double_tap() {
        let mut c = both_controls();
        assert_eq!(c.handle_event_at(&down("KEY_LEFTCTRL"), 0.0), None);
        assert_eq!(
            c.handle_event_at(&down("KEY_RIGHTCTRL"), 0.2),
            Some(Transition::StartListening)
        );
    }

    /// The guard that makes an everyday key safe to bind: one press must never
    /// start dictation, or every copy-paste would.
    #[test]
    fn a_single_control_press_never_activates() {
        let mut c = both_controls();
        assert_eq!(c.handle_event_at(&down("KEY_LEFTCTRL"), 0.0), None);
        assert_eq!(c.handle_event_at(&down("KEY_LEFTCTRL"), 5.0), None);
        assert!(!c.listening());
    }

    /// The reason binding Control is safe at all. `Ctrl+C` twice in a terminal
    /// is two Control presses inside the double-tap window with a `C` between
    /// them; without the chord guard it starts dictation, which is how you
    /// interrupt a running command.
    #[test]
    fn ctrl_c_twice_in_a_row_does_not_start_dictation() {
        let mut c = both_controls();
        // Ctrl+C
        assert_eq!(c.handle_event_at(&down("KEY_LEFTCTRL"), 0.0), None);
        assert_eq!(c.handle_event_at(&down("KEY_C"), 0.05), None);
        c.handle_event_at(&up("KEY_C"), 0.06);
        c.handle_event_at(&up("KEY_LEFTCTRL"), 0.07);
        // Ctrl+C again, well inside the 400ms window
        assert_eq!(c.handle_event_at(&down("KEY_LEFTCTRL"), 0.15), None);
        assert_eq!(c.handle_event_at(&down("KEY_C"), 0.2), None);
        assert!(!c.listening(), "a repeated shortcut is not a double tap");
    }

    /// The guard must not eat the real gesture: a modifier is part of a chord
    /// in progress, not an ordinary key, so it does not cancel a pending tap.
    #[test]
    fn a_modifier_between_taps_does_not_cancel_them() {
        let mut c = both_controls();
        assert_eq!(c.handle_event_at(&down("KEY_LEFTCTRL"), 0.0), None);
        assert_eq!(c.handle_event_at(&down("KEY_LEFTSHIFT"), 0.1), None);
        assert_eq!(
            c.handle_event_at(&down("KEY_RIGHTCTRL"), 0.2),
            Some(Transition::StartListening)
        );
    }

    #[test]
    fn an_unlisted_key_is_still_ignored() {
        let mut c = both_controls();
        assert_eq!(c.handle_event_at(&down("KEY_LEFTSHIFT"), 0.0), None);
        assert_eq!(c.handle_event_at(&down("KEY_LEFTSHIFT"), 0.1), None);
        assert!(!c.listening());
    }

    #[test]
    fn push_to_talk_listens_while_held() {
        let mut c = controller(ActivationMode::PushToTalk);
        assert_eq!(c.handle_event(&down(PTT)), Some(Transition::StartListening));
        assert!(c.listening());
        assert_eq!(c.handle_event(&up(PTT)), Some(Transition::StopListening));
        assert!(!c.listening());
    }

    #[test]
    fn push_to_talk_ignores_autorepeat() {
        let mut c = controller(ActivationMode::PushToTalk);
        assert_eq!(c.handle_event(&down(PTT)), Some(Transition::StartListening));
        // A second key-down with no intervening key-up must not re-fire the
        // indicator: the tray would flicker on every repeat.
        assert_eq!(c.handle_event(&down(PTT)), None);
        assert!(c.listening());
    }

    #[test]
    fn push_to_talk_ignores_other_keys() {
        let mut c = controller(ActivationMode::PushToTalk);
        assert_eq!(c.handle_event(&down("KEY_A")), None);
        assert!(!c.listening());
    }

    #[test]
    fn toggle_flips_on_each_press() {
        let mut c = controller(ActivationMode::Toggle);
        assert_eq!(
            c.handle_event(&down(TOGGLE)),
            Some(Transition::StartListening)
        );
        // The key-up in between must change nothing.
        assert_eq!(c.handle_event(&up(TOGGLE)), None);
        assert_eq!(
            c.handle_event(&down(TOGGLE)),
            Some(Transition::StopListening)
        );
        assert!(!c.listening());
    }

    #[test]
    fn double_tap_needs_two_presses_inside_the_window() {
        let mut c = controller(ActivationMode::DoubleTap);
        assert_eq!(c.handle_event_at(&down(TOGGLE), 0.0), None);
        assert_eq!(
            c.handle_event_at(&down(TOGGLE), 0.3),
            Some(Transition::StartListening)
        );
    }

    #[test]
    fn a_slow_second_press_starts_a_new_window_rather_than_toggling() {
        let mut c = controller(ActivationMode::DoubleTap);
        assert_eq!(c.handle_event_at(&down(TOGGLE), 0.0), None);
        assert_eq!(c.handle_event_at(&down(TOGGLE), 0.5), None, "too slow");
        // ...but it counts as the first tap of a fresh pair.
        assert_eq!(
            c.handle_event_at(&down(TOGGLE), 0.8),
            Some(Transition::StartListening)
        );
    }

    #[test]
    fn a_third_press_does_not_ride_the_second() {
        // After a successful double-tap the timer is cleared, so three presses
        // are one toggle and one pending tap — not two toggles. This is what
        // stops a rapid triple-tap from starting and immediately stopping.
        let mut c = controller(ActivationMode::DoubleTap);
        c.handle_event_at(&down(TOGGLE), 0.0);
        assert_eq!(
            c.handle_event_at(&down(TOGGLE), 0.1),
            Some(Transition::StartListening)
        );
        assert_eq!(c.handle_event_at(&down(TOGGLE), 0.2), None);
        assert!(c.listening());
    }

    #[test]
    fn double_tap_ignores_an_incidental_single_press() {
        // The point of the mode: KEY_RIGHTCTRL is usable in ordinary shortcuts.
        let mut c = controller(ActivationMode::DoubleTap);
        for (i, _) in (0..5).enumerate() {
            let t = i as f64; // a second apart — never a double-tap.
            assert_eq!(c.handle_event_at(&down(TOGGLE), t), None);
            assert_eq!(c.handle_event_at(&up(TOGGLE), t + 0.01), None);
        }
        assert!(!c.listening());
    }

    #[test]
    fn modifiers_are_tracked_across_modes() {
        let mut c = controller(ActivationMode::Toggle);
        assert!(!c.modifiers_held());
        c.handle_event(&down("KEY_LEFTSHIFT"));
        assert!(c.modifiers_held());
        c.handle_event(&up("KEY_LEFTSHIFT"));
        assert!(!c.modifiers_held());
    }

    #[test]
    fn the_toggle_key_is_tracked_when_it_is_itself_a_modifier() {
        // The default toggle key IS a modifier, which is exactly the case that
        // produced the Ctrl+W tab-close bug: the session starts while Ctrl is
        // still physically down.
        let mut c = controller(ActivationMode::DoubleTap);
        c.handle_event_at(&down(TOGGLE), 0.0);
        c.handle_event_at(&down(TOGGLE), 0.1);
        assert!(c.listening());
        assert!(
            c.modifiers_held(),
            "injection must wait for this to clear, or 'www' becomes Ctrl+W"
        );
    }

    #[test]
    fn ordinary_keys_are_never_recorded() {
        // Anything stored here that is not a modifier would be a keylogger.
        let mut c = controller(ActivationMode::Toggle);
        for key in ["KEY_H", "KEY_U", "KEY_N", "KEY_T", "KEY_2"] {
            c.handle_event(&down(key));
        }
        assert!(!c.modifiers_held());
        assert!(c.held_modifiers.is_empty());
    }

    #[test]
    fn auto_stop_ends_a_latched_session() {
        let mut c = controller(ActivationMode::Toggle);
        c.handle_event(&down(TOGGLE));
        assert_eq!(c.auto_stop(), Some(Transition::StopListening));
        assert!(!c.listening());
        // And the next press starts a fresh session rather than resuming.
        assert_eq!(
            c.handle_event(&down(TOGGLE)),
            Some(Transition::StartListening)
        );
    }

    #[test]
    fn auto_stop_is_a_no_op_when_idle_or_push_to_talk() {
        assert_eq!(controller(ActivationMode::Toggle).auto_stop(), None);

        let mut ptt = controller(ActivationMode::PushToTalk);
        ptt.handle_event(&down(PTT));
        assert_eq!(
            ptt.auto_stop(),
            None,
            "push-to-talk has no latched session to end"
        );
        assert!(ptt.listening(), "the key is still physically held");
    }

    #[test]
    fn transitions_name_the_indicator_state() {
        assert_eq!(Transition::StartListening.state(), "listening");
        assert_eq!(Transition::StopListening.state(), "idle");
    }
}
