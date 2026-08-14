//! Cutting a continuous microphone stream into utterances.
//!
//! The segmenter is a pure state machine: it is handed a speech *probability*
//! per frame and never computes one. That split is what keeps the interesting
//! logic — the hysteresis, the hangover, the minimum-length rejection —
//! testable without ONNX, a model file, or a microphone. `govox-vad` supplies
//! the probabilities in production; the tests here supply a canned sequence.

use std::sync::Arc;

use crate::domain::{AudioBuffer, AudioFrame, Utterance};

/// Turns speech probabilities into utterance boundaries.
///
/// Two thresholds rather than one, which is the whole design: a frame above
/// `speech_threshold` extends the phrase, a frame below `silence_threshold`
/// advances the hangover timer, and a frame *between* them does neither. That
/// dead band is what stops a trailing-off word from being chopped mid-syllable.
#[derive(Debug)]
pub struct VadSegmenter {
    pub speech_threshold: f64,
    pub silence_threshold: f64,
    pub min_speech_ms: f64,
    pub hangover_ms: f64,
    speech_frames: Vec<AudioFrame>,
    speech_ms: f64,
    silence_ms: f64,
}

impl VadSegmenter {
    #[must_use]
    pub fn new(
        speech_threshold: f64,
        silence_threshold: f64,
        min_speech_ms: f64,
        hangover_ms: f64,
    ) -> Self {
        Self {
            speech_threshold,
            silence_threshold,
            min_speech_ms,
            hangover_ms,
            speech_frames: Vec::new(),
            speech_ms: 0.0,
            silence_ms: 0.0,
        }
    }

    #[must_use]
    pub fn from_config(config: &crate::config::VadConfig) -> Self {
        Self::new(
            config.speech_threshold,
            config.silence_threshold,
            f64::from(config.min_speech_ms),
            f64::from(config.hangover_ms),
        )
    }

    /// Whether speech frames are currently buffered (a phrase is in progress).
    ///
    /// Used by the silence monitor: while this is true the speaker is
    /// mid-phrase and the silence auto-stop timer must not advance.
    #[must_use]
    pub fn in_speech(&self) -> bool {
        !self.speech_frames.is_empty()
    }

    /// Feed one frame. Returns an utterance when the phrase has ended.
    pub fn process(&mut self, frame: &AudioFrame, probability: f64) -> Option<Utterance> {
        let duration_ms = frame_duration_ms(frame);

        if probability >= self.speech_threshold {
            self.speech_frames.push(frame.clone());
            self.speech_ms += duration_ms;
            self.silence_ms = 0.0;
            return None;
        }

        // Silence before any speech is just room tone; nothing to close.
        if self.speech_frames.is_empty() {
            return None;
        }

        if probability <= self.silence_threshold {
            self.silence_ms += duration_ms;
        }

        // Deliberately outside the branch above, matching govox-py: a frame in
        // the dead band does not advance the timer but does re-check it, so a
        // phrase already past its hangover closes on the next frame whatever
        // that frame's probability is.
        if self.silence_ms < self.hangover_ms {
            return None;
        }

        if self.speech_ms < self.min_speech_ms {
            // Too short to be speech — a cough, a keystroke, a door. Drop it
            // rather than send it to the recogniser, which would hallucinate.
            self.reset();
            return None;
        }

        let utterance = self.build_utterance();
        self.reset();
        utterance
    }

    /// Force-emit any buffered speech without waiting for the hangover.
    ///
    /// Used on push-to-talk key release so the utterance dispatches
    /// immediately rather than after `hangover_ms` of silence.
    pub fn flush(&mut self) -> Option<Utterance> {
        if self.speech_ms < self.min_speech_ms {
            self.reset();
            return None;
        }
        let utterance = self.build_utterance();
        self.reset();
        utterance
    }

    pub fn reset(&mut self) {
        self.speech_frames.clear();
        self.speech_ms = 0.0;
        self.silence_ms = 0.0;
    }

    /// `None` only when nothing is buffered, which the callers already exclude.
    fn build_utterance(&self) -> Option<Utterance> {
        let first = self.speech_frames.first()?;
        let last = self.speech_frames.last()?;

        let total: usize = self.speech_frames.iter().map(|f| f.samples.len()).sum();
        let mut samples = Vec::with_capacity(total);
        for frame in &self.speech_frames {
            samples.extend_from_slice(&frame.samples);
        }

        Some(Utterance {
            audio: AudioBuffer {
                samples: Arc::from(samples),
                sample_rate: first.sample_rate,
                start_ts: first.timestamp,
                end_ts: last.timestamp,
            },
            speech_end_ts: last.timestamp,
        })
    }
}

