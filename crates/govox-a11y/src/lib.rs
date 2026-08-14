//! AT-SPI focused-field reading over D-Bus.
//!
//! A rewrite rather than a port. `govox-py` uses libatspi through GObject
//! introspection — `Atspi.init()`, `get_desktop(0)`, `StateSet`, an
//! `EventListener` pumped by its own GLib loop — while the `atspi` crate is
//! pure zbus: a connection, a match rule and an async event stream. The shape
//! is a better fit for tokio than the Python is for asyncio, but none of the
//! call sequence carries over. What does carry over is every hard-won *rule*,
//! and those are what the comments here are about.
//!
//! Three of them are load-bearing:
//!
//! 1. **FOCUSED alone is not enough.** A text view in an inactive window reads
//!    perfectly and is not where the keystrokes are going. Confirming "delete
//!    that" against a window that will not receive the backspaces is worse than
//!    not reading at all — no snapshot beats the wrong snapshot. The search is
//!    scoped to the toplevel carrying ACTIVE.
//! 2. **gnome-shell must be skipped by name.** Its "Main stage" holds FOCUSED
//!    permanently and sorts first on the bus, so a first-match search returns
//!    it every time.
//! 3. **Bound the search by time, not by node count.** A 400-node cap was a
//!    shipped bug: Logseq's tree runs past 800 nodes, so the walk gave up
//!    before reaching the focused entry and reported "nothing readable" for an
//!    application that was exposing exactly what was wanted.
//!
//! Offsets here are *character* offsets, matching AT-SPI's own units and
//! `govox-core`'s `CharIdx`; never byte offsets.
//!
//! `[editing] read_focused_field` is off by default and the design contract is
//! that field access is an enhancement, never a dependency — so if this crate
//! never works, the dictation-buffer `TextModel` is a complete implementation
//! of the default configuration.

pub mod model;
pub mod reader;
pub mod tracker;

pub use model::AtspiTextModel;
pub use reader::FieldReader;
pub use tracker::FocusTracker;

/// Why the accessibility bus could not be used.
///
/// There is only one variant, and that is the point: every *other* failure —
/// an application that dies mid-read, a toolkit that exposes a broken node, a
/// tree too large to walk in budget — is reported as `None` from
/// [`govox_core::domain::TextModel::read_field`] rather than as an error,
/// because callers must treat all of them the same way.
#[derive(Debug, thiserror::Error)]
pub enum A11yError {
    /// The accessibility bus is not running, or refused the connection.
    ///
    /// Usually means accessibility is switched off for the session. `govox
    /// doctor` says so; dictation carries on with the buffer alone.
    #[error("the accessibility bus is unavailable: {0}")]
    NoBus(String),
}
