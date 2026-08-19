//! Pipeline orchestration, diagnostics and telemetry.
//!
//! The only crate that knows every other one exists.
//!
//! # Concurrency
//!
//! One tokio runtime and no GLib main loops — that is what reaching the tray,
//! IBus and AT-SPI over D-Bus buys, and it collapses `govox-py`'s three GLib
//! threads plus an asyncio loop into ordinary tasks.
//!
//! State is split in two, which is what removes `govox-py`'s
//! `mode_holder: list[Daemon]` construction cycle rather than emulating it:
//!
//! - [`SharedState`] is built **first** and handed to everyone. It carries the
//!   command-mode flag, the held-modifier set, and `ArcSwap` snapshots of the
//!   config, dictionary and correction pipeline.
//! - [`Daemon`] owns the pipeline state and is driven by exactly one task. It
//!   is never shared, so nothing needs a lock.
//!
//! Reload follows the same split: the *action* travels as a command message so
//! the swap happens on the owning task, and the *data* is published through
//! `ArcSwap` so readers are wait-free and each utterance sees one coherent
//! snapshot. `govox-py` instead rebinds attributes from the GLib tray thread
//! with no synchronisation, which is sound only because of the GIL.

pub mod daemon;
pub mod diagnostics;
pub mod feedback;
pub mod pipeline;
pub mod state;
pub mod watch;

/// The version of *this build*: the manifest version, plus the commit as
/// semver build metadata when this is not a tagged release.
///
/// `0.1.0` on the tag, `0.1.0+14.a18ad6e` fourteen commits later, and plain
/// `0.1.0` again where there is no repository to ask. Build metadata is ignored
/// for precedence, so the longer form ranks *equal* to the release rather than
/// below it — which `git describe`'s own `0.1.0-14-g…` shape would not, since
/// everything after the first `-` is a prerelease and sorts under its release.
///
/// Exported so `--version` and the tray's About read the same string. Two
/// version surfaces that disagree are worse than one that is vague. See this
/// crate's `build.rs`.
pub const BUILD_VERSION: &str = env!("GOVOX_BUILD_VERSION");

pub use daemon::{Announcer, Daemon, LogAnnouncer, Transcriber, begin_session, end_session};
pub use feedback::FeedbackChannel;
pub use pipeline::{PipelineError, run};
pub use state::SharedState;

use govox_core::config::Config;
use govox_core::domain::PersonalDictionary;

/// Load the personal dictionary, or fail loudly.
///
/// A dictionary that will not load is **fatal by design**, not something to
/// degrade around: it is text govox has been told to put in the user's
/// documents, and quietly dictating without it would be a silent wrong answer
/// rather than a missing feature. That makes it unlike the optional layers
/// (IBus, AT-SPI, the tray), which degrade precisely because their absence
/// changes nothing about what gets typed.
///
/// # Errors
/// If the file cannot be read or does not parse.
pub fn load_dictionary(config: &Config) -> Result<PersonalDictionary, DictionaryLoadError> {
    let path = config.correction.dictionary_path.trim();
    if path.is_empty() {
        return Ok(PersonalDictionary::default());
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    PersonalDictionary::load(std::path::Path::new(path), home.as_deref()).map_err(|source| {
        DictionaryLoadError {
            path: path.to_owned(),
            source: Box::new(source),
        }
    })
}

/// Reported like every other bad configuration — one line naming the file and
/// the problem — rather than a stack trace.
///
/// The source is boxed to keep this off the happy path's stack: it rides in the
/// `Err` arm of every startup result, and `DictionaryError` carries a `PathBuf`
/// and an `io::Error`.
#[derive(Debug, thiserror::Error)]
#[error("cannot use personal dictionary {path}: {source}")]
pub struct DictionaryLoadError {
    pub path: String,
    pub source: Box<govox_core::domain::DictionaryError>,
}
