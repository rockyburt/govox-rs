//! Finding the GGUF model file, and deciding whether we may go to the network.
//!
//! `govox-py` names models the way faster-whisper does (`small`,
//! `large-v3-turbo`) and lets CTranslate2 resolve them to a Hugging Face repo.
//! whisper.cpp wants a single `ggml-*.bin`, so the mapping is explicit here —
//! which is a gain, because it makes the one real gap visible: the `distil-*`
//! family has no GGUF build, and a user configured for it must be told so
//! rather than silently given a different model.

use std::path::{Path, PathBuf};

use govox_core::config::{DownloadPolicy, RecognitionConfig};
use whisper_rs::DtwModelPreset;

/// The repo that publishes whisper.cpp's GGUF conversions.
const GGUF_REPO_OWNER: &str = "ggerganov";
const GGUF_REPO_NAME: &str = "whisper.cpp";

/// Every model govox can load, with its DTW alignment preset.
///
/// The preset is not optional detail: DTW is chosen when the *context* is
/// built, not per call, and getting it wrong is silent — word timestamps come
/// back as zeros and streaming quietly commits at the wrong boundaries.
const MODELS: &[(&str, DtwModelPreset)] = &[
    ("tiny", DtwModelPreset::Tiny),
    ("tiny.en", DtwModelPreset::TinyEn),
    ("base", DtwModelPreset::Base),
    ("base.en", DtwModelPreset::BaseEn),
    ("small", DtwModelPreset::Small),
    ("small.en", DtwModelPreset::SmallEn),
    ("medium", DtwModelPreset::Medium),
    ("medium.en", DtwModelPreset::MediumEn),
    ("large-v1", DtwModelPreset::LargeV1),
    ("large-v2", DtwModelPreset::LargeV2),
    ("large-v3", DtwModelPreset::LargeV3),
    ("large-v3-turbo", DtwModelPreset::LargeV3Turbo),
];

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error(
        "model {name:?} has no whisper.cpp GGUF build. \
         Known models: {known}. \
         The distil-* family is CTranslate2-only and has no equivalent here; \
         `large-v3-turbo` is the closest fast alternative."
    )]
    Unknown { name: String, known: String },

    #[error(
        "model {name:?} is not in the local cache and [recognition] \
         download_policy is \"offline\". Set it to \"cache_first\" to fetch it \
         once, or point [recognition] model_dir at an existing ggml-*.bin."
    )]
    NotCached { name: String },

    #[error("[recognition] model_dir {path} does not contain a readable model file")]
    BadModelDir { path: PathBuf },

    #[error("could not fetch model {name:?} from Hugging Face: {source}")]
    Fetch {
        name: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// A model located on disk, ready to load.
///
/// No `PartialEq`: `DtwModelPreset` derives only `Debug` and `Clone`, and
/// comparing presets by their `Debug` rendering (as the tests do) is a test
/// affordance, not something callers should rely on.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub path: PathBuf,
    pub dtw_preset: DtwModelPreset,
    /// The configured name, for logging.
    pub name: String,
}

/// The GGUF filename for a model name, if one exists.
#[must_use]
pub fn gguf_filename(name: &str) -> Option<String> {
    known_model(name).map(|_| format!("ggml-{name}.bin"))
}

fn known_model(name: &str) -> Option<DtwModelPreset> {
    MODELS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, preset)| preset.clone())
}

