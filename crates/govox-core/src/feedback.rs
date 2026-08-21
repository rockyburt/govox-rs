//! The numeric and state-machine half of user feedback.
//!
//! Kept free of audio, D-Bus and windowing so the daemon's capture path can
//! call it directly and the tests can exercise it with no desktop at all. The
//! surfaces that actually make noise or draw pixels live in `govox-ui`.

/// Speech RMS on a normalized [-1, 1] signal sits well under 1.0 in practice,
/// so a flat gain brings typical speech into a visually useful 0..1 meter range
/// instead of barely nudging off zero.
const DEFAULT_GAIN: f32 = 4.0;
const DEFAULT_ALPHA: f32 = 0.3;
const DEFAULT_NOISE_FLOOR: f32 = 0.02;
/// Rising input is followed faster than falling input, the way a VU meter
/// behaves. A single averaging rate has to choose between the two and gets both
/// wrong: fast enough to catch a syllable makes the meter jitter, slow enough
/// to look smooth rounds the syllables off into one long swell. Splitting them
/// lets the bars snap up on a word and ease back down, which is what reads as a
/// voice rather than a slider.
const DEFAULT_ATTACK: f32 = 0.6;

/// Root-mean-square amplitude of one audio frame.
#[must_use]
pub fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

/// Exponential moving average with a noise floor and gain, mapping raw RMS into
/// a 0..1 meter value that rests at zero in a quiet room rather than jittering
/// on ambient noise.
#[derive(Debug, Clone)]
pub struct LevelSmoother {
    /// The *release* rate, for falling input.
    pub alpha: f32,
    /// The rate for rising input.
    pub attack: f32,
    pub noise_floor: f32,
    pub gain: f32,
    level: f32,
}

impl Default for LevelSmoother {
    fn default() -> Self {
        Self {
            alpha: DEFAULT_ALPHA,
            attack: DEFAULT_ATTACK,
            noise_floor: DEFAULT_NOISE_FLOOR,
            gain: DEFAULT_GAIN,
            level: 0.0,
        }
    }
}

impl LevelSmoother {
    pub fn update(&mut self, rms: f32) -> f32 {
        let rate = if rms > self.level {
            self.attack
        } else {
            self.alpha
        };
        self.level = rate * rms + (1.0 - rate) * self.level;
        let scaled = self.level * self.gain;
        if scaled < self.noise_floor {
            return 0.0;
        }
        scaled.min(1.0)
    }

    pub fn reset(&mut self) {
        self.level = 0.0;
    }
}

/// Decide when a toggle session has been silent long enough to auto-stop.
///
/// Pure and clock-injected: callers pass each frame's timestamp and the VAD's
/// current voice state to [`observe`](Self::observe), which returns `true`
/// exactly once the gap since the last voice activity exceeds `timeout_s`.
/// Voice activity (or [`reset`](Self::reset)) restarts the window, so
/// deliberate dictation with natural pauses is never cut off as long as speech
/// keeps occurring.
#[derive(Debug, Clone)]
pub struct SilenceMonitor {
    pub timeout_s: f64,
    last_voice: Option<f64>,
    fired: bool,
}

impl SilenceMonitor {
    #[must_use]
    pub const fn new(timeout_s: f64) -> Self {
        Self {
            timeout_s,
            last_voice: None,
            fired: false,
        }
    }

    /// Restart the window; the next observation seeds the timer afresh.
    pub fn reset(&mut self) {
        self.last_voice = None;
        self.fired = false;
    }

    /// Record an observation and report whether auto-stop should fire now.
    ///
    /// Returns `true` once per silent stretch, when `now - last_voice` first
    /// exceeds `timeout_s`; further silent observations return `false` until
    /// voice activity (or [`reset`](Self::reset)) re-arms the monitor.
    pub fn observe(&mut self, now: f64, voice_active: bool) -> bool {
        if self.last_voice.is_none() {
            self.last_voice = Some(now);
        }
        if voice_active {
            self.last_voice = Some(now);
            self.fired = false;
            return false;
        }
        if self.fired {
            return false;
        }
        // Strictly greater, matching the reference: at exactly the timeout the
        // session survives one more frame.
        if now - self.last_voice.unwrap_or(now) > self.timeout_s {
            self.fired = true;
            return true;
        }
        false
    }
}

/// A dictation session beginning or ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEdge {
    Started,
    Stopped,
}

/// Tracks whether a dictation *session* is running, as distinct from the
/// moment-to-moment state.
///
/// This distinction is the whole point. `process_utterance` oscillates
/// `listening` → `transcribing` → `listening` several times within one session,
/// and the session-scoped layers — the chime, the overlay, the tray pulse —
/// must fire on the session edge only. Driving them from a bare `listening`
/// transition would chime on every utterance, which is noise.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionTracker {
    active: bool,
}

impl SessionTracker {
    #[must_use]
    pub const fn is_active(self) -> bool {
        self.active
    }

