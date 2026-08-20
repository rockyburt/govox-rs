//! IBus preedit engine over D-Bus.
//!
//! Dictation shown as underlined provisional text inside the focused field,
//! revised as Whisper revises it and committed once at the end. Preedit is
//! rendered by the application but is *not* in its document, so revising it is
//! a whole-string replace with nothing to verify and nothing to clobber — the
//! same mechanism macOS Dictation uses. It also reaches applications AT-SPI
//! reports as readable but not writable, Chrome among them.
//!
//! `govox-py` reaches IBus through PyGObject on a dedicated GLib main loop.
//! Here it is `zbus` against the raw interfaces, so there is no GLib loop in
//! the daemon at all — and, less obviously, no libibus, no GObject
//! introspection and no generated bindings. What that costs is the GVariant
//! layouts libibus builds for you, which are documented nowhere; see
//! [`variant`] for how each was recovered from the running system.
//!
//! Four behaviours here are load-bearing, each one a case where the API reports
//! success and does nothing:
//!
//! 1. **The synchronous engine switch deadlocks.** IBus has to call *back* into
//!    this process through our factory to start the engine, and a blocked loop
//!    cannot answer — a 15-second timeout returning failure, indistinguishable
//!    from GNOME refusing the engine. Every call here is `async` and under a
//!    timeout, so the failure mode is a logged degrade rather than a hang.
//! 2. **Register the component before anything resolves the engine by name**,
//!    and export the factory before that. See [`session::IbusSession::start`].
//! 3. **`PreeditFocusMode::COMMIT` makes the application commit a pending
//!    preedit on focus loss**, which typed literal command phrases into
//!    documents. It is not constructible in this crate — see
//!    [`variant::PreeditFocusMode`].
//! 4. **`RegisterComponent` returning OK proves nothing**, and
//!    `GetEnginesByNames` cannot confirm it. Only the factory callback can.
//!
//! While the engine is active it receives every key event in the focused field.
//! The handler returns immediately and never logs, counts by key, or retains
//! anything.

pub mod address;
pub mod engine;
pub mod session;
pub mod variant;

pub use engine::FieldState;
pub use session::{BUS_NAME, IbusSession};

/// Why an IBus engine could not be used.
///
/// Every one of these is a *degrade*, never a daemon failure: the caller's
/// fallback is "streaming behaves as it did before this existed", which is a
/// perfectly good outcome and not worth taking dictation down for.
#[derive(Debug, thiserror::Error)]
pub enum ImeError {
    /// No live `ibus-daemon` was found.
    #[error("ibus-daemon is not running: {0}")]
    NoDaemon(String),

    /// Another process already holds govox's bus name.
    ///
    /// Very nearly always the other govox: the Python reference claims the same
    /// name, which makes this the guard against both daemons driving the same
    /// engine during the parity period.
    #[error("another input method already owns govox's bus name ({0}); is govox-py running?")]
    NameTaken(String),

    /// A call to ibus-daemon did not return in time.
    #[error("ibus-daemon did not answer {0} in time")]
    Timeout(String),

    /// Anything the bus itself rejected.
    #[error("IBus D-Bus error: {0}")]
    Bus(#[from] zbus::Error),
}
