//! Turning a growing audio buffer into a growing caption.
//!
//! Holds the audio window and feeds it to the model on each chunk; the
//! *decision* about which words are final lives in
//! [`govox_core::streaming`], which is pure and tested without a model.

use std::collections::VecDeque;

use govox_core::config::{BufferTrimming, StreamingConfig};
use govox_core::streaming::{HypothesisBuffer, TimedWord, join_words, trim_point};

use crate::whisper::{AsrError, WhisperHandle};

/// What one chunk produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamingUpdate {
    /// Words made final by this chunk. Safe to inject.
    pub committed: String,
    /// The provisional tail, for display only.
    pub pending: String,
}

impl StreamingUpdate {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.committed.is_empty() && self.pending.is_empty()
    }

    /// Everything to show right now, final and provisional together.
    #[must_use]
    pub fn caption(&self) -> String {
        format!("{}{}", self.committed, self.pending)
    }
}

/// Feeds a growing window to Whisper and commits what two passes agree on.
pub struct OnlineProcessor {
    asr: WhisperHandle,
    hypotheses: HypothesisBuffer,
    /// A ring rather than a `Vec`: `govox-py` uses `np.append`, which copies
    /// the whole buffer on every chunk — O(n) per 500 ms for the length of the
    /// session.
    audio: VecDeque<f32>,
    sample_rate: u32,
    /// Seconds of audio already dropped off the front, so word timestamps stay
    /// absolute across trims.
    offset_s: f64,
    min_chunk_s: f64,
    buffer_limit_s: f64,
    trimming: BufferTrimming,
}

impl OnlineProcessor {
    #[must_use]
    pub fn new(asr: WhisperHandle, config: &StreamingConfig, sample_rate: u32) -> Self {
        Self {
            asr,
            hypotheses: HypothesisBuffer::new(),
            audio: VecDeque::new(),
            sample_rate,
            offset_s: 0.0,
            min_chunk_s: config.min_chunk_size_s,
            buffer_limit_s: config.buffer_trimming_sec,
            trimming: config.buffer_trimming,
        }
    }

    /// Add captured audio. Cheap; no decoding happens here.
    pub fn push(&mut self, samples: &[f32]) {
        self.audio.extend(samples.iter().copied());
    }

    fn buffered_s(&self) -> f64 {
        self.audio.len() as f64 / f64::from(self.sample_rate.max(1))
    }

