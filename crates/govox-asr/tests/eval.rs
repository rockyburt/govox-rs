//! Accuracy eval: score the configured model against a recorded corpus.
//!
//! `CHANGELOG.md` has carried "word error rate is not measured" since the first
//! release. This is what measures it.
//!
//! ```text
//! tools/record-eval.sh                                        # once
//! cargo test -p govox-asr --test eval -- --ignored --nocapture
//! ```
//!
//! # What it is, and what it is not
//!
//! It is a **regression baseline for the configured model**, not a comparison
//! between models. It answers "did this get worse", and — through per-term
//! recall — "is the personal dictionary still earning its place". It does not
//! answer whether `large-v3-turbo` is worth its decode cost over `small.en`;
//! that needs a sweep, and a sweep is a loop around [`score_clip`] rather than
//! a rewrite, deliberately.
//!
//! # Two references per clip
//!
//! `say` is what was spoken and scores the **raw** recogniser output. `expect`
//! is what should land in the document and scores the **corrected** output.
//! They differ exactly where govox does work — "rentals dot ca" becomes
//! "rentals.ca", "comma" becomes "," — so the gap between the two scores is the
//! correction pipeline and the dictionary, measured rather than assumed.
//!
//! # Two decode paths
//!
//! By default this scores [`WhisperHandle::transcribe`] — one decode over the
//! whole clip. That is not what dictation runs. Set `GOVOX_EVAL_STREAMING=1` to
//! score the streaming path instead: an [`OnlineProcessor`] fed in frames,
//! decoding a growing window through `transcribe_words`, committing on
//! LocalAgreement-2. It is a different code path with different accuracy and
//! very different cost, and until this existed it was unmeasured.
//!
//! The streaming mode also reports **how many decodes** a clip took, because
//! the daemon's cadence is set by decode duration: `pipeline.rs` throttles to a
//! ~50% duty cycle, so a change that halves decode time does not halve total
//! cost — it doubles the number of decodes and halves the time between
//! captions. Both numbers are needed to read the trade.
//!
//! Streaming runs deliberately leave `corpus/eval/baseline.json` alone; that
//! file is the utterance-path record.
//!
//! # Ignored, and single-test
//!
//! `#[ignore]` because it needs a model and a recorded corpus, matching
//! `recognition.rs`. It is deliberately **one** test that loops, rather than one
//! test per clip: `recognition.rs` documents that whisper.cpp segfaults when two
//! threads initialise a GPU backend at once, and one test cannot race itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use govox_asr::streaming::OnlineProcessor;
use govox_asr::whisper::{WhisperHandle, WhisperRecognizer};
use govox_core::config::{Config, StreamingConfig};
use govox_core::correction::{Context, CorrectionPipeline};
use govox_core::domain::{AudioBuffer, PersonalDictionary};
use govox_core::eval;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Manifest {
    clip: Vec<Clip>,
}

#[derive(Debug, Deserialize)]
struct Clip {
    id: String,
    /// What was read aloud. Also the reference for the raw output.
    say: String,
    /// What should land in the document. Defaults to `say`.
    #[serde(default)]
    expect: Option<String>,
    #[serde(default)]
    terms: Vec<String>,
}

impl Clip {
    fn expected(&self) -> &str {
        self.expect.as_deref().unwrap_or(&self.say)
    }
}

fn repo_root() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Canonicalised so the skip message names a path someone can paste, rather
    // than one with `crates/govox-asr/../..` in the middle of it.
    path.canonicalize().unwrap_or(path)
}

fn manifest_path() -> PathBuf {
    repo_root().join("corpus/eval/manifest.toml")
}

fn audio_dir() -> PathBuf {
    repo_root().join("corpus/eval/audio")
}

