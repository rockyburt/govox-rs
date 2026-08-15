//! evdev hotkeys, ydotool and clipboard injection.
//!
//! Arrives in M3 (injection) and M4 (hotkeys).
//!
//! Keyboards are *observed*, never grabbed. The injectors shell out to
//! `ydotool` and `wl-copy` exactly as `govox-py` does, behind a `Runner` trait
//! so the tests can assert on exact argv without a desktop session.
//!
//! `ydotool key` is only ever given raw keycodes. Passing it a key *name* exits
//! 0 and presses nothing — a silent no-op no return code catches — so the
//! keycode table in `govox-core` yields a `KeyCode` newtype that makes the
//! name-passing mistake unrepresentable rather than merely tested against.

pub mod clipboard;
pub mod evdev_listener;
pub mod runner;
pub mod selector;
pub mod ydotool;

pub use clipboard::ClipboardInjector;
pub use runner::{CommandResult, ProcessRunner, RecordingRunner, Runner};
pub use selector::{
    FallbackInjector, InjectionReport, Notify, SilentNotify, UsedBackend, select_injector,
};
pub use ydotool::YdotoolInjector;