    /// Whether there is enough new audio to be worth a decode.
    ///
    /// Decoding more often than this costs GPU time without producing more
    /// agreement: a word needs two passes to commit either way.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.buffered_s() >= self.min_chunk_s
    }

    /// Decode the current window and commit what agrees.
    ///
    /// # Errors
    /// If the model fails.
    pub async fn process(&mut self) -> Result<StreamingUpdate, AsrError> {
        if self.audio.is_empty() {
            return Ok(StreamingUpdate::default());
        }
        let window: Vec<f32> = self.audio.iter().copied().collect();
        let words = self.asr.transcribe_words(&window).await?;

        // The model sees only the window, so its timestamps start at zero;
        // shift them so everything downstream works in session time.
        let shifted: Vec<TimedWord> = words
            .into_iter()
            .map(|w| TimedWord::new(w.start + self.offset_s, w.end + self.offset_s, w.text))
            .collect();

        self.hypotheses.insert(shifted);
        let committed = self.hypotheses.flush();
        let update = StreamingUpdate {
            committed: join_words(&committed),
            pending: join_words(self.hypotheses.incomplete()),
        };

        self.maybe_trim();
        Ok(update)
    }

    /// Drop settled audio off the front once the window grows too long.
    ///
    /// Unbounded growth is not just memory: Whisper re-decodes the *whole*
    /// window every chunk, so a 60-second buffer makes each update take four
    /// times as long as a 15-second one.
    fn maybe_trim(&mut self) {
        // `sentence` trimming needs a sentence tokenizer for the target
        // language, which is a dependency for a bounded gain over trimming at
        // a word boundary. Recorded in docs/parity.md rather than silently
        // treated as equivalent.
        if self.trimming == BufferTrimming::Sentence {
            tracing::debug!("sentence trimming is not implemented; trimming at a word boundary");
        }

        let buffered_s = self.buffered_s();
        let cut_s = match trim_point(
            self.hypotheses.committed_words(),
            buffered_s,
            self.buffer_limit_s,
        ) {
            Some(cut_s) => cut_s,
            // No settled word to cut at, so fall back to the backstop; see
            // `forced_trim_point` for why an uncommitted window still has to
            // be bounded.
            None => match govox_core::streaming::forced_trim_point(
                buffered_s,
                self.buffer_limit_s,
                self.offset_s,
            ) {
                Some(cut_s) => {
                    tracing::warn!(
                        buffered_s,
                        limit_s = self.buffer_limit_s,
                        "streaming buffer is over its limit with nothing committed; trimming blind"
                    );
                    cut_s
                }
                None => return,
            },
        };

        let drop_s = cut_s - self.offset_s;
        if drop_s <= 0.0 {
            return;
        }
        let drop_samples = (drop_s * f64::from(self.sample_rate)) as usize;
        let drop_samples = drop_samples.min(self.audio.len());
        self.audio.drain(..drop_samples);
        self.offset_s += drop_samples as f64 / f64::from(self.sample_rate.max(1));
        self.hypotheses.pop_committed(self.offset_s);
    }

    /// Discard all but the last `keep_s` seconds of buffered audio.
    ///
    /// For dropping the silence a session accumulates before the user starts
    /// speaking. Gating the decode on the VAD is not enough on its own: at the
    /// moment speech is first detected the buffer already holds a second or
    /// more of room tone and only a frame of voice, so the first decode still
    /// sees a window that is almost entirely silence and still fills it with
    /// the phrase Whisper reaches for when there is nothing to hear.
    ///
    /// A short pre-roll is kept rather than clearing outright, because the VAD
    /// recognises speech a little after it starts and cutting to the exact
    /// frame would clip the first consonant.
    ///
    /// Only meaningful before anything has been committed — it moves the
    /// buffer's origin, so committed word times would no longer line up.
    pub fn keep_only_last(&mut self, keep_s: f64) {
        let drop_samples =
            govox_core::streaming::samples_to_drop(self.audio.len(), self.sample_rate, keep_s);
        if drop_samples == 0 {
            return;
        }
        self.audio.drain(..drop_samples);
        self.offset_s += drop_samples as f64 / f64::from(self.sample_rate.max(1));
    }

    /// Drop everything and start a fresh session.
    ///
    /// The audio buffer is cleared as well as the hypotheses: it holds the
    /// context that produced the discarded words, and leaving it would let
    /// them bleed back into the next session's transcript.
    pub fn reset(&mut self) {
        self.hypotheses = HypothesisBuffer::new();
        self.audio.clear();
        self.offset_s = 0.0;
    }

    /// Everything still uncommitted, for the final commit at session end.
    /// # Errors
    /// Never — a failed final decode degrades to the words already in hand.
    pub async fn finish(&mut self, decode_tail: bool) -> String {
        // Decode what has not been decoded yet. Between two decodes there is
        // always up to `min_chunk_size_s` of audio the model has never seen,
        // and at the end of a session that audio is the last thing the user
        // said. Returning only the standing hypothesis discards it, so a
        // session ends by truncating its own final word.
        //
        // `decode_tail` is the caller's answer to "is there speech in there?".
        // Usually there is not: a session ends a moment after the last word,
        // so the leftover is the silence between finishing speaking and
        // reaching for the key — and a decode of that comes back with the
        // stock phrase, appended to the end of the user's sentence.
        let mut tail = String::new();
        if decode_tail && !self.audio.is_empty() {
            match self.process().await {
                Ok(update) => tail.push_str(&update.committed),
                Err(error) => {
                    tracing::warn!(%error, "final decode failed; keeping what was already agreed");
                }
            }
        }
        tail.push_str(&join_words(self.hypotheses.incomplete()));
        self.audio.clear();
        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_joins_final_and_provisional_text() {
        let update = StreamingUpdate {
            committed: " Hello".to_owned(),
            pending: " wor".to_owned(),
        };
        assert_eq!(update.caption(), " Hello wor");
        assert!(!update.is_empty());
        assert!(StreamingUpdate::default().is_empty());
    }
}
