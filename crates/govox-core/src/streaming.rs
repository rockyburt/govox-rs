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

/// How far the end of an overlapping word may sit from the commit point and
/// still count as the same word rather than a genuine repetition.
///
/// Word timestamps are not stable between decodes of overlapping windows, so
/// this has to be loose. Ordinary movement is 0.3–0.5 s; the largest seen on
/// the corpus is 1.22 s, on `twillingate-drive`, where a tighter bound rejected
/// the overlap and the fallback then swallowed "drove out to Twillingate".
///
/// Raising it to 3.0 changes nothing on the corpus — the useful range is a
/// plateau rather than a tuned edge — so this keeps the smaller value, which
/// leaves more room between drift and a word genuinely said twice.
const JOIN_DRIFT_S: f64 = 2.0;

/// Fallback slack for the timestamp filter, used only when no text overlap can
/// be found. Words starting further back than this are treated as already seen.
const COMMIT_SLACK_S: f64 = 0.1;

/// How much settled audio to keep behind a trim, as run-up for the words after
/// it that are not settled yet. See [`trim_point`].
const TRIM_CONTEXT_S: f64 = 2.0;

/// How many committed words at most are matched to locate the join.
///
/// Long enough that a match is not a coincidence, short enough to survive the
/// model revising a word further back — which is the case that made matching
/// the whole committed region useless. Bounds the search, which is otherwise
/// quadratic in the window's word count.
const MAX_TAIL_MATCH: usize = 8;

