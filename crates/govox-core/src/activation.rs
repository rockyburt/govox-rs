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

use crate::config::ActivationMode;

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
}

impl Transition {
    /// The tray/overlay state name this transition moves to.
    #[must_use]
    pub const fn state(self) -> &'static str {
        match self {
            Self::StartListening => "listening",
            Self::StopListening => "idle",
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
    pub push_to_talk_key: String,
    pub toggle_key: String,
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
        push_to_talk_key: impl Into<String>,
        toggle_key: impl Into<String>,
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
        }
    }

    #[must_use]
    pub fn from_config(config: &crate::config::ActivationConfig) -> Self {
        Self::new(
            config.mode,
            config.push_to_talk_key.clone(),
            config.toggle_key.clone(),
            f64::from(config.double_tap_ms) / 1000.0,
        )
    }

    /// The single key this controller acts on, given its mode.
    ///
    /// Only one activation key is live at a time: govox observes evdev events
    /// without grabbing them, so every watched key also reaches the focused
    /// app. Watching only the mode's key keeps the inactive one from leaking,
    /// and lets the daemon open just the keyboards that emit it.
    #[must_use]
    pub fn active_key(&self) -> &str {
        match self.mode {
            ActivationMode::PushToTalk => &self.push_to_talk_key,
            _ => &self.toggle_key,
        }
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
        if self.mode != ActivationMode::DoubleTap {
            return self.handle_event(event);
        }
        self.track_modifier(event);
        self.handle_double_tap(event, now_s)
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
            KeyEvent::Down(key) if *key == self.toggle_key => self.flip_toggle(),
            _ => None,
        }
    }

    /// Toggle only on two presses of the key within the window.
    ///
    /// A single incidental press (Right Ctrl as part of a real shortcut) is
    /// ignored, so the activation key can be one used in everyday typing.
    fn handle_double_tap(&mut self, event: &KeyEvent, now_s: f64) -> Option<Transition> {
        let KeyEvent::Down(key) = event else {
            return None;
        };
        if *key != self.toggle_key {
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
            KeyEvent::Down(key) if *key == self.push_to_talk_key => self.set_listening(true),
            KeyEvent::Up(key) if *key == self.push_to_talk_key => self.set_listening(false),
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

    #[test]
    fn active_key_follows_the_mode() {
        assert_eq!(controller(ActivationMode::PushToTalk).active_key(), PTT);
        assert_eq!(controller(ActivationMode::Toggle).active_key(), TOGGLE);
        assert_eq!(controller(ActivationMode::DoubleTap).active_key(), TOGGLE);
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
