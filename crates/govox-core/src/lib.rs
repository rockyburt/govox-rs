//! Domain types, traits, config and the correction/editing pipeline.
//!
//! This crate deliberately depends on nothing that touches the operating
//! system: no tokio, no audio, no D-Bus, no windowing. Everything here is pure
//! logic that can be tested on any machine with no hardware and no desktop
//! session, which is what makes the differential parity harness against
//! `govox-py` cheap enough to run on every save.
//!
//! It also owns every trait the rest of the workspace implements
//! (`Recognizer`, `Corrector`, `Injector`, `PreeditSink`, `TextModel`, ...),
//! mirroring the `Protocol` definitions in `govox-py`'s `domain.py`. Sibling
//! crates depend on `govox-core` and never on each other; only `govox-daemon`
//! knows the concrete implementations exist.

pub mod activation;
pub mod audio;
pub mod caret;
pub mod config;
pub mod correction;
pub mod domain;
pub mod editing;
pub mod eval;
pub mod feedback;
pub mod keycodes;
pub mod logging;
pub mod reload;
pub mod streaming;
pub mod textmodel;
pub mod vad;

/// Character offset into a string, as distinct from a byte offset.
///
/// Every offset in the editing and span logic ported from `govox-py` is a
/// Python *code point* index — `("backspace",) * len(last)` counts code points,
/// and AT-SPI reports its own offsets in characters too. Rust's `str::len()` is
/// bytes, so mixing the two silently emits the wrong number of backspaces into
/// the user's document. This newtype exists so that mistake cannot be made by
/// accident: there is no `From<usize>`, only [`CharIdx::of`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CharIdx(usize);

impl CharIdx {
    /// The number of characters in `text` — never its byte length.
    #[must_use]
    pub fn of(text: &str) -> Self {
        Self(text.chars().count())
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }

    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_idx_counts_characters_not_bytes() {
        // The spoken-emoji table contains multi-byte and multi-code-point
        // entries; a byte count here would over-delete in the user's document.
        assert_eq!(CharIdx::of("hello").get(), 5);
        assert_eq!(CharIdx::of("café").get(), 4);
        assert_eq!("café".len(), 5, "byte length differs, which is the point");
        assert_eq!(CharIdx::of("🤷").get(), 1);
    }

    #[test]
    fn char_idx_counts_vs16_emoji_as_two_code_points() {
        // "❤️" is U+2764 U+FE0F. Python's len() reports 2, so the ported
        // backspace arithmetic must report 2 as well — matching govox-py's
        // behaviour exactly, including where that behaviour is arguably wrong.
        assert_eq!(CharIdx::of("\u{2764}\u{fe0f}").get(), 2);
    }
}
