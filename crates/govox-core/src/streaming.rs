//! LocalAgreement-2: deciding which words are safe to show as final.
//!
//! Reimplemented from the *algorithm*, not translated from `govox-py`'s
//! vendored 970-line `whisper_online.py`, of which only two symbols were ever
//! used. The rule is simple and the value is entirely in getting the edges
//! right:
//!
//! > A word is committed once two consecutive hypotheses agree on it, in order,
//! > from the start of the uncommitted region.
//!
//! Whisper re-decodes its whole audio window on every chunk, so early words
//! change as later context arrives — "I scream" becomes "ice cream". Committing
//! on the first hypothesis would type text that then has to be taken back;
//! waiting for agreement means a word is only made final once a second, better
//! informed pass confirmed it.
//!
//! Pure: no audio, no model, no async. The whole state machine is exercised
//! with hand-written word lists.

/// One recognised word with its span, in seconds from the session start.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

impl TimedWord {
    #[must_use]
    pub fn new(start: f64, end: f64, text: impl Into<String>) -> Self {
        Self {
            start,
            end,
            text: text.into(),
        }
    }
}

/// How many consecutive words are checked for an n-gram repeat.
///
/// Whisper sometimes re-emits words it has already produced when its window
/// slides. Without this the same phrase is typed twice.
const MAX_NGRAM: usize = 5;

/// Words whose start is more than this far behind the last commit are dropped
/// as already-seen. The slack absorbs timestamp jitter between decodes of
/// overlapping windows.
const COMMIT_SLACK_S: f64 = 0.1;

/// Accumulates hypotheses and commits their agreed prefix.
#[derive(Debug, Default)]
pub struct HypothesisBuffer {
    /// The previous hypothesis, still awaiting agreement.
    buffer: Vec<TimedWord>,
    /// The hypothesis just inserted.
    incoming: Vec<TimedWord>,
    /// Committed words still inside the audio window.
    committed: Vec<TimedWord>,
    last_committed_time: f64,
}

impl HypothesisBuffer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a new hypothesis for the current window.
    ///
    /// Words already behind the commit point are discarded, and a repeated
    /// n-gram at the join is dropped so the same phrase is not emitted twice.
    pub fn insert(&mut self, words: Vec<TimedWord>) {
        self.incoming = words
            .into_iter()
            .filter(|word| word.start > self.last_committed_time - COMMIT_SLACK_S)
            .collect();

        let Some(first) = self.incoming.first() else {
            return;
        };
        // Only de-duplicate at a real join. A hypothesis starting well after
        // the commit point is new material, not a repeat.
        if (first.start - self.last_committed_time).abs() >= 1.0 || self.committed.is_empty() {
            return;
        }

        let limit = MAX_NGRAM.min(self.committed.len()).min(self.incoming.len());
        for n in 1..=limit {
            let tail: Vec<&str> = self.committed[self.committed.len() - n..]
                .iter()
                .map(|w| w.text.as_str())
                .collect();
            let head: Vec<&str> = self.incoming[..n].iter().map(|w| w.text.as_str()).collect();
            if tail == head {
                self.incoming.drain(..n);
                break;
            }
        }
    }

    /// Commit the longest common prefix of the two most recent hypotheses.
    ///
    /// Returns the newly committed words, which are safe to display as final.
    pub fn flush(&mut self) -> Vec<TimedWord> {
        let mut commit = Vec::new();
        while let (Some(new), Some(old)) = (self.incoming.first(), self.buffer.first()) {
            if new.text != old.text {
                break;
            }
            let word = self.incoming.remove(0);
            self.buffer.remove(0);
            self.last_committed_time = word.end;
            commit.push(word);
        }
        // Whatever did not agree becomes the hypothesis the *next* insert is
        // compared against.
        self.buffer = std::mem::take(&mut self.incoming);
        self.committed.extend(commit.iter().cloned());
        commit
    }

    /// Forget committed words that have fallen out of the audio window.
    pub fn pop_committed(&mut self, before: f64) {
        self.committed.retain(|word| word.end > before);
    }

    /// Committed words still inside the audio window.
    ///
    /// Used to decide where the buffer can safely be trimmed.
    #[must_use]
    pub fn committed_words(&self) -> &[TimedWord] {
        &self.committed
    }

    /// The uncommitted tail, for display as provisional text.
    #[must_use]
    pub fn incomplete(&self) -> &[TimedWord] {
        &self.buffer
    }

    #[must_use]
    pub const fn last_committed_time(&self) -> f64 {
        self.last_committed_time
    }
}