/// Decode a 16-bit PCM WAV to mono f32 at 16 kHz.
///
/// The same minimal RIFF walk as `recognition.rs`'s `load_fixture`, taking a
/// path: enough for files this repository records itself, not a general parser.
fn load_wav(path: &Path) -> AudioBuffer {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));

    assert_eq!(&bytes[0..4], b"RIFF", "{} is not RIFF", path.display());
    assert_eq!(&bytes[8..12], b"WAVE", "{} is not WAVE", path.display());

    let mut channels = 0_u16;
    let mut sample_rate = 0_u32;
    let mut data: &[u8] = &[];
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body = &bytes[offset + 8..(offset + 8 + size).min(bytes.len())];
        match id {
            b"fmt " => {
                channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
                sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            }
            b"data" => data = body,
            _ => {}
        }
        offset += 8 + size + (size & 1);
    }
    assert!(!data.is_empty(), "{} has no data chunk", path.display());
    assert!(
        channels > 0 && sample_rate > 0,
        "{} has no fmt",
        path.display()
    );

    // Interleaved i16 -> mono f32, then nearest-neighbour down to 16 kHz. The
    // capture path does the same job with a real resampler; this is a fixture
    // loader and the difference does not survive Whisper's own front end.
    let channels = channels as usize;
    let mono: Vec<f32> = data
        .chunks_exact(2 * channels)
        .map(|frame| {
            let sum: i32 = (0..channels)
                .map(|c| i16::from_le_bytes([frame[2 * c], frame[2 * c + 1]]) as i32)
                .sum();
            (sum as f32 / channels as f32) / f32::from(i16::MAX)
        })
        .collect();

    let target = 16_000_u32;
    let samples: Vec<f32> = if sample_rate == target {
        mono
    } else {
        let ratio = f64::from(sample_rate) / f64::from(target);
        let out_len = (mono.len() as f64 / ratio) as usize;
        (0..out_len)
            .map(|i| mono[((i as f64) * ratio) as usize])
            .collect()
    };

    let duration = samples.len() as f64 / f64::from(target);
    AudioBuffer {
        samples: samples.into(),
        sample_rate: target,
        start_ts: 0.0,
        end_ts: duration,
    }
}

struct Scored {
    id: String,
    raw: String,
    corrected: String,
    raw_wer: f64,
    corrected_wer: f64,
    terms: Vec<(String, bool)>,
    seconds: f64,
    /// Decodes the clip cost. Always 1 on the utterance path.
    decodes: usize,
}

/// Whether to score the streaming path instead of the whole-utterance one.
fn streaming_mode() -> bool {
    std::env::var("GOVOX_EVAL_STREAMING").is_ok_and(|v| v == "1")
}

/// A fixed decode cadence, in seconds of audio, from `GOVOX_EVAL_CADENCE_S`.
///
/// Without it the schedule follows measured decode times, which is what the
/// daemon does — and which makes accuracy **irreproducible**: GPU jitter
/// changes how many decodes a clip gets, that changes the windows, and that
/// changes what LocalAgreement commits. Three identical baseline runs scored
/// 0.253, 0.247 and 0.248 with term recall 20, 19 and 21.
///
/// Pinning the cadence decouples the two questions. Accuracy is then exact and
/// comparable between builds, and decode *time* is still measured and reported
/// — it just no longer feeds back into the schedule. Compare accuracy with this
/// set, and speed with it unset.
fn fixed_cadence_s() -> Option<f64> {
    std::env::var("GOVOX_EVAL_CADENCE_S")
        .ok()
        .and_then(|v| v.parse().ok())
}

