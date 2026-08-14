//! Microphone capture.
//!
//! `govox-py` calls `stream.read()` in a loop on a worker thread. cpal is
//! callback-driven instead, which is closer to what `ARCHITECTURE.md` always
//! claimed the design was. The callback does the minimum — downmix, resample,
//! send — and everything else happens on the receiving task, because a
//! callback that blocks is a callback that drops audio.
//!
//! Chime synthesis arrives with M7.

pub mod capture;
pub mod supervisor;

pub use capture::{CaptureError, DeviceInfo, MicrophoneCapture, list_devices};
pub use supervisor::{Backoff, CaptureSupervisor};
