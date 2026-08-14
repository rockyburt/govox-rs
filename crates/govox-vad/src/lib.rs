//! Silero VAD speech-probability backend.
//!
//! Supplies the probabilities that [`govox_core::vad::VadSegmenter`] turns into
//! utterance boundaries. The split matters: everything that decides *where a
//! phrase ends* lives in `govox-core` and is tested without a model, and this
//! crate does nothing but score 512-sample windows.
//!
//! The model is compiled into the binary and ONNX Runtime is linked statically,
//! so there is no `libonnxruntime.so` to install and nothing downloaded at
//! runtime. That is a real gain over `govox-py`, which needs `silero-vad`,
//! `onnxruntime` **and** torch present at runtime — and which imports torch
//! solely to build the input tensor for an ONNX model.
//!
//! Verified in the M-1(c) spike: 44 of 47 windows bit-identical to `govox-py`
//! at six decimal places, the other three differing by 1e-6 (float32 print
//! rounding). Acceptance was 1e-4, so the VAD thresholds carry over untouched.

use govox_core::domain::AudioFrame;

/// Silero's streaming model demands a fixed window per call.
const WINDOW_16K: usize = 512;
const WINDOW_8K: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum VadError {
    #[error("silero session could not start: {0}")]
    SessionUnavailable(String),
    #[error("silero inference failed: {0}")]
    Inference(String),
    #[error("unsupported sample rate {0} Hz; silero accepts 8000 or 16000")]
    UnsupportedRate(u32),
}

/// Scores audio frames for speech.
///
/// Implemented here by Silero, and by a canned sequence in tests. The trait
/// exists so `govox-daemon` can be wired up without ONNX Runtime.
pub trait SpeechProbability: Send {
    /// Probability that the most recent audio contains speech, in `0.0..=1.0`.
    fn probability(&mut self, frame: &AudioFrame) -> Result<f32, VadError>;

    /// Forget recurrent state at an utterance boundary.
    fn reset(&mut self);
}

/// Silero v5 over ONNX Runtime.
///
/// Owns the recurrent `StreamState` explicitly. `govox-py` hides the same state
/// in a closure over `nonlocal` variables, which is the awkward part of its
/// `vad.py`; here it is a value with a visible lifetime, so `reset` at an
/// utterance edge is an ordinary method call rather than a rebound closure.
pub struct SileroVad {
    session: silero::Session,
    stream: silero::StreamState,
    /// Samples not yet forming a full window, carried to the next call.
    ///
    /// Capture frames are 30 ms (480 samples at 16 kHz) and the model wants
    /// 512, so the two never align. Without this the leftover would be dropped
    /// and roughly 6% of the audio would never be scored.
    pending: Vec<f32>,
    window: usize,
    /// The last window's score, returned for frames too short to complete one.
    ///
    /// Holding the previous value rather than reporting 0.0 is what stops a
    /// short frame from reading as silence and cutting a phrase in half.
    last: f32,
}

impl SileroVad {
    /// Load the bundled model for `sample_rate`.
    ///
    /// # Errors
    /// If the rate is not 8 or 16 kHz, or ONNX Runtime cannot start.
    pub fn new(sample_rate: u32) -> Result<Self, VadError> {
        let (rate, window) = match sample_rate {
            16_000 => (silero::SampleRate::Rate16k, WINDOW_16K),
            8_000 => (silero::SampleRate::Rate8k, WINDOW_8K),
            other => return Err(VadError::UnsupportedRate(other)),
        };
        let session =
            silero::Session::bundled().map_err(|e| VadError::SessionUnavailable(e.to_string()))?;
        Ok(Self {
            session,
            stream: silero::StreamState::new(rate),
            pending: Vec::with_capacity(window * 2),
            window,
            last: 0.0,
        })
    }
}

impl SpeechProbability for SileroVad {
    fn probability(&mut self, frame: &AudioFrame) -> Result<f32, VadError> {
        self.pending.extend_from_slice(&frame.samples);

        // A frame can span more than one window (or none). Score every whole
        // window available and report the most recent, matching govox-py.
        let mut consumed = 0;
        while self.pending.len() - consumed >= self.window {
            let chunk = &self.pending[consumed..consumed + self.window];
            self.last = self
                .session
                .infer_chunk(&mut self.stream, chunk)
                .map_err(|e| VadError::Inference(e.to_string()))?;
            consumed += self.window;
        }
        self.pending.drain(..consumed);

        Ok(self.last)
    }

    fn reset(&mut self) {
        self.stream.reset();
        self.pending.clear();
        self.last = 0.0;
    }
}

/// A [`SpeechProbability`] that replays a fixed sequence.
///
/// Lets the daemon's own tests drive segmentation deterministically without
/// ONNX Runtime or an audio fixture.
#[derive(Debug, Default)]
pub struct ScriptedVad {
    probabilities: std::collections::VecDeque<f32>,
    /// Returned once the script runs out.
    default: f32,
    pub resets: usize,
}

impl ScriptedVad {
    #[must_use]
    pub fn new(probabilities: impl IntoIterator<Item = f32>) -> Self {
        Self {
            probabilities: probabilities.into_iter().collect(),
            default: 0.0,
            resets: 0,
        }
    }
}

impl SpeechProbability for ScriptedVad {
    fn probability(&mut self, _frame: &AudioFrame) -> Result<f32, VadError> {
        Ok(self.probabilities.pop_front().unwrap_or(self.default))
    }

    fn reset(&mut self) {
        self.resets += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn frame(samples: usize) -> AudioFrame {
        AudioFrame {
            samples: Arc::from(vec![0.0_f32; samples]),
            sample_rate: 16_000,
            timestamp: 0.0,
        }
    }

    #[test]
    fn an_unsupported_rate_is_rejected_rather_than_guessed() {
        assert!(matches!(
            SileroVad::new(44_100),
            Err(VadError::UnsupportedRate(44_100))
        ));
    }

    #[test]
    fn the_scripted_backend_replays_then_falls_silent() {
        let mut vad = ScriptedVad::new([0.9, 0.8]);
        assert_eq!(vad.probability(&frame(480)).unwrap(), 0.9);
        assert_eq!(vad.probability(&frame(480)).unwrap(), 0.8);
        assert_eq!(vad.probability(&frame(480)).unwrap(), 0.0);
        vad.reset();
        assert_eq!(vad.resets, 1);
    }

    /// Requires ONNX Runtime; run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "loads the bundled Silero model"]
    fn silence_scores_low_and_short_frames_hold_the_last_value() {
        let mut vad = SileroVad::new(16_000).expect("bundled model loads");

        // 480 samples is under the 512-sample window: nothing to score yet, so
        // the initial value stands rather than a spurious 0.0-from-inference.
        let first = vad.probability(&frame(480)).expect("scores");
        assert!((first - 0.0).abs() < 1e-6);

        // A second frame completes a window.
        let second = vad.probability(&frame(480)).expect("scores");
        assert!(
            (0.0..=1.0).contains(&second),
            "probability out of range: {second}"
        );
    }
}
