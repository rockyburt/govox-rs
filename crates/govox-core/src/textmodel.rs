//! Dictation-buffer text model.
//!
//! Records exactly what govox injected, and how recently. It needs no
//! privileges and no accessibility stack, but it only ever knows about text
//! govox itself produced, and it desynchronizes the moment the user types or
//! clicks.
//!
//! **It cannot detect that moment.** Nothing on GNOME Wayland tells an
//! unprivileged process that the focused window changed, so [`TextModel::reset`]
//! — which the caller is meant to invoke on a focus change — has no trigger
//! available to it. A stale record is not cosmetic: "delete that" backspaces
//! one character per character of the remembered text, so believing a stale
//! span means firing a burst of backspaces into whatever the user has since
//! clicked on.
//!
//! Since focus cannot be observed, recency is the proxy: a record older than
//! `ttl_s` is treated as unknown. That does not make a stale delete impossible
//! — clicking away and saying "delete that" two seconds later still fires — it
//! bounds how long the window of belief stays open. Deliberately conservative:
//! this backend would rather report "I don't know" than hand out an offset that
//! might be stale.

use std::sync::Mutex;

use crate::domain::{FieldSnapshot, TextModel};

/// Long enough for the natural "dictate, read it back, say 'delete that'"
/// loop; short enough that a record cannot survive a coffee break and then eat
/// text in a different window.
pub const DEFAULT_TTL_S: f64 = 30.0;

/// Monotonic seconds. Injected so tests advance time without sleeping.
pub trait Clock: Send + Sync {
    fn now_s(&self) -> f64;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct MonotonicClock;

impl Clock for MonotonicClock {
    fn now_s(&self) -> f64 {
        use std::sync::OnceLock;
        static START: OnceLock<std::time::Instant> = OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
    }
}

#[derive(Debug, Default)]
struct Record {
    last: Option<String>,
    recorded_at: f64,
}

/// In-session record of govox's own most recent insertion.
///
/// Only the most recent one is retained. Deleting further back would require
/// knowing the user has not moved the caret in between, which this backend
/// cannot know.
pub struct DictationBuffer<C: Clock = MonotonicClock> {
    pub ttl_s: f64,
    clock: C,
    // A `Mutex` rather than `&mut self` because `TextModel` takes `&self`: the
    // pipeline shares one text model across tasks. Uncontended in practice —
    // only the daemon's own loop touches it.
    record: Mutex<Record>,
}

impl DictationBuffer<MonotonicClock> {
    #[must_use]
    pub fn new(ttl_s: f64) -> Self {
        Self::with_clock(ttl_s, MonotonicClock)
    }
}

impl<C: Clock> DictationBuffer<C> {
    #[must_use]
    pub fn with_clock(ttl_s: f64, clock: C) -> Self {
        Self {
            ttl_s,
            clock,
            record: Mutex::new(Record::default()),
        }
    }

    /// The record, or `None` once it is too old to be trusted.
    ///
    /// Drops it rather than re-checking forever: once stale, always stale.
    fn unexpired(&self) -> Option<String> {
        let mut record = self.record.lock().expect("dictation buffer poisoned");
        let last = record.last.clone()?;
        if self.clock.now_s() - record.recorded_at >= self.ttl_s {
            record.last = None;
            return None;
        }
        Some(last)
    }
}

impl<C: Clock> TextModel for DictationBuffer<C> {
    fn last_insertion(&self) -> Option<String> {
        self.unexpired()
    }

    fn record_insertion(&self, text: &str) {
        let mut record = self.record.lock().expect("dictation buffer poisoned");
        // Empty text is "nothing remembered", not "remembered nothing": an
        // empty span would make "delete that" a silent no-op that still
        // consumed the record.
        record.last = (!text.is_empty()).then(|| text.to_owned());
        record.recorded_at = self.clock.now_s();
    }

    fn consume_last(&self) -> Option<String> {
        let last = self.unexpired();
        self.record.lock().expect("dictation buffer poisoned").last = None;
        last
    }

    /// Always `None` — this backend cannot see the field at all.
    ///
    /// It knows what govox *typed*, which is not the same as what is there:
    /// the user may have typed, clicked or scrolled since. Returning a snapshot
    /// built from the record would be a fabrication, and callers use snapshots
    /// specifically to *check* that belief.
    fn read_field(&self) -> Option<FieldSnapshot> {
        None
    }

