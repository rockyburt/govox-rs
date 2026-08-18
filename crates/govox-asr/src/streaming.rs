//! Turning a growing audio buffer into a growing caption.
//!
//! Holds the audio window and feeds it to the model on each chunk; the
//! *decision* about which words are final lives in
//! [`govox_core::streaming`], which is pure and tested without a model.

use std::collections::VecDeque;

use govox_core::config::{BufferTrimming, StreamingConfig};
use govox_core::domain::{GovoxError, WordRecognizer};
use govox_core::streaming::{HypothesisBuffer, TimedWord, join_words, trim_point};

use crate::whisper::WhisperHandle;

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

/// Feeds a growing window to a recognizer and commits what two passes agree on.
///
/// Generic over [`WordRecognizer`] so the window management, trimming and
/// offset arithmetic below can be tested against a scripted recognizer rather
/// than a loaded model. The type parameter defaults to [`WhisperHandle`], so
/// callers naming `OnlineProcessor` bare still get the Whisper processor.
pub struct OnlineProcessor<R = WhisperHandle> {
    asr: R,
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

impl<R: WordRecognizer> OnlineProcessor<R> {
    #[must_use]
    pub fn new(asr: R, config: &StreamingConfig, sample_rate: u32) -> Self {
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
    pub async fn process(&mut self) -> Result<StreamingUpdate, GovoxError> {
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

        // The raw hypothesis, before agreement decides any of it. Words are
        // lost between here and the caption often enough that reconstructing
        // this from the outside is the first thing anyone needs; decoding the
        // window twice to get it would change what the recognizer does.
        if tracing::enabled!(tracing::Level::DEBUG) {
            tracing::debug!(
                window_s = self.buffered_s(),
                offset_s = self.offset_s,
                hypothesis = %shifted
                    .iter()
                    .map(|w| format!("{}[{:.2}-{:.2}]", w.text.trim(), w.start, w.end))
                    .collect::<Vec<_>>()
                    .join(" "),
                "streaming hypothesis"
            );
        }

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
        // language — a dependency for a bounded gain over word-boundary
        // trimming. Recorded in docs/parity.md, not silently treated as equal.
        if self.trimming == BufferTrimming::Sentence {
            tracing::debug!("sentence trimming is not implemented; trimming at a word boundary");
        }

        let buffered_s = self.buffered_s();
        let cut_s = match trim_point(
            self.hypotheses.committed_words(),
            buffered_s,
            self.buffer_limit_s,
            self.offset_s,
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
        // Decode what has not been decoded yet: between two decodes there is
        // always up to `min_chunk_size_s` of audio the model has never seen,
        // and at a session's end that is the last thing the user said —
        // returning only the standing hypothesis truncates the final word.
        //
        // `decode_tail` is the caller's answer to "is there speech in there?".
        // Usually not: the leftover is the silence between the last word and
        // reaching for the key, which decodes to the stock phrase, appended.
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
    use govox_core::config::StreamingEngine;
    use govox_core::domain::ScriptedWordRecognizer;

    const RATE: u32 = 16_000;

    fn config(min_chunk_s: f64, limit_s: f64) -> StreamingConfig {
        StreamingConfig {
            enabled: true,
            engine: StreamingEngine::WhisperStreaming,
            min_chunk_size_s: min_chunk_s,
            buffer_trimming: BufferTrimming::Segment,
            buffer_trimming_sec: limit_s,
            vad: true,
            fallback_to_utterance: true,
        }
    }

    /// Audio is never inspected by these tests — only its length matters, which
    /// is the whole point of decoding through the trait.
    fn audio(seconds: f64) -> Vec<f32> {
        vec![0.0; (seconds * f64::from(RATE)) as usize]
    }

    fn word(start: f64, end: f64, text: &str) -> TimedWord {
        TimedWord::new(start, end, text)
    }

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

    /// The model sees only the current window, so its timestamps restart at
    /// zero after every trim. If the offset is not added back, a word decoded
    /// after a trim looks *older* than the last commit and `HypothesisBuffer`
    /// discards it as already-seen — the session silently drops words rather
    /// than failing.
    #[tokio::test]
    async fn word_times_stay_in_session_time_across_a_trim() {
        // Window-relative spans. After the trim the buffer origin moves to
        // 0.5s, so "again" at 0.1 is really 0.6 in session time — ahead of the
        // 0.5 commit point. Unshifted it would be behind it, and dropped.
        let asr = ScriptedWordRecognizer::saying(vec![
            vec![word(0.0, 0.5, "hello")],
            vec![word(0.0, 0.5, "hello"), word(0.6, 1.2, "world")],
            vec![word(0.1, 0.7, "again")],
            vec![word(0.1, 0.7, "again")],
        ]);
        let mut processor = OnlineProcessor::new(asr, &config(1.0, 1.5), RATE);

        processor.push(&audio(1.0));
        assert_eq!(processor.process().await.unwrap().committed.trim(), "");

        processor.push(&audio(1.0));
        let update = processor.process().await.unwrap();
        assert_eq!(update.committed.trim(), "hello");
        // 2.0s buffered is over the 1.5s limit and "hello" ends at 0.5, so the
        // first half-second is dropped and the origin moves with it.
        assert!(
            (processor.offset_s - 0.5).abs() < 1e-9,
            "expected a 0.5s trim"
        );

        processor.push(&audio(1.0));
        processor.process().await.unwrap();
        let update = processor.process().await.unwrap();
        assert_eq!(
            update.caption().trim(),
            "again",
            "a word decoded after a trim was discarded as already-seen"
        );
    }

    /// The pre-roll drop moves the buffer origin, and must not be allowed to
    /// underflow into draining the whole buffer.
    #[tokio::test]
    async fn keeping_only_the_last_seconds_moves_the_origin() {
        let asr = ScriptedWordRecognizer::saying(vec![vec![word(0.0, 0.2, "hi")]]);
        let mut processor = OnlineProcessor::new(asr, &config(1.0, 10.0), RATE);

        processor.push(&audio(3.0));
        processor.keep_only_last(0.5);
        assert!((processor.offset_s - 2.5).abs() < 1e-9);

        // Shorter than the pre-roll: nothing to drop, and emphatically not a
        // wrapped subtraction that drains everything.
        processor.keep_only_last(4.0);
        assert!((processor.offset_s - 2.5).abs() < 1e-9);

        processor.process().await.unwrap();
        assert_eq!(
            processor.asr.windows(),
            vec![8_000],
            "the decode should see only the half-second that was kept"
        );
    }

    /// A recognizer that returns nothing commits nothing, so there is no word
    /// boundary to trim at — and Whisper re-decodes the whole window every
    /// chunk, so an untrimmed window gets slower without bound.
    #[tokio::test]
    async fn an_uncommitted_window_is_still_bounded() {
        let asr = ScriptedWordRecognizer::saying(vec![vec![], vec![]]);
        let mut processor = OnlineProcessor::new(asr, &config(1.0, 1.0), RATE);

        processor.push(&audio(3.0));
        processor.process().await.unwrap();

        assert!(
            (processor.offset_s - 2.0).abs() < 1e-9,
            "the backstop should have cut back to the limit"
        );
        processor.process().await.unwrap();
        assert_eq!(
            processor.asr.windows(),
            vec![48_000, 16_000],
            "the second decode should see a window bounded by the limit"
        );
    }

    #[tokio::test]
    async fn an_empty_buffer_is_not_decoded() {
        let asr = ScriptedWordRecognizer::saying(vec![vec![word(0.0, 0.5, "never")]]);
        let mut processor = OnlineProcessor::new(asr, &config(1.0, 10.0), RATE);

        assert!(processor.process().await.unwrap().is_empty());
        assert_eq!(processor.asr.calls(), 0, "decoded an empty window");
    }

    /// A stumble on the final decode must not cost the user the words already
    /// agreed — `finish` degrades to them rather than propagating the error.
    #[tokio::test]
    async fn a_failed_final_decode_keeps_what_was_agreed() {
        let asr = ScriptedWordRecognizer::failing_nth(
            vec![
                vec![word(0.0, 0.5, "hello")],
                vec![word(0.0, 0.5, "hello"), word(0.6, 1.2, "world")],
            ],
            3,
        );
        let mut processor = OnlineProcessor::new(asr, &config(1.0, 10.0), RATE);

        processor.push(&audio(1.0));
        processor.process().await.unwrap();
        processor.push(&audio(1.0));
        assert_eq!(processor.process().await.unwrap().committed.trim(), "hello");

        assert_eq!(processor.finish(true).await.trim(), "world");
        assert_eq!(
            processor.asr.calls(),
            3,
            "the tail should have been decoded"
        );
    }
}