/// Run one clip through the streaming path the way the daemon runs it.
///
/// Returns the recognised text, the seconds spent decoding, and the number of
/// decodes.
///
/// The daemon does not decode on every frame: `pipeline.rs` awaits each decode
/// on the event loop and then skips further decodes for as long again, a ~50%
/// duty cycle that keeps a slow decode from starving the key that ends the
/// session. Reproducing that matters, because it is what makes decode *count* a
/// function of decode *speed* — decode twice as fast and the clip takes twice
/// as many decodes, each on a shorter window. Ignoring it would measure a
/// cadence no user ever sees.
///
/// It is modelled in **virtual audio time** rather than by sleeping: audio
/// reaches the daemon in real time, so seconds of audio pushed is the clock. A
/// decode starting at `t` and taking `took` blocks the next until `t + 2×took`.
///
/// The pre-speech gates (`MIN_VOICED_S`, the pre-roll) are not modelled. They
/// decide *when the first decode happens*, not how fast one is, and the corpus
/// clips are already trimmed to speech.
async fn stream_clip(
    handle: &WhisperHandle,
    streaming: &StreamingConfig,
    audio: &AudioBuffer,
) -> (String, f64, usize) {
    let mut processor = OnlineProcessor::new(handle.clone(), streaming, audio.sample_rate);
    // 20 ms, the granularity capture delivers at.
    let frame = (audio.sample_rate as usize / 50).max(1);

    let mut text = String::new();
    let mut clock_s = 0.0_f64;
    let mut next_decode_s = 0.0_f64;
    let mut seconds = 0.0_f64;
    let mut decodes = 0_usize;

    for chunk in audio.samples.chunks(frame) {
        processor.push(chunk);
        clock_s += chunk.len() as f64 / f64::from(audio.sample_rate);
        if processor.ready() && clock_s >= next_decode_s {
            let started = std::time::Instant::now();
            let update = processor.process().await.expect("streaming decode");
            let took = started.elapsed().as_secs_f64();
            seconds += took;
            decodes += 1;
            text.push_str(&update.committed);
            next_decode_s = clock_s + fixed_cadence_s().unwrap_or(2.0 * took);
        }
    }

    // `decode_tail` is true because a corpus clip ends on the last word rather
    // than on the silence of someone reaching for a key.
    let started = std::time::Instant::now();
    text.push_str(&processor.finish(true).await);
    seconds += started.elapsed().as_secs_f64();
    decodes += 1;

    (text, seconds, decodes)
}

#[tokio::test]
#[ignore = "needs the configured model and a recorded corpus; see tools/record-eval.sh"]
async fn scores_the_configured_model_against_the_eval_corpus() {
    let manifest: Manifest = toml::from_str(
        &std::fs::read_to_string(manifest_path()).expect("the eval manifest is checked in"),
    )
    .expect("the eval manifest parses");

    // The corpus audio is gitignored, so "not recorded yet" is the normal state
    // on a fresh clone and must not read as a failure. Naming the script is the
    // whole point: a skip that does not say how to un-skip itself is noise.
    let missing: Vec<&str> = manifest
        .clip
        .iter()
        .filter(|c| !audio_dir().join(format!("{}.wav", c.id)).is_file())
        .map(|c| c.id.as_str())
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "SKIPPING: {} of {} clips have no audio in {}.\n\
             The recordings are deliberately not in git (public repository).\n\
             Record them with:  tools/record-eval.sh\n\
             Missing: {}",
            missing.len(),
            manifest.clip.len(),
            audio_dir().display(),
            missing.join(", ")
        );
        return;
    }

    let config = Config::load(None).expect("the machine's own configuration loads");
    let dictionary = load_dictionary(&config);
    let pipeline = CorrectionPipeline::new(
        config.correction.clone(),
        dictionary.clone(),
        config.editing.command_mode,
    );

    let recognizer = WhisperRecognizer::start(&config.recognition, &dictionary, 4)
        .expect("the configured recognizer starts");
    let handle = recognizer.handle();
    handle.warm_up().await.expect("the configured model loads");

    let mut scored = Vec::new();
    for clip in &manifest.clip {
        let audio = load_wav(&audio_dir().join(format!("{}.wav", clip.id)));
        let (raw, seconds, decodes) = if streaming_mode() {
            stream_clip(&handle, &config.streaming, &audio).await
        } else {
            let started = std::time::Instant::now();
            let raw = handle.transcribe(&audio).await.expect("transcribes");
            (raw, started.elapsed().as_secs_f64(), 1)
        };

        // Default context: no field purpose and no preceding text, which is the
        // prose path — the same one an ordinary dictation into a document takes.
        let corrected = pipeline.correct(&raw, &Context::default()).corrected_text;

        scored.push(Scored {
            id: clip.id.clone(),
            raw_wer: eval::word_error_rate(&clip.say, &raw),
            corrected_wer: eval::word_error_rate(clip.expected(), &corrected),
            terms: eval::term_recall(&corrected, &clip.terms)
                .into_iter()
                .map(|(t, hit)| (t.to_owned(), hit))
                .collect(),
            raw,
            corrected,
            seconds,
            decodes,
        });
    }

    report(&config, &scored);
    if streaming_mode() {
        eprintln!(
            "baseline.json not written: it is the utterance-path record, and this was a \
             streaming run.\n"
        );
    } else {
        write_baseline(&config, &scored);
    }
}

