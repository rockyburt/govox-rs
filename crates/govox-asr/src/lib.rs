//! Whisper recognition: model resolution, decoding, and the text tidy-up.
//!
//! The model runs on a dedicated thread that owns it outright — see
//! [`whisper`] for why a `Mutex` plus `spawn_blocking` would have been the
//! wrong shape.
//!
//! Two things here are compile-time where `govox-py` has them at runtime, and
//! both are guarded rather than papered over: the GPU backend (a cargo
//! feature, checked against `[recognition] device` at startup) and the model
//! catalogue (an explicit name → GGUF mapping, so the `distil-*` gap is a
//! clear error instead of a silent substitution).

pub mod model;
pub mod streaming;
pub mod text;
pub mod whisper;

pub use model::{ModelError, ResolvedModel, gguf_filename};
pub use streaming::{OnlineProcessor, StreamingUpdate};
pub use text::{bias_prompt, postprocess_text, whisper_language};
pub use whisper::{AsrError, Backend, WhisperHandle, WhisperRecognizer};