    fn reset(&self) {
        self.record.lock().expect("dictation buffer poisoned").last = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// A clock the test drives by hand.
    #[derive(Default)]
    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn advance(&self, seconds: f64) {
            self.0
                .fetch_add((seconds * 1000.0) as u64, Ordering::Relaxed);
        }
    }

    impl Clock for &FakeClock {
        fn now_s(&self) -> f64 {
            self.0.load(Ordering::Relaxed) as f64 / 1000.0
        }
    }

    fn buffer(clock: &FakeClock) -> DictationBuffer<&FakeClock> {
        DictationBuffer::with_clock(DEFAULT_TTL_S, clock)
    }

    #[test]
    fn nothing_is_remembered_to_begin_with() {
        let clock = FakeClock::default();
        assert_eq!(buffer(&clock).last_insertion(), None);
    }

    #[test]
    fn an_insertion_is_remembered() {
        let clock = FakeClock::default();
        let buffer = buffer(&clock);
        buffer.record_insertion("Hello world.");
        assert_eq!(buffer.last_insertion().as_deref(), Some("Hello world."));
    }

    #[test]
    fn a_record_expires_at_the_ttl() {
        let clock = FakeClock::default();
        let buffer = buffer(&clock);
        buffer.record_insertion("Hello world.");

        clock.advance(DEFAULT_TTL_S - 0.1);
        assert!(buffer.last_insertion().is_some(), "still inside the window");

        clock.advance(0.2);
        assert_eq!(
            buffer.last_insertion(),
            None,
            "past the TTL the record must not be trusted"
        );
    }

    #[test]
    fn expiry_is_inclusive_at_exactly_the_ttl() {
        // `>=`, matching govox-py. Worth pinning because the boundary decides
        // whether a burst of backspaces fires into a window govox may no longer
        // own — the conservative side is to forget.
        let clock = FakeClock::default();
        let buffer = buffer(&clock);
        buffer.record_insertion("Hello world.");
        clock.advance(DEFAULT_TTL_S);
        assert_eq!(buffer.last_insertion(), None);
    }

    #[test]
    fn a_new_insertion_restarts_the_clock() {
        let clock = FakeClock::default();
        let buffer = buffer(&clock);
        buffer.record_insertion("first");
        clock.advance(DEFAULT_TTL_S - 1.0);
        buffer.record_insertion("second");
        clock.advance(2.0);
        assert_eq!(
            buffer.last_insertion().as_deref(),
            Some("second"),
            "the second insertion has its own TTL"
        );
    }

    #[test]
    fn empty_text_clears_rather_than_records() {
        let clock = FakeClock::default();
        let buffer = buffer(&clock);
        buffer.record_insertion("Hello");
        buffer.record_insertion("");
        assert_eq!(
            buffer.last_insertion(),
            None,
            "an empty span would make 'delete that' a no-op that still consumed the record"
        );
    }

    #[test]
    fn consuming_forgets_the_span() {
        // The guard against a second "delete that" eating text govox never
        // typed.
        let clock = FakeClock::default();
        let buffer = buffer(&clock);
        buffer.record_insertion("Hello world.");

        assert_eq!(buffer.consume_last().as_deref(), Some("Hello world."));
        assert_eq!(buffer.consume_last(), None);
        assert_eq!(buffer.last_insertion(), None);
    }

    #[test]
    fn consuming_an_expired_record_yields_nothing() {
        let clock = FakeClock::default();
        let buffer = buffer(&clock);
        buffer.record_insertion("Hello world.");
        clock.advance(DEFAULT_TTL_S + 1.0);
        assert_eq!(buffer.consume_last(), None);
    }

    #[test]
    fn reset_forgets_everything() {
        let clock = FakeClock::default();
        let buffer = buffer(&clock);
        buffer.record_insertion("Hello world.");
        buffer.reset();
        assert_eq!(buffer.last_insertion(), None);
    }

    #[test]
    fn the_field_is_never_readable() {
        // Fabricating a snapshot from the record would defeat the purpose:
        // callers read snapshots precisely to check that belief.
        let clock = FakeClock::default();
        let buffer = buffer(&clock);
        buffer.record_insertion("Hello world.");
        assert!(buffer.read_field().is_none());
    }
}