/// The manifest is checked in, so it can be wrong in git without anyone
/// noticing until the next recording session. **Not** `#[ignore]`d: it needs no
/// model and no audio, so CI can hold the corpus to its own rules.
#[test]
fn the_eval_manifest_is_well_formed() {
    let manifest: Manifest = toml::from_str(
        &std::fs::read_to_string(manifest_path()).expect("the eval manifest is checked in"),
    )
    .expect("the eval manifest parses");

    assert!(
        manifest.clip.len() >= 20,
        "the corpus should not shrink quietly"
    );

    let mut seen = std::collections::BTreeSet::new();
    for clip in &manifest.clip {
        assert!(
            seen.insert(clip.id.as_str()),
            "duplicate clip id {:?}: the second recording would overwrite the first",
            clip.id
        );
        assert!(
            clip.id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "clip id {:?} becomes a filename; keep it boring",
            clip.id
        );
        assert!(
            !clip.say.trim().is_empty(),
            "{} has nothing to say",
            clip.id
        );

        // The authoring bug this catches: a clip that declares a term its own
        // reference does not contain can never pass, however good the model is.
        for (term, present) in eval::term_recall(clip.expected(), &clip.terms) {
            assert!(
                present,
                "{}: term {term:?} does not appear in its own reference {:?}",
                clip.id,
                clip.expected()
            );
        }
    }
}