/// Where to trim the audio buffer once it grows past its limit.
///
/// Trimming at a committed word boundary is what keeps the window bounded
/// without cutting a word in half — a cut mid-word makes the next decode
/// hallucinate a fragment.
#[must_use]
pub fn trim_point(committed: &[TimedWord], buffer_s: f64, limit_s: f64) -> Option<f64> {
    if buffer_s <= limit_s || committed.is_empty() {
        return None;
    }
    // The end of the last committed word: everything before it is settled, so
    // the decoder never needs that audio again.
    committed.last().map(|word| word.end)
}

/// Where to cut when the buffer is over its limit and nothing has committed.
///
/// [`trim_point`] can only cut at a settled word, so it declines when the
/// recognizer has produced none. That is the dangerous case rather than a
/// harmless one: Whisper answers a chunk it cannot place with no words at all,
/// and it re-decodes the whole window every time, so a window that yields
/// nothing grows for as long as the user keeps speaking and each decode costs
/// more than the last. Left alone it stops being a quality problem and becomes
/// an unresponsive daemon.
///
/// So cut blind, keeping the most recent `limit_s` seconds. The audio being
/// dropped is audio that produced nothing.
///
/// `offset_s` is where the buffer starts in session time; the result is in the
/// same frame as [`trim_point`]'s.
#[must_use]
pub fn forced_trim_point(buffer_s: f64, limit_s: f64, offset_s: f64) -> Option<f64> {
    if buffer_s <= limit_s {
        return None;
    }
    Some(offset_s + (buffer_s - limit_s))
}

/// How many samples to drop from the front to leave `keep_s` seconds behind.
///
/// Zero when the buffer is already that short or shorter — never a negative
/// count wrapped into a huge one, which is what a plain subtraction on
/// `usize` would produce and what would then drain the entire buffer.
#[must_use]
pub fn samples_to_drop(buffered: usize, sample_rate: u32, keep_s: f64) -> usize {
    let keep = (keep_s.max(0.0) * f64::from(sample_rate)) as usize;
    buffered.saturating_sub(keep)
}

/// Phrases Whisper emits when it is given audio with nothing to transcribe.
///
/// Not a general profanity-style filter: these are compared against the
/// *whole* hypothesis, so an utterance that merely contains one is unaffected.
const SILENCE_ARTIFACTS: &[&str] = &[
    "thank you for watching",
    "thanks for watching",
    "thank you",
    "you",
    "bye",
    "subscribe",
    "subtitles by the amara.org community",
];

/// Does this look like Whisper's answer to silence rather than to speech?
///
/// Two families, both seen in the field on this machine's weights. Bare
/// domains — `www.github.com`, then `www.johnson.com` the session after —
/// and a handful of stock sign-off phrases. Both arrive as the *entire*
/// hypothesis, which is what makes them separable from real speech: a person
/// dictating "thank you for watching" mid-sentence produces a hypothesis with
/// more in it than that.
///
/// Used only to withhold a session's opening words until a second decode has
/// agreed with them. Nothing is discarded on the strength of this — a real
/// utterance that trips it is shown a decode later, not dropped.
#[must_use]
pub fn is_silence_artifact(text: &str) -> bool {
    let cleaned: String = text
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '.' | ',' | '!' | '?' | '"' | '\'' | '…'))
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return false;
    }
    if SILENCE_ARTIFACTS.contains(&cleaned) {
        return true;
    }
    // A bare domain: one token, no spaces, and shaped like a hostname. Real
    // dictation does not produce these, because the correction pipeline has
    // no path that turns spoken words into a URL.
    !cleaned.contains(' ') && (cleaned.starts_with("www") || cleaned.ends_with("com"))
}

