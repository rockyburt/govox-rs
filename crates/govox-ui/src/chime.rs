//! Short synthesized start/stop/tick cues.
//!
//! Modelled on **macOS Dictation**, not on `govox-py`. The reference plays a
//! linear frequency sweep — a 180 ms glide from 440 Hz to 880 Hz — which reads
//! as a slide whistle. macOS instead plays two *discrete* pitched notes with a
//! bell-like envelope: a fast attack and an exponential decay, ascending to
//! start and descending to stop. It is shorter, quieter and far easier to stop
//! hearing, which matters for a cue that fires every time you dictate.
//!
//! Recorded in `docs/parity.md` as a deliberate divergence.
//!
//! Buffers are built with `std` alone, so the *shape* of every cue is testable
//! without an audio device — which matters because the envelope is the whole
//! difference between a cue and a click.

use std::sync::atomic::{AtomicBool, Ordering};

/// Note pair for the start cue: a rising perfect fifth in the bright register
/// where a short tone stays audible over room noise without being shrill.
const START_NOTES: [f32; 2] = [880.00, 1318.51]; // A5 → E6

/// The stop cue is the same interval, descending. Deliberately the exact
/// inverse: the pair has to be identifiable without looking at the screen, and
/// direction is the one property the ear reads instantly.
const STOP_NOTES: [f32; 2] = [1318.51, 880.00]; // E6 → A5

/// A single mid note for "still listening".
const TICK_NOTE: f32 = 1046.50; // C6

/// Each note in a cue. Two of these back to back is ~110 ms total — shorter
/// than the reference's single 180 ms sweep.
const NOTE_S: f32 = 0.055;
const NOTE_AMPLITUDE: f32 = 0.32;
/// The tick fires repeatedly during a long session, so it is markedly quieter.
const TICK_AMPLITUDE: f32 = 0.10;
const TICK_S: f32 = 0.045;

/// Plays a mono f32 buffer. Should return promptly; the real sink plays
/// asynchronously.
pub trait PlaySink: Send + Sync {
    /// # Errors
    /// If the device is unavailable. The caller logs once and carries on.
    fn play(&self, samples: &[f32], sample_rate: u32) -> Result<(), String>;
}

pub struct Chime<S: PlaySink> {
    pub sample_rate: u32,
    sink: S,
    /// Warn once, not once per cue: a missing sound device would otherwise
    /// produce a line of log per utterance forever.
    warned: AtomicBool,
}

impl<S: PlaySink> Chime<S> {
    #[must_use]
    pub fn new(sink: S, sample_rate: u32) -> Self {
        Self {
            sample_rate,
            sink,
            warned: AtomicBool::new(false),
        }
    }

    /// Rising two-note cue marking session start.
    pub fn start(&self) {
        self.play(&phrase(
            self.sample_rate,
            &START_NOTES,
            NOTE_S,
            NOTE_AMPLITUDE,
        ));
    }

    /// Falling two-note cue marking session stop.
    pub fn stop(&self) {
        self.play(&phrase(
            self.sample_rate,
            &STOP_NOTES,
            NOTE_S,
            NOTE_AMPLITUDE,
        ));
    }

    /// Single quiet "still listening" note.
    pub fn tick(&self) {
        self.play(&phrase(
            self.sample_rate,
            &[TICK_NOTE],
            TICK_S,
            TICK_AMPLITUDE,
        ));
    }

    fn play(&self, samples: &[f32]) {
        // A playback failure must never propagate: losing an audio cue is not
        // a reason to interrupt dictation.
        if let Err(error) = self.sink.play(samples, self.sample_rate)
            && !self.warned.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(%error, "chime playback failed; continuing without audio cues");
        }
    }
}

/// One struck note: a sine with a fast attack and an exponential decay.
///
/// The attack is a 2 ms ramp rather than an instant onset — starting a sine at
/// full amplitude is a step discontinuity, which is heard as a click in front
/// of the note. The decay is exponential rather than linear because that is
/// what a struck or plucked body does, and it is the difference between a bell
/// and a beep.
#[must_use]
pub fn note(sample_rate: u32, freq: f32, duration_s: f32, amplitude: f32) -> Vec<f32> {
    let count = ((sample_rate as f32 * duration_s).round() as usize).max(1);
    let attack = ((sample_rate as f32 * 0.002).round() as usize)
        .max(1)
        .min(count);
    // Chosen so the note is ~1% of peak by its end: any audible tail would
    // overlap the next note and muddy the interval.
    let decay_rate = 5.0_f32;

    (0..count)
        .map(|index| {
            let t = index as f32 / sample_rate as f32;
            let progress = index as f32 / count as f32;
            let attack_gain = if index < attack {
                index as f32 / attack as f32
            } else {
                1.0
            };
            let envelope = attack_gain * (-decay_rate * progress).exp();
            amplitude * envelope * (2.0 * std::f32::consts::PI * freq * t).sin()
        })
        .collect()
}