fn known_names() -> String {
    MODELS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Locate the model file named by `config`, downloading it if policy allows.
///
/// Blocking, and it may do network I/O. Call it from the recogniser's own
/// thread, never from a tokio worker.
///
/// # Errors
/// If the model has no GGUF build, is absent under an offline policy, or the
/// download fails.
pub fn resolve(config: &RecognitionConfig) -> Result<ResolvedModel, ModelError> {
    let name = config.model.trim();
    let preset = known_model(name).ok_or_else(|| ModelError::Unknown {
        name: name.to_owned(),
        known: known_names(),
    })?;

    // An explicit model_dir wins outright and never touches the network: it is
    // how a user pins a hand-converted or quantised model.
    if !config.model_dir.trim().is_empty() {
        let path = resolve_model_dir(Path::new(config.model_dir.trim()), name)?;
        return Ok(ResolvedModel {
            path,
            dtw_preset: preset,
            name: name.to_owned(),
        });
    }

    let filename = format!("ggml-{name}.bin");
    let path = fetch(&filename, name, config.download_policy)?;
    Ok(ResolvedModel {
        path,
        dtw_preset: preset,
        name: name.to_owned(),
    })
}

/// Accept either a path straight to the `.bin` or a directory holding one.
fn resolve_model_dir(dir: &Path, name: &str) -> Result<PathBuf, ModelError> {
    if dir.is_file() {
        return Ok(dir.to_path_buf());
    }
    let candidate = dir.join(format!("ggml-{name}.bin"));
    if candidate.is_file() {
        return Ok(candidate);
    }
    Err(ModelError::BadModelDir {
        path: dir.to_path_buf(),
    })
}

fn fetch(filename: &str, name: &str, policy: DownloadPolicy) -> Result<PathBuf, ModelError> {
    let repo = hf_hub::HFClientSync::new()
        .map_err(|e| ModelError::Fetch {
            name: name.to_owned(),
            source: Box::new(e),
        })?
        .model(GGUF_REPO_OWNER, GGUF_REPO_NAME);

    let cached = |local_only: bool| {
        repo.download_file()
            .filename(filename.to_owned())
            .local_files_only(local_only)
            .send()
    };

    match policy {
        // Never touch the network. A missing model is a clear, actionable
        // error rather than a startup that hangs on a DNS timeout.
        DownloadPolicy::Offline => cached(true).map_err(|_| ModelError::NotCached {
            name: name.to_owned(),
        }),

        // Go straight to the hub, letting it revalidate.
        DownloadPolicy::Allow => cached(false).map_err(|e| ModelError::Fetch {
            name: name.to_owned(),
            source: Box::new(e),
        }),

        // Prefer the cached copy, which skips the revision check — a network
        // round-trip on every startup that also hangs when offline. Only fall
        // through to a download when the cache genuinely has nothing.
        DownloadPolicy::CacheFirst => match cached(true) {
            Ok(path) => Ok(path),
            Err(_) => {
                tracing::info!(
                    model = name,
                    "not in local cache; downloading from Hugging Face"
                );
                cached(false).map_err(|e| ModelError::Fetch {
                    name: name.to_owned(),
                    source: Box::new(e),
                })
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_model_name_maps_to_a_gguf_file() {
        for (name, _) in MODELS {
            assert_eq!(
                gguf_filename(name).as_deref(),
                Some(format!("ggml-{name}.bin").as_str())
            );
        }
    }

    /// `DtwModelPreset` derives only `Debug`, so its rendering is the only
    /// handle a test has on which preset came back.
    fn preset_of(name: &str) -> Option<String> {
        known_model(name).map(|preset| format!("{preset:?}"))
    }

    #[test]
    fn the_configured_model_is_supported() {
        // The reference install runs `small`. If that ever stops resolving,
        // the port cannot replace govox-py on this machine.
        assert!(gguf_filename("small").is_some());
        assert_eq!(preset_of("small").as_deref(), Some("Small"));
    }

    #[test]
    fn the_distil_family_is_rejected_with_a_usable_message() {
        // The one real model-availability gap, and it must not degrade into
        // silently loading something else.
        assert_eq!(gguf_filename("distil-large-v3"), None);

        let error = ModelError::Unknown {
            name: "distil-large-v3".to_owned(),
            known: known_names(),
        };
        let message = error.to_string();
        assert!(message.contains("distil-*"), "must name the family");
        assert!(
            message.contains("large-v3-turbo"),
            "must suggest an alternative"
        );
    }

    #[test]
    fn english_only_variants_get_english_only_presets() {
        // Mixing these up is silent: word timestamps come back as zeros.
        assert_eq!(preset_of("small.en").as_deref(), Some("SmallEn"));
        assert_eq!(preset_of("small").as_deref(), Some("Small"));
        assert_ne!(preset_of("small.en"), preset_of("small"));
    }

    #[test]
    fn every_model_has_a_distinct_preset() {
        // A copy-paste slip in the table would pair a model with another
        // model's alignment heads, which produces plausible-looking but wrong
        // word timestamps rather than an error.
        let mut presets: Vec<String> = MODELS
            .iter()
            .map(|(_, preset)| format!("{preset:?}"))
            .collect();
        let total = presets.len();
        presets.sort();
        presets.dedup();
        assert_eq!(presets.len(), total, "two models share a DTW preset");
    }

    #[test]
    fn an_unknown_name_lists_what_is_available() {
        let error = ModelError::Unknown {
            name: "enormous".to_owned(),
            known: known_names(),
        };
        let message = error.to_string();
        assert!(message.contains("small"));
        assert!(message.contains("large-v3-turbo"));
    }

    #[test]
    fn an_offline_miss_says_how_to_fix_it() {
        let message = ModelError::NotCached {
            name: "small".to_owned(),
        }
        .to_string();
        assert!(message.contains("cache_first"));
        assert!(message.contains("model_dir"));
    }

    #[test]
    fn a_model_dir_accepts_a_file_or_its_directory() {
        let dir = std::env::temp_dir().join(format!("govox-model-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("ggml-small.bin");
        std::fs::write(&file, b"not really a model").expect("write");

        assert_eq!(resolve_model_dir(&dir, "small").unwrap(), file);
        assert_eq!(resolve_model_dir(&file, "small").unwrap(), file);
        // A directory with no matching model is an error, not a silent miss.
        assert!(resolve_model_dir(&dir, "medium").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}