/// Whether two hypotheses mean the same word, for matching an overlap.
///
/// Case and edge punctuation are ignored, because the model supplies both from
/// context and the context changes as the window grows: the same audio comes
/// back as "milk." at the end of one hypothesis and "Milk" mid-sentence in the
/// next. Treating those as different words leaves the overlap unrecognised and
/// types the word twice — observed on `ultra-filtered-milk-long` as "milk.
/// Milk".
///
/// Only the edges are stripped, so `rentals.ca` keeps its dot. Words that are
/// nothing but punctuation compare literally, so a comma is not equal to a
/// full stop.
fn same_word(a: &str, b: &str) -> bool {
    let normalize = |s: &str| {
        s.trim()
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let (a_norm, b_norm) = (normalize(a), normalize(b));
    if a_norm.is_empty() || b_norm.is_empty() {
        return a.trim() == b.trim();
    }
    a_norm == b_norm
}

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
    /// The words this hypothesis repeats from the committed tail are dropped,
    /// so the same phrase is not emitted twice.
    pub fn insert(&mut self, words: Vec<TimedWord>) {
        self.incoming = words;

        if let Some(overlap) = self.committed_overlap() {
            self.incoming.drain(..overlap);
            return;
        }

        // No prefix of this hypothesis matches the committed tail, so there is
        // nothing to align against and the timestamps are all that is left.
        // Reached when the model revises a word it has already emitted, which
        // is uncommon; the cost of being wrong here is a repeated word rather
        // than a lost one.
        self.incoming
            .retain(|word| word.start > self.last_committed_time - COMMIT_SLACK_S);
    }

    /// How many leading words of the incoming hypothesis are already committed.
    ///
    /// Whisper re-transcribes the **whole** window on every chunk, so a
    /// hypothesis normally opens by repeating everything already made final.
    /// Those words have to come off, and the question is how to recognise them.
    ///
    /// Matching on text rather than on timestamps, because the timestamps move.
    /// The same word decoded from two overlapping windows can land 0.3–0.5 s
    /// apart — traced on `prose-groceries`, where "the" was reported at
    /// 1.28–1.57 s in one decode and 0.80–0.99 s in the next. A rule that drops
    /// everything starting before the commit point then discards words that
    /// were never committed, and they are gone for good: the audio behind them
    /// stays in the window but the word can never be re-offered. That is the
    /// mechanism behind whole words vanishing mid-sentence.
    ///
    /// The overlap is still anchored in time, just at the **join** instead of
    /// at the start of the hypothesis: the last matched word has to end near
    /// the commit point. Without that, "one" said again after a pause would be
    /// mistaken for the "one" already typed and swallowed.
    ///
    /// Only the **tail** of the committed text is matched, and its position in
    /// the hypothesis is searched for rather than assumed. Requiring the whole
    /// committed region to line up from the first word fails the moment the
    /// model revises any one word inside it — and it does: in a long session
    /// "Demir" came back as "Demerr" six words behind the commit point, no
    /// alignment was found, and the fallback below then re-committed "the",
    /// typing "but the the pipeline".
    ///
    /// The longest tail that matches wins, because a one-word match is easy to
    /// find by coincidence in a sentence that repeats a word. Where a tail
    /// matches in more than one place, the timestamps break the tie: the
    /// candidate ending nearest the commit point is the one meant.
    fn committed_overlap(&self) -> Option<usize> {
        if self.committed.is_empty() || self.incoming.is_empty() {
            return None;
        }
        for len in (1..=MAX_TAIL_MATCH.min(self.committed.len())).rev() {
            let tail = &self.committed[self.committed.len() - len..];
            let mut best: Option<(f64, usize)> = None;
            for end in len..=self.incoming.len() {
                let candidate = &self.incoming[end - len..end];
                if !candidate
                    .iter()
                    .zip(tail)
                    .all(|(a, b)| same_word(&a.text, &b.text))
                {
                    continue;
                }
                let drift = (self.incoming[end - 1].end - self.last_committed_time).abs();
                if drift <= JOIN_DRIFT_S && best.is_none_or(|(seen, _)| drift < seen) {
                    best = Some((drift, end));
                }
            }
            if let Some((_, end)) = best {
                return Some(end);
            }
        }
        None
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
///
/// The cut is held [`TRIM_CONTEXT_S`] *behind* the last committed word rather
/// than exactly at it. Everything before that word is settled and the decoder
/// will never be asked for it again, so cutting there looks free — but the words
/// after it are **not** settled, and they are decoded from whatever audio
/// remains. Cut flush to the commit point and the uncommitted tail is
/// re-transcribed with no run-up at all: traced on a long session, "runs in
/// GitLab" was correct in the window before the trim and came back from the
/// 1.1 s fragment left afterwards as "In GitHub.", then never recovered. Keeping
/// a couple of seconds of already-committed speech costs a little decode time
/// and gives the tail something to be heard against.
///
/// The margin is a preference, not a guarantee: it yields to keeping the buffer
/// inside `limit_s`. A margin comparable to the limit would otherwise reclaim
/// nothing on each trim and let the window grow without bound, which is the
/// pathology [`forced_trim_point`] exists to prevent — an ever-slower decode and
/// eventually a daemon that stops answering its own stop key. The cut also never
/// runs past the commit point, because the audio after it has not been
/// transcribed yet.
#[must_use]
pub fn trim_point(
    committed: &[TimedWord],
    buffer_s: f64,
    limit_s: f64,
    offset_s: f64,
) -> Option<f64> {
    if buffer_s <= limit_s || committed.is_empty() {
        return None;
    }
    let boundary = committed.last()?.end;
    // Where the cut has to be for the buffer to end up at its limit.
    let backstop = offset_s + (buffer_s - limit_s);
    Some((boundary - TRIM_CONTEXT_S).max(backstop).min(boundary))
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

    /// Traced on `prose-groceries`, which streamed as "I need to stop at store
    /// on the way home" — "the" gone from the middle of a sentence the model
    /// transcribes perfectly in one pass.
    #[test]
    fn a_word_is_not_lost_when_timestamps_drift_backwards() {
        let mut buffer = HypothesisBuffer::new();
        let settled = words(&[
            (0.00, 0.28, " I"),
            (0.28, 0.55, " need"),
            (0.55, 1.00, " to"),
            (1.00, 1.24, " stop"),
            (1.24, 1.28, " at"),
        ]);
        buffer.insert(settled.clone());
        buffer.flush();
        buffer.insert(settled);
        buffer.flush();
        assert!((buffer.last_committed_time() - 1.28).abs() < 1e-9);

        // The same five words re-decoded from a longer window, every one of
        // them reported around half a second earlier, plus the next word.
        buffer.insert(words(&[
            (0.00, 0.28, " I"),
            (0.28, 0.31, " need"),
            (0.31, 0.43, " to"),
            (0.43, 0.68, " stop"),
            (0.68, 0.80, " at"),
            (0.80, 0.99, " the"),
        ]));

        assert_eq!(
            texts(&buffer.incoming),
            [" the"],
            "the new word starts behind the commit point only because the \
             timestamps moved; dropping it loses it permanently"
        );
    }

    /// Traced on `twillingate-drive`, which streamed as "We on Saturday
    /// afternoon" against a one-pass "We drove out to Twillingate on Saturday
    /// afternoon". One word was committed from a short window, where its
    /// timestamp ran late; the next hypothesis placed it 1.2 s earlier, the
    /// overlap was rejected as too far from the join, and the fallback filter
    /// then discarded four words that had never been committed.
    #[test]
    fn a_late_committed_word_still_matches_after_a_second_of_drift() {
        let mut buffer = HypothesisBuffer::new();
        buffer.insert(words(&[(1.24, 1.50, " We")]));
        buffer.flush();
        buffer.insert(words(&[(1.24, 1.50, " We")]));
        buffer.flush();
        assert_eq!(texts(&buffer.committed), [" We"]);

        buffer.insert(words(&[
            (0.00, 0.28, " We"),
            (0.28, 0.60, " drove"),
            (0.60, 0.90, " out"),
            (0.90, 1.10, " to"),
            (1.10, 1.60, " Twillingate"),
        ]));

        assert_eq!(
            texts(&buffer.incoming),
            [" drove", " out", " to", " Twillingate"],
            "the overlap is 1.2s from the commit point but is still the same word"
        );
    }

    /// Traced on a long session, which streamed "but the the pipeline runs runs
    /// in GitLab". The model revised a word *behind* the commit point — "Demir"
    /// became "Demerr" — which broke an alignment that required the whole
    /// committed region to match from the first word, and the timestamp
    /// fallback then let the already-committed tail through to be typed twice.
    ///
    /// Only reachable in a window long enough to have been trimmed, so no
    /// single corpus clip exercises it.
    #[test]
    fn a_word_revised_behind_the_commit_point_does_not_cause_a_repeat() {
        let mut buffer = HypothesisBuffer::new();
        let settled = words(&[
            (14.00, 14.66, " Demir"),
            (14.66, 14.92, " is"),
            (14.92, 15.18, " on"),
            (15.18, 16.18, " GitHub,"),
            (16.18, 16.50, " but"),
            (16.50, 16.82, " the"),
        ]);
        buffer.insert(settled.clone());
        buffer.flush();
        buffer.insert(settled);
        buffer.flush();
        assert_eq!(buffer.committed.len(), 6);

        // The same audio, but the model has changed its mind about "Demir" —
        // six words behind the commit point — and carried on past "the".
        buffer.insert(words(&[
            (14.00, 14.73, " Demerr"),
            (14.73, 15.02, " is"),
            (15.02, 15.25, " on"),
            (15.25, 16.24, " GitHub,"),
            (16.24, 16.61, " but"),
            (16.61, 16.98, " the"),
            (16.98, 18.00, " pipeline"),
        ]));

        assert_eq!(
            texts(&buffer.incoming),
            [" pipeline"],
            "everything up to the committed tail must come off, however the \
             model has revised what sits behind it"
        );
    }

    /// The overlap is however much of the window is already settled. Capping it
    /// at five words — as a fixed n-gram guard does — fails on the sixth.
    #[test]
    fn an_overlap_longer_than_five_words_is_recognised() {
        let mut buffer = HypothesisBuffer::new();
        let six = words(&[
            (0.0, 0.2, " one"),
            (0.2, 0.4, " two"),
            (0.4, 0.6, " three"),
            (0.6, 0.8, " four"),
            (0.8, 1.0, " five"),
            (1.0, 1.2, " six"),
        ]);
        buffer.insert(six.clone());
        buffer.flush();
        buffer.insert(six);
        buffer.flush();

        buffer.insert(words(&[
            (0.00, 0.15, " one"),
            (0.15, 0.30, " two"),
            (0.30, 0.45, " three"),
            (0.45, 0.60, " four"),
            (0.60, 0.75, " five"),
            (0.75, 0.90, " six"),
            (0.90, 1.10, " seven"),
        ]));

        assert_eq!(texts(&buffer.incoming), [" seven"]);
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
        assert_eq!(trim_point(&committed, 10.0, 15.0, 0.0), None);
    }

    #[test]
    fn trimming_happens_at_a_committed_word_boundary() {
        // Cutting mid-word makes the next decode hallucinate a fragment. Here
        // the buffer is 5s over its limit, so the backstop outweighs the context
        // margin and the cut lands on the boundary itself.
        let committed = words(&[(0.0, 1.0, " a"), (1.0, 2.5, " b")]);
        assert_eq!(trim_point(&committed, 20.0, 15.0, 0.0), Some(2.5));
    }

    #[test]
    fn a_trim_keeps_some_settled_audio_as_run_up() {
        // The case from a long session: barely over the limit, so the margin is
        // affordable and the cut is held behind the commit point. Cutting flush
        // at 18.21 left "in GitLab" to be decoded from a 1.1s fragment.
        let committed = words(&[(17.94, 18.21, " runs")]);
        assert_eq!(
            trim_point(&committed, 10.36, 10.0, 9.0),
            Some(18.21 - TRIM_CONTEXT_S)
        );
    }

    #[test]
    fn the_context_margin_never_lets_the_buffer_outgrow_its_limit() {
        // A margin bigger than the limit would reclaim nothing and the window
        // would grow for the length of the session.
        let committed = words(&[(0.0, 0.5, " a")]);
        let cut = trim_point(&committed, 2.0, 1.5, 0.0).expect("over the limit");
        assert!(
            (cut - 0.5).abs() < 1e-9,
            "the cut must still reclaim the 0.5s the buffer is over by, got {cut}"
        );
    }

    #[test]
    fn a_trim_never_cuts_past_the_commit_point() {
        // Audio after the commit point has not been transcribed yet.
        let committed = words(&[(0.0, 1.0, " a")]);
        let cut = trim_point(&committed, 30.0, 5.0, 0.0).expect("over the limit");
        assert!(cut <= 1.0, "cut {cut} is past the last committed word");
    }

    #[test]
    fn an_over_long_buffer_with_nothing_committed_has_no_word_boundary() {
        // There is no settled word to cut at, so this function declines. The
        // buffer is still bounded — by `forced_trim_point` below.
        assert_eq!(trim_point(&[], 20.0, 15.0, 0.0), None);
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