    /// Feed a state name; returns an edge only when the session itself changed.
    pub fn observe(&mut self, state: &str) -> Option<SessionEdge> {
        let active = state != "idle";
        match (self.active, active) {
            (false, true) => {
                self.active = true;
                Some(SessionEdge::Started)
            }
            (true, false) => {
                self.active = false;
                Some(SessionEdge::Stopped)
            }
            _ => None,
        }
    }

    /// End the session if one is running, for shutdown.
    pub fn finish(&mut self) -> Option<SessionEdge> {
        self.active.then(|| {
            self.active = false;
            SessionEdge::Stopped
        })
    }
}

/// How a daemon state is presented in the tray.
///
/// Icons are standard freedesktop symbolic names present in most themes, which
/// is why govox ships no icon assets of its own.
#[must_use]
pub fn state_presentation(state: &str) -> (&'static str, &'static str) {
    match state {
        "idle" => ("Idle", "audio-input-microphone-symbolic"),
        // A record dot reads as "capturing now" and is unmistakably different
        // from the idle microphone outline in a monochrome panel.
        "listening" => ("Listening", "media-record-symbolic"),
        "transcribing" => ("Transcribing", "audio-input-microphone-high-symbolic"),
        "backlogged" => ("Backlogged", "dialog-warning-symbolic"),
        _ => ("Idle", FALLBACK_ICON),
    }
}

/// How a sustained *mode* is shown, as distinct from a transient state.
///
/// Modes and states answer different questions and must not share a slot.
/// "Listening" is what govox is doing this second; "command mode" is what it
/// will keep doing until told otherwise. macOS Voice Control keeps its mode on
/// screen permanently, and the reason is the failure this fixes: a mode
/// announced once, by a notification that fades, is a mode you can be in
/// without knowing — which reads exactly like the feature being broken.
#[must_use]
pub fn mode_presentation(mode: &str) -> (&'static str, &'static str) {
    match mode {
        // Deliberately not a microphone: the point is that speech is *not*
        // being transcribed right now.
        "command" => ("Command mode", "system-run-symbolic"),
        "spelling" => ("Spelling mode", "format-text-underline-symbolic"),
        "asleep" => ("Asleep", "media-playback-pause-symbolic"),
        _ => ("Dictating", FALLBACK_ICON),
    }
}

pub const FALLBACK_ICON: &str = "audio-input-microphone-symbolic";

