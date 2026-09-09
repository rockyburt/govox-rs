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
use govox_core::discovery::{
    BiasPlan, Candidates, DiscoverySpec, ProviderName, WatchSet, device_terms, plan_bias,
};
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

/// Load the hand-authored dictionary, then fold in what the machine says.
///
/// Two failure classes, deliberately different. The **file** is fatal, exactly
/// as [`load_dictionary`] describes: it is an instruction, and a typo in it
/// means govox is doing something the user did not ask for. The **machine** is
/// never fatal: a repository root that does not exist, an unreadable
/// `~/.ssh/config`, a checkout mid-rebase — none of those are instructions, and
/// none change what gets typed except by omission. Refusing to start because a
/// directory was missing would be the worst of both.
///
/// Returns the paths worth watching alongside the dictionary, so a clone or a
/// checkout can invalidate the answer without anything scanning the disk on the
/// session hot path.
///
/// # Errors
/// If the dictionary file cannot be read or does not parse.
pub fn load_dictionary_with_discovery(
    config: &Config,
) -> Result<LoadedDictionary, DictionaryLoadError> {
    let mut dictionary = load_dictionary(config)?;
    let Some(spec) = dictionary.discover.clone() else {
        return Ok(LoadedDictionary {
            dictionary,
            watch: WatchSet::default(),
            plan: BiasPlan::default(),
        });
    };

    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let extra = audio_device_candidates(config, &spec);
    let found = govox_discover::discover(&spec, home.as_deref(), &extra);

    let plan = plan_bias(
        &dictionary,
        &found.candidates,
        config.recognition.bias_prompt_token_budget,
    );
    report_overflow(&plan, config.recognition.bias_prompt_token_budget);
    tracing::info!(
        found = found.len(),
        biased = plan.terms.len(),
        words = plan.words,
        "planned the bias list"
    );
    dictionary.bias_terms = plan.terms.clone();

    Ok(LoadedDictionary {
        dictionary,
        watch: found.watch,
        plan,
    })
}

/// What a dictionary load produced.
///
/// A struct rather than a tuple because the third element is easy to mistake
/// for the second at a call site, and one of them decides what gets typed while
/// the other only decides what a menu says.
pub struct LoadedDictionary {
    pub dictionary: PersonalDictionary,
    /// Paths whose change should re-run discovery.
    pub watch: WatchSet,
    /// What the budget decided, for the About menu to read back.
    pub plan: BiasPlan,
}

/// Capture device names, which discovery cannot ask for itself.
///
/// `govox-discover` deliberately does not depend on `cpal`: doing so would make
/// every provider fail wherever the audio backend fails, for the sake of two
/// words. The one crate that already owns the backend supplies them instead.
fn audio_device_candidates(config: &Config, spec: &DiscoverySpec) -> Vec<Candidates> {
    if !spec.runs(ProviderName::AudioDevices) {
        return Vec::new();
    }
    let mut terms: Vec<String> = Vec::new();
    // Only the microphone actually in use, and the host's default — never the
    // whole enumeration.
    //
    // ALSA reports one card many times over: `hw:`, `plughw:`, `sysdefault:`,
    // `front:` and `dsnoop:` are the same microphone five ways. The list also
    // carries backend furniture whose *label is a sentence* — "Discard all
    // samples (playback) or generate zero samples (capture)". Splitting all of
    // it into words produced 29 terms on the development desk, most of them
    // ordinary English: `capture`, `playback`, `samples`, `zero`. Biasing those
    // is worse than biasing nothing, because it nudges the decoder toward
    // common words in every utterance and the damage is invisible — no error,
    // just a wrong transcript that reads as the model being bad.
    //
    // What anyone actually says out loud is the name of their microphone, and
    // that is the device they configured.
    let configured = config.audio.device.clone();
    let labels = std::iter::once(configured.clone()).chain(
        govox_audio::capture::list_devices()
            .into_iter()
            .filter(|device| device.is_default || device.id == configured)
            .map(|device| device.name),
    );
    for label in labels {
        for term in device_terms(&label) {
            if !terms.contains(&term) {
                terms.push(term);
            }
        }
    }
    vec![Candidates::new(ProviderName::AudioDevices, terms)]
}

/// Say what did not fit, since `bias_prompt` will not.
///
/// Truncation by word in list order is silent by design, which was fine while
/// the list was hand-written and small. A machine-sized list needs someone to
/// be able to answer "why is that word still coming out wrong", so the terms
/// that were dropped are named, along with the two knobs that would have kept
/// them.
fn report_overflow(plan: &BiasPlan, budget: u32) {
    if !plan.overflowed() {
        return;
    }
    const NAMED: usize = 12;
    let shown = plan
        .dropped
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let rest = plan.dropped.len().saturating_sub(NAMED);
    let tail = if rest > 0 {
        format!(", and {rest} more")
    } else {
        String::new()
    };
    tracing::warn!(
        "discovery found more terms than the {budget}-word bias budget holds; \
         {} were dropped: {shown}{tail}. Raise [recognition] bias_prompt_token_budget \
         or lower [dictionary.discover] max_repos.",
        plan.dropped.len(),
    );
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