/// Join committed words into displayable text.
///
/// Whisper emits each word with its own leading space, so the separator is the
/// empty string — inserting one here would double every space.
#[must_use]
pub fn join_words(words: &[TimedWord]) -> String {
    words.iter().map(|w| w.text.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(specs: &[(f64, f64, &str)]) -> Vec<TimedWord> {
        specs
            .iter()
            .map(|(s, e, t)| TimedWord::new(*s, *e, *t))
            .collect()
    }

    fn texts(words: &[TimedWord]) -> Vec<String> {
        words.iter().map(|w| w.text.clone()).collect()
    }

    #[test]
    fn the_first_hypothesis_commits_nothing() {
        // Nothing to agree with yet. Committing here is the mistake the whole
        // algorithm exists to avoid.
        let mut buffer = HypothesisBuffer::new();
        buffer.insert(words(&[(0.0, 0.5, " I"), (0.5, 1.0, " scream")]));
        assert!(buffer.flush().is_empty());
    }

    #[test]
    fn two_agreeing_hypotheses_commit_their_prefix() {
        let mut buffer = HypothesisBuffer::new();
        buffer.insert(words(&[(0.0, 0.5, " hello"), (0.5, 1.0, " there")]));
        buffer.flush();

        buffer.insert(words(&[
            (0.0, 0.5, " hello"),
            (0.5, 1.0, " there"),
            (1.0, 1.5, " world"),
        ]));
        let commit = buffer.flush();

        assert_eq!(texts(&commit), [" hello", " there"]);
        // "world" has been seen once and is still provisional.
        assert_eq!(texts(buffer.incomplete()), [" world"]);
    }

    #[test]
    fn a_word_that_changes_on_reflection_is_never_committed() {
        // The case the algorithm is for: "I scream" → "ice cream" once later
        // context arrives. Committing the first guess would type text that has
        // to be taken back.
        let mut buffer = HypothesisBuffer::new();
        buffer.insert(words(&[(0.0, 0.5, " I"), (0.5, 1.0, " scream")]));
        buffer.flush();

        buffer.insert(words(&[(0.0, 0.5, " ice"), (0.5, 1.0, " cream")]));
        assert!(
            buffer.flush().is_empty(),
            "the hypotheses disagree from the first word"
        );
        assert_eq!(texts(buffer.incomplete()), [" ice", " cream"]);
    }

    #[test]
    fn agreement_stops_at_the_first_disagreement() {
        let mut buffer = HypothesisBuffer::new();
        buffer.insert(words(&[
            (0.0, 0.5, " the"),
            (0.5, 1.0, " quick"),
            (1.0, 1.5, " brown"),
        ]));
        buffer.flush();
        buffer.insert(words(&[
            (0.0, 0.5, " the"),
            (0.5, 1.0, " quick"),
            (1.0, 1.5, " brownish"),
        ]));

        // Prefix only: "brown"/"brownish" differ, so nothing past them commits.
        assert_eq!(texts(&buffer.flush()), [" the", " quick"]);
    }

    #[test]
    fn committing_advances_the_watermark() {
        let mut buffer = HypothesisBuffer::new();
        buffer.insert(words(&[(0.0, 0.5, " hello")]));
        buffer.flush();
        buffer.insert(words(&[(0.0, 0.5, " hello"), (0.5, 1.0, " world")]));
        buffer.flush();

        assert!((buffer.last_committed_time() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn words_behind_the_commit_point_are_discarded() {
        // Whisper re-decodes its whole window, so every hypothesis repeats
        // words already made final. Re-committing them would type them twice.
        let mut buffer = HypothesisBuffer::new();
        buffer.insert(words(&[(0.0, 0.5, " hello")]));
        buffer.flush();
        buffer.insert(words(&[(0.0, 0.5, " hello"), (0.5, 1.0, " world")]));
        buffer.flush();

        // A later hypothesis still containing the committed word.
        buffer.insert(words(&[
            (0.0, 0.5, " hello"),
            (0.5, 1.0, " world"),
            (1.0, 1.5, " again"),
        ]));
        assert!(
            !texts(&buffer.incoming).contains(&" hello".to_owned()),
            "an already-committed word survived the filter"
        );
    }

    #[test]
    fn a_repeated_ngram_at_the_join_is_dropped_once() {
        // Whisper re-emits words when its window slides; without this the same
        // phrase lands in the document twice.
        let mut buffer = HypothesisBuffer::new();
        buffer.insert(words(&[(0.0, 0.5, " one"), (0.5, 1.0, " two")]));
        buffer.flush();
        buffer.insert(words(&[(0.0, 0.5, " one"), (0.5, 1.0, " two")]));
        let commit = buffer.flush();
        assert_eq!(texts(&commit), [" one", " two"]);

        // A hypothesis that starts by repeating the committed tail.
        buffer.insert(words(&[(0.95, 1.4, " two"), (1.4, 1.9, " three")]));
        assert_eq!(
            texts(&buffer.incoming),
            [" three"],
            "the repeated word should have been dropped"
        );
    }

    #[test]
    fn material_well_past_the_commit_point_is_left_alone() {
        // A gap means new speech, not a repeat, so the de-duplication must not
        // fire and eat a genuine word.
        let mut buffer = HypothesisBuffer::new();
        buffer.insert(words(&[(0.0, 0.5, " one")]));
        buffer.flush();
        buffer.insert(words(&[(0.0, 0.5, " one")]));
        buffer.flush();

        // A pause, then the same word said again for real.
        buffer.insert(words(&[(5.0, 5.5, " one"), (5.5, 6.0, " more")]));
        assert_eq!(
            texts(&buffer.incoming),
            [" one", " more"],
            "a genuine repeat after a pause must survive"
        );
    }

    #[test]
    fn committed_words_are_forgotten_once_out_of_the_window() {
        let mut buffer = HypothesisBuffer::new();
        for _ in 0..2 {
            buffer.insert(words(&[(0.0, 0.5, " a"), (0.5, 1.0, " b")]));
            buffer.flush();
        }
        assert_eq!(buffer.committed.len(), 2);

        buffer.pop_committed(0.75);
        assert_eq!(texts(&buffer.committed), [" b"], "'a' ended before 0.75");
    }

    #[test]
    fn the_buffer_is_not_trimmed_before_its_limit() {
        let committed = words(&[(0.0, 1.0, " a")]);
        assert_eq!(trim_point(&committed, 10.0, 15.0), None);
    }

    #[test]
    fn trimming_happens_at_a_committed_word_boundary() {
        // Cutting mid-word makes the next decode hallucinate a fragment.
        let committed = words(&[(0.0, 1.0, " a"), (1.0, 2.5, " b")]);
        assert_eq!(trim_point(&committed, 20.0, 15.0), Some(2.5));
    }

    #[test]
    fn an_over_long_buffer_with_nothing_committed_has_no_word_boundary() {
        // There is no settled word to cut at, so this function declines. The
        // buffer is still bounded — by `forced_trim_point` below.
        assert_eq!(trim_point(&[], 20.0, 15.0), None);
    }

    #[test]
    fn whispers_answers_to_silence_are_recognised() {
        // Both observed in the journal on this machine's weights, one session
        // after the other.
        assert!(is_silence_artifact("www.github.com"));
        assert!(is_silence_artifact("www.johnson.com"));
        assert!(is_silence_artifact("Thank you for watching!"));
        assert!(is_silence_artifact(" thanks for watching. "));
    }

    #[test]
    fn real_speech_is_not_mistaken_for_an_artifact() {
        // The whole hypothesis is compared, so a stock phrase inside a longer
        // utterance is ordinary speech and shows immediately.
        assert!(!is_silence_artifact("thank you for watching the demo"));
        assert!(!is_silence_artifact("for breakfast I had oatmeal"));
        assert!(!is_silence_artifact("commit the change"));
        assert!(!is_silence_artifact(""));
        assert!(!is_silence_artifact("   "));
    }

    #[test]
    fn the_preroll_keeps_the_tail_and_drops_the_rest() {
        // 1s buffered at 16 kHz, keeping 0.3s: drop the first 0.7s.
        assert_eq!(samples_to_drop(16_000, 16_000, 0.3), 11_200);
    }

    #[test]
    fn the_preroll_never_drops_more_than_it_has() {
        // The saturating subtraction is the point: a plain `-` on usize would
        // wrap to about 18 quintillion and drain the whole buffer, throwing
        // away the very speech the pre-roll exists to keep.
        assert_eq!(samples_to_drop(1_000, 16_000, 0.3), 0);
        assert_eq!(samples_to_drop(0, 16_000, 0.3), 0);
    }

    #[test]
    fn a_zero_preroll_drops_everything_buffered() {
        assert_eq!(samples_to_drop(16_000, 16_000, 0.0), 16_000);
        // A negative keep is clamped rather than wrapping.
        assert_eq!(samples_to_drop(16_000, 16_000, -1.0), 16_000);
    }

    #[test]
    fn the_backstop_leaves_the_buffer_at_its_limit() {
        // 20s buffered against a 15s limit: drop the oldest 5s, so the cut
        // lands 5s after the buffer's start.
        assert_eq!(forced_trim_point(20.0, 15.0, 0.0), Some(5.0));
    }

    #[test]
    fn the_backstop_reports_the_cut_in_session_time() {
        // The caller subtracts `offset_s` again to get a buffer index, so the
        // two frames have to agree or the trim cuts the wrong audio.
        assert_eq!(forced_trim_point(20.0, 15.0, 100.0), Some(105.0));
    }

    #[test]
    fn the_backstop_leaves_a_buffer_inside_its_limit_alone() {
        // Trimming a window the decoder is still entitled to see would cost
        // accuracy for no benefit; the runaway is the only thing being guarded.
        assert_eq!(forced_trim_point(10.0, 15.0, 0.0), None);
        assert_eq!(forced_trim_point(15.0, 15.0, 0.0), None);
    }

    #[test]
    fn words_join_without_inserting_spaces() {
        // Whisper emits each word with its own leading space; adding another
        // would double every one.
        let joined = join_words(&words(&[(0.0, 0.5, " Hello"), (0.5, 1.0, " world.")]));
        assert_eq!(joined, " Hello world.");
        assert!(!joined.contains("  "));
    }

    #[test]
    fn a_long_session_commits_everything_exactly_once() {
        // The property that matters end to end: no word typed twice, none lost.
        let script = [" the", " quick", " brown", " fox", " jumps", " over"];
        let mut buffer = HypothesisBuffer::new();
        let mut committed: Vec<String> = Vec::new();

        for length in 1..=script.len() {
            let hypothesis: Vec<TimedWord> = script[..length]
                .iter()
                .enumerate()
                .map(|(i, w)| TimedWord::new(i as f64 * 0.5, (i as f64 + 1.0) * 0.5, *w))
                .collect();
            buffer.insert(hypothesis);
            committed.extend(texts(&buffer.flush()));
        }
        // The final word has only been seen once, so it stays provisional.
        committed.extend(texts(buffer.incomplete()));

        assert_eq!(committed, script, "words were duplicated or dropped");
    }
}