/// While listening, alternate the record dot with the idle mic outline on a
/// timer so the panel visibly blinks rather than showing a static dot.
pub const PULSE_FRAMES: &[&str] = &["media-record-symbolic", "audio-input-microphone-symbolic"];
pub const PULSE_INTERVAL_MS: u64 = 600;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mode_never_presents_as_a_microphone() {
        // The whole signal is "speech is not being transcribed right now". An
        // icon that still says microphone would defeat the indicator.
        for mode in ["command", "spelling", "asleep"] {
            let (label, icon) = mode_presentation(mode);
            assert!(!label.is_empty(), "{mode}");
            assert_ne!(icon, FALLBACK_ICON, "{mode} must look unlike dictation");
        }
    }

    #[test]
    fn an_unknown_mode_reads_as_plain_dictation() {
        // Never a blank panel: an unrecognised mode degrades to the ordinary
        // state rather than to nothing.
        assert_eq!(mode_presentation("nonsense").1, FALLBACK_ICON);
    }

    #[test]
    fn modes_and_states_do_not_share_a_presentation() {
        // They answer different questions, so a reader must be able to tell
        // "listening" from "in command mode" at a glance.
        assert_ne!(
            mode_presentation("command").1,
            state_presentation("listening").1
        );
        assert_ne!(
            mode_presentation("command").0,
            state_presentation("listening").0
        );
    }

    #[test]
    fn an_empty_frame_has_no_amplitude() {
        assert_eq!(compute_rms(&[]), 0.0);
    }

    #[test]
    fn rms_of_a_constant_signal_is_its_magnitude() {
        assert!((compute_rms(&[0.5, -0.5, 0.5, -0.5]) - 0.5).abs() < 1e-6);
        assert_eq!(compute_rms(&[0.0; 64]), 0.0);
    }

    #[test]
    fn a_quiet_room_rests_at_zero() {
        // The noise floor is the whole reason this is not a bare average: an
        // idle meter that jitters on ambient noise reads as "still listening"
        // when nothing is being said.
        let mut smoother = LevelSmoother::default();
        for _ in 0..50 {
            assert_eq!(smoother.update(0.001), 0.0);
        }
    }

    #[test]
    fn the_meter_is_clamped_to_one() {
        let mut smoother = LevelSmoother::default();
        for _ in 0..50 {
            assert!(smoother.update(10.0) <= 1.0);
        }
    }

    #[test]
    fn rising_input_is_followed_faster_than_falling() {
        // The behaviour that makes it read as a voice rather than a slider.
        let mut rising = LevelSmoother::default();
        let after_one_loud_frame = rising.update(0.5);

        let mut falling = LevelSmoother::default();
        for _ in 0..20 {
            falling.update(0.5);
        }
        let settled = falling.update(0.5);
        let after_one_quiet_frame = falling.update(0.0);

        let rise = after_one_loud_frame;
        let fall = settled - after_one_quiet_frame;
        assert!(
            rise > fall,
            "attack ({rise}) should outpace release ({fall})"
        );
    }

    #[test]
    fn resetting_returns_the_meter_to_rest() {
        let mut smoother = LevelSmoother::default();
        for _ in 0..20 {
            smoother.update(0.5);
        }
        assert!(smoother.update(0.5) > 0.0);
        smoother.reset();
        assert_eq!(smoother.update(0.0), 0.0);
    }

    #[test]
    fn silence_fires_once_past_the_timeout() {
        let mut monitor = SilenceMonitor::new(3.0);
        assert!(!monitor.observe(0.0, true), "voice seeds the window");
        assert!(!monitor.observe(2.0, false), "not yet");
        assert!(monitor.observe(3.5, false), "past the timeout");
        assert!(
            !monitor.observe(4.0, false),
            "it must fire once per silent stretch, not every frame"
        );
    }

    #[test]
    fn speech_re_arms_the_monitor() {
        // Deliberate dictation with natural pauses must never be cut off.
        let mut monitor = SilenceMonitor::new(3.0);
        monitor.observe(0.0, true);
        assert!(monitor.observe(4.0, false), "fires");
        assert!(!monitor.observe(5.0, true), "speech resumes");
        assert!(!monitor.observe(6.0, false), "the window restarted");
        assert!(monitor.observe(9.0, false), "and can fire again");
    }

    #[test]
    fn the_first_observation_seeds_the_window() {
        // Without this, a session starting in silence would compare against a
        // timestamp of zero and auto-stop immediately.
        let mut monitor = SilenceMonitor::new(3.0);
        assert!(!monitor.observe(1000.0, false));
        assert!(!monitor.observe(1002.0, false));
        assert!(monitor.observe(1004.0, false));
    }

    #[test]
    fn exactly_at_the_timeout_the_session_survives() {
        // Strictly greater, matching the reference.
        let mut monitor = SilenceMonitor::new(3.0);
        monitor.observe(0.0, false);
        assert!(!monitor.observe(3.0, false));
        assert!(monitor.observe(3.001, false));
    }

    #[test]
    fn resetting_disarms_a_fired_monitor() {
        let mut monitor = SilenceMonitor::new(3.0);
        monitor.observe(0.0, false);
        assert!(monitor.observe(4.0, false));
        monitor.reset();
        assert!(!monitor.observe(5.0, false), "the window seeds afresh");
        assert!(monitor.observe(9.0, false));
    }

    #[test]
    fn every_state_has_a_presentation_and_an_unknown_one_falls_back() {
        for state in ["idle", "listening", "transcribing", "backlogged"] {
            let (label, icon) = state_presentation(state);
            assert!(!label.is_empty());
            assert!(icon.ends_with("-symbolic"), "{icon} is not a symbolic name");
        }
        assert_eq!(state_presentation("nonsense").1, FALLBACK_ICON);
    }

    #[test]
    fn a_session_starts_once_and_stops_once() {
        let mut tracker = SessionTracker::default();
        assert_eq!(tracker.observe("idle"), None, "already at rest");
        assert_eq!(tracker.observe("listening"), Some(SessionEdge::Started));
        assert_eq!(tracker.observe("idle"), Some(SessionEdge::Stopped));
    }

    #[test]
    fn utterances_within_a_session_do_not_re_fire_the_layers() {
        // The bug this prevents: a chime on every utterance instead of once
        // per session. `process_utterance` oscillates these states.
        let mut tracker = SessionTracker::default();
        assert_eq!(tracker.observe("listening"), Some(SessionEdge::Started));
        for _ in 0..5 {
            assert_eq!(tracker.observe("transcribing"), None);
            assert_eq!(tracker.observe("listening"), None);
        }
        assert_eq!(tracker.observe("idle"), Some(SessionEdge::Stopped));
    }

    #[test]
    fn a_session_can_start_in_any_non_idle_state() {
        // Push-to-talk can go straight to "transcribing" on a short press.
        let mut tracker = SessionTracker::default();
        assert_eq!(tracker.observe("transcribing"), Some(SessionEdge::Started));
        assert!(tracker.is_active());
    }

    #[test]
    fn shutdown_closes_a_running_session_exactly_once() {
        // Or the overlay would be left on screen after the daemon exits.
        let mut tracker = SessionTracker::default();
        tracker.observe("listening");
        assert_eq!(tracker.finish(), Some(SessionEdge::Stopped));
        assert_eq!(tracker.finish(), None);
    }

    #[test]
    fn shutdown_with_no_session_running_does_nothing() {
        assert_eq!(SessionTracker::default().finish(), None);
    }

    #[test]
    fn the_pulse_alternates_between_two_frames() {
        assert_eq!(PULSE_FRAMES.len(), 2);
        assert_eq!(PULSE_FRAMES[0], state_presentation("listening").1);
        assert_eq!(PULSE_FRAMES[1], FALLBACK_ICON);
    }
}
