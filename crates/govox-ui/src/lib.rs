//! Tray, notifications and the overlay client.
//!
//! Chimes are modelled on **macOS Dictation** rather than `govox-py`: two
//! discrete pitched notes with a bell envelope, not a frequency sweep. See
//! [`chime`] for why, and `docs/parity.md` for the record of the divergence.
//!
//! The tray is `ksni` — StatusNotifierItem over D-Bus — which deletes GTK3,
//! AyatanaAppIndicator3, a GLib main loop and one of `govox-py`'s three
//! `sys.path` bridging hacks. Icons stay freedesktop symbolic names, so there
//! are still no shipped image assets.
//!
//! Notifications are a deliberate divergence: `govox-py` declares a
//! `NotifyBackend` protocol but hardcodes a null implementation, so every
//! `notify()` call in it today is dead. Here they are actually delivered.
//!
//! This crate owns only the *client* side of the overlay — spawning the helper
//! and speaking its newline-delimited text protocol (`show`/`pulse`/`hide`/
//! `level`/`caption`/`anchor`/`expect-anchor`/`caret-marker`/`compact`/`quit`
//! out, `stop` in). The renderer is a separate process, so an overlay crash
//! cannot take dictation down.

pub mod chime;
pub mod notify;
pub mod overlay;
pub mod tray;

pub use chime::{Chime, PlaySink, RodioSink, SilentSink};
pub use notify::{DesktopNotifier, LogNotifier, Notifier, NullNotifier};
pub use overlay::{OverlayClient, OverlayCommand, OverlaySink};
pub use tray::{Tray, TrayCommand};