/// Several notes played back to back.
#[must_use]
pub fn phrase(sample_rate: u32, freqs: &[f32], note_s: f32, amplitude: f32) -> Vec<f32> {
    let mut buffer = Vec::new();
    for freq in freqs {
        buffer.extend(note(sample_rate, *freq, note_s, amplitude));
    }
    buffer
}

/// Plays through the default output device.
///
/// The device sink is opened **once** and held. Opening one per cue would put a
/// device-open on the session edge, which is exactly where the latency is most
/// noticeable — and on PipeWire it is tens of milliseconds.
pub struct RodioSink {
    // Dropping this ends playback and disposes the OS sink.
    device: rodio::stream::MixerDeviceSink,
}

impl RodioSink {
    /// Open the default output device.
    ///
    /// # Errors
    /// If no output device is available — an ordinary outcome on a headless
    /// session, not a failure worth stopping for.
    pub fn open() -> Result<Self, String> {
        let mut device = rodio::stream::DeviceSinkBuilder::open_default_sink()
            .map_err(|e| format!("no audio output: {e}"))?;
        // Its drop message is not something the user needs to read on Ctrl-C.
        device.log_on_drop(false);
        Ok(Self { device })
    }
}

impl PlaySink for RodioSink {
    fn play(&self, samples: &[f32], sample_rate: u32) -> Result<(), String> {
        let channels = std::num::NonZero::new(1).expect("1 is non-zero");
        let rate = std::num::NonZero::new(sample_rate).ok_or("sample rate is zero")?;
        // Mixed in rather than played synchronously: `start()` is called on the
        // session edge and must not delay the first frame of capture.
        self.device.mixer().add(rodio::buffer::SamplesBuffer::new(
            channels,
            rate,
            samples.to_vec(),
        ));
        Ok(())
    }
}

/// Drops every cue. Used when there is no output device.
pub struct SilentSink;

impl PlaySink for SilentSink {
    fn play(&self, _samples: &[f32], _sample_rate: u32) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingSink {
        played: Mutex<Vec<Vec<f32>>>,
        fail: bool,
    }

    impl PlaySink for &RecordingSink {
        fn play(&self, samples: &[f32], _sample_rate: u32) -> Result<(), String> {
            self.played.lock().unwrap().push(samples.to_vec());
            if self.fail {
                return Err("no device".to_owned());
            }
            Ok(())
        }
    }