/// The spoken-punctuation clips assert that "comma" becomes "," — but that is
/// govox's own behaviour, testable here with no model and no audio.
///
/// Worth pinning separately, because a wrong `expect` is invisible until a
/// recording session and then shows up as a permanent error the model cannot
/// fix. This proves the target is reachable before anyone speaks into a
/// microphone.
///
/// Only the clips whose transformation is the *pipeline's* — the dictionary
/// ones depend on `~/.config/govox/dictionary.toml`, which is not in this
/// repository and must not decide whether CI passes.
#[test]
fn the_spoken_punctuation_targets_are_reachable_without_a_model() {
    let manifest: Manifest =
        toml::from_str(&std::fs::read_to_string(manifest_path()).expect("manifest is checked in"))
            .expect("manifest parses");

    let config = Config::load_from(None, &govox_core::config::Environment::default())
        .expect("defaults are valid");
    let pipeline = CorrectionPipeline::new(
        config.correction.clone(),
        PersonalDictionary::default(),
        false,
    );

    let mut checked = 0;
    for clip in manifest.clip.iter().filter(|c| c.id.starts_with("spoken-")) {
        let corrected = pipeline
            .correct(&clip.say, &Context::default())
            .corrected_text;
        assert_eq!(
            eval::normalize_for_scoring(&corrected),
            eval::normalize_for_scoring(clip.expected()),
            "{}: correcting {:?} did not reach its own target",
            clip.id,
            clip.say
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "the sweep proved nothing if it checked nothing"
    );
}

fn load_dictionary(config: &Config) -> PersonalDictionary {
    let path = config.correction.dictionary_path.trim();
    if path.is_empty() {
        return PersonalDictionary::default();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    PersonalDictionary::load(Path::new(path), home.as_deref())
        .expect("the configured personal dictionary loads")
}

fn report(config: &Config, scored: &[Scored]) {
    eprintln!(
        "\nmodel={}  clips={}  path={}",
        config.recognition.model,
        scored.len(),
        if streaming_mode() {
            "streaming (transcribe_words)"
        } else {
            "utterance (transcribe)"
        }
    );
    if let Some(cadence) = fixed_cadence_s() {
        eprintln!(
            "decode cadence pinned at {cadence:.2}s of audio: accuracy is reproducible, \
             decode times are still real but do not drive the schedule"
        );
    }
    eprintln!(
        "\n{:<28} {:>8} {:>10} {:>8} {:>5}  terms",
        "clip", "raw wer", "corr. wer", "secs", "dec"
    );
    for s in scored {
        let terms = if s.terms.is_empty() {
            "-".to_owned()
        } else {
            s.terms
                .iter()
                .map(|(t, hit)| format!("{}{t}", if *hit { "✓" } else { "✗" }))
                .collect::<Vec<_>>()
                .join(" ")
        };
        eprintln!(
            "{:<28} {:>8.3} {:>10.3} {:>8.2} {:>5}  {terms}",
            s.id, s.raw_wer, s.corrected_wer, s.seconds, s.decodes
        );
        // The text only when it did not come out clean: a clean run should be
        // readable at a glance, and a wall of correct transcriptions hides the
        // two lines worth looking at.
        if s.corrected_wer > 0.0 {
            eprintln!("{:<28}   got: {:?}", " ", s.corrected);
        }
    }

    let mean = |f: fn(&Scored) -> f64| scored.iter().map(f).sum::<f64>() / scored.len() as f64;
    let hits: Vec<&(String, bool)> = scored.iter().flat_map(|s| s.terms.iter()).collect();
    let recalled = hits.iter().filter(|(_, hit)| *hit).count();

    let total_decodes: usize = scored.iter().map(|s| s.decodes).sum();
    eprintln!(
        "\naggregate: raw WER {:.3}   corrected WER {:.3}   mean decode time {:.2}s per clip",
        mean(|s| s.raw_wer),
        mean(|s| s.corrected_wer),
        mean(|s| s.seconds)
    );
    if streaming_mode() {
        // Per-decode cost is the number the daemon's caption cadence follows;
        // per-clip cost is the number a GPU bill follows. They move in opposite
        // directions when the window shrinks, so print both.
        eprintln!(
            "decodes: {total_decodes} over {} clips ({:.1} per clip, {:.3}s each)",
            scored.len(),
            total_decodes as f64 / scored.len() as f64,
            scored.iter().map(|s| s.seconds).sum::<f64>() / total_decodes as f64,
        );
    }
    if hits.is_empty() {
        eprintln!("term recall: no terms declared");
    } else {
        eprintln!("term recall: {recalled}/{} ", hits.len());
        let missed: Vec<&str> = hits
            .iter()
            .filter(|(_, hit)| !*hit)
            .map(|(t, _)| t.as_str())
            .collect();
        if !missed.is_empty() {
            eprintln!("missed: {}", missed.join(", "));
        }
    }

    // The number that says whether the dictionary is worth its file. If it is
    // zero, every rule in it is either dead or no longer needed — worth knowing
    // either way, and invisible without measuring both sides.
    let gap = mean(|s| s.raw_wer) - mean(|s| s.corrected_wer);
    eprintln!("correction + dictionary closed {gap:.3} WER\n");
}

fn write_baseline(config: &Config, scored: &[Scored]) {
    let clips: Vec<serde_json::Value> = scored
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "raw": s.raw,
                "corrected": s.corrected,
                "raw_wer": (s.raw_wer * 1000.0).round() / 1000.0,
                "corrected_wer": (s.corrected_wer * 1000.0).round() / 1000.0,
                "terms": s.terms.iter().map(|(t, hit)| (t.clone(), *hit)).collect::<BTreeMap<String, bool>>(),
            })
        })
        .collect();

    let mean = |f: fn(&Scored) -> f64| {
        let m = scored.iter().map(f).sum::<f64>() / scored.len() as f64;
        (m * 1000.0).round() / 1000.0
    };
    // Decode times are deliberately **not** written here. They are a property of
    // whichever GPU was idle at the time, and a committed number that moves on
    // its own trains the reader to ignore the file. `times_the_configured_model`
    // is where timing lives.
    let baseline = serde_json::json!({
        "model": config.recognition.model,
        "clips": clips,
        "raw_wer": mean(|s| s.raw_wer),
        "corrected_wer": mean(|s| s.corrected_wer),
    });

    let path = repo_root().join("corpus/eval/baseline.json");
    let mut text = serde_json::to_string_pretty(&baseline).expect("baseline serialises");
    text.push('\n');
    std::fs::write(&path, text).expect("baseline is writable");
    eprintln!("wrote {}", path.display());
}