#[must_use]
pub fn frame_duration_ms(frame: &AudioFrame) -> f64 {
    if frame.sample_rate == 0 {
        return 0.0;
    }
    frame.samples.len() as f64 / f64::from(frame.sample_rate) * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 30 ms frame at 16 kHz: 480 samples, the real capture size.
    fn frame(timestamp: f64) -> AudioFrame {
        AudioFrame {
            samples: Arc::from(vec![0.1_f32; 480]),
            sample_rate: 16_000,
            timestamp,
        }
    }

    fn segmenter() -> VadSegmenter {
        // The shipped defaults.
        VadSegmenter::new(0.5, 0.35, 180.0, 400.0)
    }

    /// Feed `(probability, ...)` as consecutive 30 ms frames.
    fn run(seg: &mut VadSegmenter, probabilities: &[f64]) -> Vec<Utterance> {
        probabilities
            .iter()
            .enumerate()
            .filter_map(|(i, p)| seg.process(&frame(i as f64 * 0.03), *p))
            .collect()
    }

    #[test]
    fn frame_duration_is_thirty_milliseconds() {
        assert!((frame_duration_ms(&frame(0.0)) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn silence_alone_emits_nothing() {
        let mut seg = segmenter();
        assert!(run(&mut seg, &[0.0; 40]).is_empty());
        assert!(!seg.in_speech());
    }

    #[test]
    fn a_phrase_closes_after_the_hangover() {
        let mut seg = segmenter();
        // 10 frames = 300 ms of speech, comfortably over min_speech_ms.
        let mut probabilities = vec![0.9; 10];
        // 400 ms of hangover is 14 frames (13 * 30 = 390, short of it).
        probabilities.extend([0.0; 14]);

        let utterances = run(&mut seg, &probabilities);

        assert_eq!(utterances.len(), 1);
        // Only the speech frames are kept; the silence is discarded.
        assert_eq!(utterances[0].audio.samples.len(), 10 * 480);
        assert!(!seg.in_speech(), "the segmenter resets after emitting");
    }

    #[test]
    fn the_hangover_is_not_reached_early() {
        let mut seg = segmenter();
        let mut probabilities = vec![0.9; 10];
        probabilities.extend([0.0; 13]); // 390 ms, just short of 400.
        assert!(run(&mut seg, &probabilities).is_empty());
        assert!(seg.in_speech(), "the phrase is still open");
    }

    #[test]
    fn a_short_burst_is_dropped_rather_than_recognised() {
        let mut seg = segmenter();
        // 5 frames = 150 ms, under the 180 ms floor. A cough, not a word.
        let mut probabilities = vec![0.9; 5];
        probabilities.extend([0.0; 14]);

        assert!(run(&mut seg, &probabilities).is_empty());
        assert!(!seg.in_speech(), "the buffer is cleared, not left dangling");
    }

    #[test]
    fn speech_resets_the_hangover_timer() {
        let mut seg = segmenter();
        let mut probabilities = vec![0.9; 10];
        probabilities.extend([0.0; 10]); // 300 ms of silence, then...
        probabilities.push(0.9); // ...one more word.
        probabilities.extend([0.0; 10]); // 300 ms again — still not enough.

        assert!(run(&mut seg, &probabilities).is_empty());
        assert!(seg.in_speech());
    }

    #[test]
    fn the_dead_band_neither_extends_speech_nor_advances_silence() {
        let mut seg = segmenter();
        // 0.4 sits between silence_threshold (0.35) and speech_threshold (0.5).
        let mut probabilities = vec![0.9; 10];
        probabilities.extend([0.4; 100]);

        assert!(
            run(&mut seg, &probabilities).is_empty(),
            "a hundred dead-band frames must never close the phrase on their own"
        );
        assert!(seg.in_speech());
    }

    #[test]
    fn a_dead_band_frame_can_close_a_phrase_already_past_its_hangover() {
        // The load-bearing consequence of govox-py checking the hangover
        // outside the silence branch. Worth pinning: the obvious refactor —
        // folding the check into the `probability <= silence_threshold` arm —
        // changes behaviour here and nowhere else.
        let mut seg = segmenter();
        let mut probabilities = vec![0.9; 10];
        probabilities.extend([0.0; 14]); // reaches the hangover exactly...

        let emitted = run(&mut seg, &probabilities);
        assert_eq!(emitted.len(), 1);

        // ...and again, but with the final frame in the dead band.
        let mut seg = segmenter();
        let mut probabilities = vec![0.9; 10];
        probabilities.extend([0.0; 13]); // 390 ms: one frame short.
        probabilities.push(0.4); // dead band: adds no silence, but re-checks.
        assert!(
            run(&mut seg, &probabilities).is_empty(),
            "390 ms is still under the hangover"
        );

        let mut seg = segmenter();
        let mut probabilities = vec![0.9; 10];
        probabilities.extend([0.0; 14]); // 420 ms: past it.
        probabilities.push(0.4);
        assert_eq!(run(&mut seg, &probabilities).len(), 1);
    }

    #[test]
    fn flush_emits_without_waiting_for_silence() {
        let mut seg = segmenter();
        run(&mut seg, &[0.9; 10]);
        assert!(seg.in_speech());

        let utterance = seg.flush().expect("300 ms clears the floor");
        assert_eq!(utterance.audio.samples.len(), 10 * 480);
        assert!(!seg.in_speech());
    }

    #[test]
    fn flush_on_a_short_burst_drops_it() {
        let mut seg = segmenter();
        run(&mut seg, &[0.9; 3]); // 90 ms.
        assert!(seg.flush().is_none());
        assert!(!seg.in_speech());
    }

    #[test]
    fn flush_on_an_empty_buffer_is_harmless() {
        assert!(segmenter().flush().is_none());
    }

    #[test]
    fn timestamps_span_the_speech_frames_only() {
        let mut seg = segmenter();
        let mut probabilities = vec![0.9; 10];
        probabilities.extend([0.0; 14]);

        let utterance = run(&mut seg, &probabilities).remove(0);

        // Frames are 30 ms apart starting at 0.0, so speech runs 0.00..0.27.
        assert!((utterance.audio.start_ts - 0.0).abs() < 1e-9);
        assert!((utterance.audio.end_ts - 0.27).abs() < 1e-9);
        // speech_end_ts is the last *speech* frame, not the last silent one:
        // it is what the streaming layer trims against.
        assert!((utterance.speech_end_ts - 0.27).abs() < 1e-9);
    }
}