    fn peak(buffer: &[f32]) -> f32 {
        buffer.iter().fold(0.0_f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn a_note_starts_at_silence() {
        // The attack ramp. Starting a sine at full amplitude is a step
        // discontinuity, heard as a click in front of the note.
        let buffer = note(44_100, 880.0, 0.055, 0.32);
        assert_eq!(buffer[0], 0.0);
    }

    #[test]
    fn a_note_decays_to_near_silence() {
        // Any audible tail overlaps the next note and muddies the interval.
        let buffer = note(44_100, 880.0, 0.055, 0.32);
        let tail = &buffer[buffer.len() * 9 / 10..];
        assert!(
            peak(tail) < 0.32 * 0.05,
            "the tail is still at {} of full scale",
            peak(tail) / 0.32
        );
    }

    #[test]
    fn a_note_decays_rather_than_fading_linearly() {
        // A struck body's envelope, which is what separates a bell from a beep.
        let buffer = note(44_100, 880.0, 0.055, 0.32);
        let quarter = buffer.len() / 4;
        let first = peak(&buffer[..quarter]);
        let second = peak(&buffer[quarter..quarter * 2]);
        let third = peak(&buffer[quarter * 2..quarter * 3]);

        // Exponential: each quarter drops by a larger *absolute* amount early
        // on than late, so the first drop exceeds the second.
        assert!(
            first > second && second > third,
            "not monotonically decaying"
        );
        assert!(
            (first - second) > (second - third),
            "the decay is linear, not exponential"
        );
    }

    #[test]
    fn a_note_never_exceeds_its_amplitude() {
        for sample in note(44_100, 1318.51, 0.055, 0.32) {
            assert!(sample.abs() <= 0.32 + 1e-6, "clipped at {sample}");
        }
    }

    #[test]
    fn the_start_cue_rises_and_the_stop_cue_falls() {
        // Direction is the one property the ear reads instantly, so start and
        // stop must be exact inverses of each other.
        const { assert!(START_NOTES[1] > START_NOTES[0], "start must ascend") };
        const { assert!(STOP_NOTES[1] < STOP_NOTES[0], "stop must descend") };
        assert_eq!(START_NOTES[0], STOP_NOTES[1]);
        assert_eq!(START_NOTES[1], STOP_NOTES[0]);
    }

    #[test]
    fn the_interval_is_a_perfect_fifth() {
        // Chosen rather than arbitrary: a consonant interval reads as a cue,
        // a dissonant one reads as an alert.
        let ratio = START_NOTES[1] / START_NOTES[0];
        assert!(
            (ratio - 1.5).abs() < 0.01,
            "expected a 3:2 ratio, got {ratio}"
        );
    }

    #[test]
    fn a_cue_is_short_enough_not_to_delay_speaking() {
        // Two notes back to back, ~110 ms — shorter than govox-py's single
        // 180 ms sweep. The user starts talking almost immediately.
        let buffer = phrase(44_100, &START_NOTES, NOTE_S, NOTE_AMPLITUDE);
        let seconds = buffer.len() as f32 / 44_100.0;
        assert!(seconds < 0.15, "the start cue lasts {seconds}s");
    }

    #[test]
    fn a_phrase_is_its_notes_end_to_end() {
        let one = note(44_100, 880.0, 0.055, 0.32);
        let two = phrase(44_100, &[880.0, 1318.51], 0.055, 0.32);
        assert_eq!(two.len(), one.len() * 2);
        assert_eq!(&two[..one.len()], one.as_slice());
    }

    #[test]
    fn the_tick_is_quieter_and_shorter_than_the_session_cues() {
        // It fires repeatedly during a long session; at start volume it would
        // be intolerable.
        let tick = phrase(44_100, &[TICK_NOTE], TICK_S, TICK_AMPLITUDE);
        let start = phrase(44_100, &START_NOTES, NOTE_S, NOTE_AMPLITUDE);
        assert!(tick.len() < start.len());
        assert!(peak(&tick) < peak(&start) / 2.0);
    }

    #[test]
    fn start_and_stop_are_distinguishable() {
        let sink = RecordingSink::default();
        let chime = Chime::new(&sink, 44_100);
        chime.start();
        chime.stop();

        let played = sink.played.lock().unwrap();
        assert_eq!(played.len(), 2);
        assert_ne!(played[0], played[1], "start and stop must not sound alike");
        assert_eq!(
            played[0].len(),
            played[1].len(),
            "same shape, opposite order"
        );
    }

    #[test]
    fn a_dead_sound_device_warns_once_and_never_propagates() {
        let sink = RecordingSink {
            played: Mutex::new(Vec::new()),
            fail: true,
        };
        let chime = Chime::new(&sink, 44_100);
        // Must not panic: losing an audio cue is not a reason to interrupt
        // dictation, and a per-utterance log line is its own problem.
        for _ in 0..10 {
            chime.start();
        }
        assert_eq!(sink.played.lock().unwrap().len(), 10);
    }

    /// Plays the real cues through the real device, so they can be judged by
    /// ear rather than by waveform assertions.
    ///
    /// `cargo test -p govox-ui --lib -- --ignored plays_the_cues_aloud --nocapture`
    #[test]
    #[ignore = "makes noise on the default output device"]
    fn plays_the_cues_aloud() {
        let Ok(sink) = RodioSink::open() else {
            eprintln!("no audio output; nothing to hear");
            return;
        };
        let chime = Chime::new(sink, 44_100);

        eprintln!("start (ascending)");
        chime.start();
        std::thread::sleep(std::time::Duration::from_millis(900));

        eprintln!("tick (quiet, single note)");
        chime.tick();
        std::thread::sleep(std::time::Duration::from_millis(900));

        eprintln!("stop (descending)");
        chime.stop();
        std::thread::sleep(std::time::Duration::from_millis(900));
    }

    #[test]
    fn a_zero_length_note_is_still_a_valid_buffer() {
        assert_eq!(note(44_100, 880.0, 0.0, 0.32).len(), 1);
    }
}
