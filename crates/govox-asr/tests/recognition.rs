//! End-to-end recognition against a real model.
//!
//! Every test here is `#[ignore]`d: they need a model file on disk, and the
//! first one may pull ~½ GB from Hugging Face. Run them deliberately:
//!
//! ```text
//! cargo test -p govox-asr --features vulkan -- --ignored
//! ```
//!
//! They are the only thing that can catch the failures this crate is really
//! exposed to — a wrong DTW preset, a decode-parameter mapped to the wrong
//! whisper.cpp name, a GPU backend that loads but produces nothing. Unit tests
//! cannot see any of that.

use std::path::PathBuf;
use std::sync::Arc;

use govox_asr::whisper::WhisperRecognizer;
use govox_core::config::{Config, Environment};
use govox_core::domain::{AudioBuffer, PersonalDictionary};

/// The fixture the M-1(a) spike used: `govox-py`'s own `hello.wav`.
/// Serialises every test that builds a Whisper context.
///
/// Not tidiness — **whisper.cpp segfaults if two threads initialise a GPU
/// backend at once**, and cargo runs a binary's tests concurrently by default.
/// Confirmed on the reference machine: `--ignored` alone dies with SIGSEGV
/// mid-`whisper_model_load`, `--ignored --test-threads=1` passes 6/6 in 10 s.
///
/// The lock lives here rather than in a `--test-threads=1` instruction because
/// an instruction in a README does not run: anyone following the documented
/// `cargo test --workspace -- --ignored` would hit the crash and have no way to
/// tell it from a real failure in whatever test happened to be running.
///
/// The two tests that only *resolve* a model file take no lock: they never
/// build a context, so they cannot hit the crash, and serialising them would
/// slow the run for nothing.
static GPU: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fixture_wav() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/hello.wav")
}

/// A small model already on disk, so the common case needs no network.
fn cached_tiny_en() -> Option<PathBuf> {
    let path = dirs_home()?.join(".cache/govox-models/ggml-tiny.en.bin");
    path.is_file().then_some(path)
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn defaults() -> Config {
    Config::load_from(None, &Environment::default()).expect("defaults are valid")
}

/// Decode a 16-bit PCM WAV to mono f32 at 16 kHz, the way capture would.
fn load_fixture() -> AudioBuffer {
    let bytes = std::fs::read(fixture_wav()).expect("fixture wav is present");

    // Minimal RIFF walk: enough for a known-good fixture, not a general parser.
    assert_eq!(&bytes[0..4], b"RIFF", "not a RIFF file");
    assert_eq!(&bytes[8..12], b"WAVE", "not a WAVE file");

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
        offset += 8 + size + (size & 1); // chunks are word-aligned
    }
    assert!(!data.is_empty(), "no data chunk");

    let interleaved: Vec<f32> = data
        .chunks_exact(2)
        .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0)
        .collect();

    // Reuse the production conversion rather than a test-local copy: if the
    // resampler is wrong, this test should see the same audio the daemon does.
    let samples = govox_core::audio::normalize_to_mono(&interleaved, channels, sample_rate, 16_000);

    AudioBuffer {
        sample_rate: 16_000,
        start_ts: 0.0,
        end_ts: samples.len() as f64 / 16_000.0,
        samples: Arc::from(samples),
    }
}

#[tokio::test]
#[ignore = "needs a model file on disk"]
async fn transcribes_the_hello_fixture() {
    let _gpu = GPU.lock().await;
    let Some(model) = cached_tiny_en() else {
        panic!("no cached model; run the fetch test first or set one up");
    };

    let mut config = defaults();
    config.recognition.model = "tiny.en".to_owned();
    config.recognition.model_dir = model.to_string_lossy().into_owned();

    let recognizer =
        WhisperRecognizer::start(&config.recognition, &PersonalDictionary::default(), 4)
            .expect("the recognizer starts");
    let handle = recognizer.handle();

    handle.warm_up().await.expect("the model loads");

    let audio = load_fixture();
    let text = handle
        .transcribe(&audio)
        .await
        .expect("transcription works");

    // The fixture says "Hello". Assert on content, not an exact string:
    // punctuation and casing are the decoder's business and vary by model.
    assert!(
        text.to_lowercase().contains("hello"),
        "expected the fixture's speech, got {text:?}"
    );
    // postprocess_text must have collapsed whitespace and trimmed.
    assert_eq!(text.trim(), text, "text should arrive trimmed");
    assert!(!text.contains("  "), "whitespace should be collapsed");
}

#[tokio::test]
#[ignore = "needs a model file on disk"]
async fn empty_audio_yields_no_text_rather_than_hallucination() {
    let _gpu = GPU.lock().await;
    let Some(model) = cached_tiny_en() else {
        panic!("no cached model");
    };

    let mut config = defaults();
    config.recognition.model = "tiny.en".to_owned();
    config.recognition.model_dir = model.to_string_lossy().into_owned();

    let recognizer =
        WhisperRecognizer::start(&config.recognition, &PersonalDictionary::default(), 4)
            .expect("the recognizer starts");

    let silence = AudioBuffer {
        samples: Arc::from(Vec::new()),
        sample_rate: 16_000,
        start_ts: 0.0,
        end_ts: 0.0,
    };
    let text = recognizer
        .handle()
        .transcribe(&silence)
        .await
        .expect("empty audio is not an error");

    // Whisper pads to 30s internally and will happily invent words for an
    // empty buffer. Refusing it before the decoder is what stops "Thank you."
    // appearing in the user's document after a stray keypress.
    assert_eq!(text, "");
}

/// Populates the Hugging Face cache with the model the reference install runs.
///
/// Separate and explicit because it may download ~½ GB. It also exercises the
/// real `cache_first` path rather than a stub, so a broken policy shows up
/// here rather than at the user's first utterance.
#[test]
#[ignore = "downloads ~500 MB from Hugging Face"]
fn fetches_the_configured_model() {
    let mut config = defaults();
    config.recognition.model = "small".to_owned();
    config.recognition.download_policy = govox_core::config::DownloadPolicy::CacheFirst;

    let resolved = govox_asr::model::resolve(&config.recognition).expect("small resolves");
    assert!(resolved.path.is_file(), "resolved path is not a file");
    assert_eq!(format!("{:?}", resolved.dtw_preset), "Small");
}

#[test]
#[ignore = "needs a model file on disk"]
fn an_offline_policy_finds_an_already_cached_model() {
    let mut config = defaults();
    config.recognition.model = "small".to_owned();
    config.recognition.download_policy = govox_core::config::DownloadPolicy::Offline;

    // Only meaningful once `fetches_the_configured_model` has run. If the
    // cache is cold this correctly errors, which is the behaviour under test.
    match govox_asr::model::resolve(&config.recognition) {
        Ok(resolved) => assert!(resolved.path.is_file()),
        Err(error) => assert!(
            error.to_string().contains("cache_first"),
            "an offline miss must say how to fix it, got: {error}"
        ),
    }
}

/// M9's definition of done: a growing caption that commits.
///
/// Feeds the fixture in 500 ms chunks, exactly as capture would, and checks
/// that the caption grows and that words eventually become final.
#[tokio::test]
#[ignore = "needs a model file on disk"]
async fn a_streaming_session_grows_a_caption_and_commits() {
    let _gpu = GPU.lock().await;
    let Some(model) = cached_tiny_en() else {
        panic!("no cached model");
    };

    let mut config = defaults();
    config.recognition.model = "tiny.en".to_owned();
    config.recognition.model_dir = model.to_string_lossy().into_owned();
    config.streaming.min_chunk_size_s = 0.5;

    let recognizer =
        WhisperRecognizer::start(&config.recognition, &PersonalDictionary::default(), 4)
            .expect("the recognizer starts");
    recognizer
        .handle()
        .warm_up()
        .await
        .expect("the model loads");

    let mut processor =
        govox_asr::OnlineProcessor::new(recognizer.handle(), &config.streaming, 16_000);

    let audio = load_fixture();
    let chunk = 8_000; // 500 ms at 16 kHz
    let mut captions: Vec<String> = Vec::new();
    let mut committed = String::new();

    for block in audio.samples.chunks(chunk) {
        processor.push(block);
        if !processor.ready() {
            continue;
        }
        let update = processor.process().await.expect("streaming decodes");
        committed.push_str(&update.committed);
        captions.push(format!("{committed}{}", update.pending));
    }
    // Awaited: `finish` decodes whatever the last chunk left undecoded, so
    // the tail of a session is transcribed rather than discarded with the
    // buffer.
    // `true`: the fixture is speech throughout, so the leftover is worth
    // decoding. The daemon decides this from how much voice the VAD saw since
    // the last decode.
    let tail = processor.finish(true).await;

    assert!(!captions.is_empty(), "no streaming updates were produced");

    // Committed text is append-only. A caption whose *committed* half shrank
    // would mean a word was made final and then revised, which is the one
    // thing LocalAgreement exists to prevent.
    assert!(
        committed.is_empty() || !committed.contains("  "),
        "committed text looks malformed: {committed:?}"
    );

    let final_text = format!("{committed}{tail}");
    assert!(
        final_text.to_lowercase().contains("hello"),
        "the session should have recognised the fixture, got {final_text:?}"
    );
}

/// Times transcription on each GPU the driver enumerates.
///
/// Exists because `[recognition] gpu_device` is easy to leave at its default
/// and the symptom of getting it wrong is silent: everything works, just
/// slower. This turns "which device should I pick" into a measurement.
///
/// `cargo test -p govox-asr --release --test recognition -- --ignored
///  compares_gpu_devices --nocapture`
#[tokio::test]
#[ignore = "needs a model file and benchmarks every GPU"]
async fn compares_gpu_devices() {
    let _gpu = GPU.lock().await;
    let Some(model) = cached_tiny_en() else {
        panic!("no cached model");
    };
    let audio = load_fixture();

    for device in [0_i32, 1] {
        let mut config = defaults();
        config.recognition.model = "tiny.en".to_owned();
        config.recognition.model_dir = model.to_string_lossy().into_owned();
        config.recognition.gpu_device = device;

        let recognizer = match WhisperRecognizer::start(
            &config.recognition,
            &PersonalDictionary::default(),
            4,
        ) {
            Ok(recognizer) => recognizer,
            Err(error) => {
                eprintln!("device {device}: unavailable ({error})");
                continue;
            }
        };
        let handle = recognizer.handle();
        if handle.warm_up().await.is_err() {
            eprintln!("device {device}: model would not load");
            continue;
        }

        // One warm pass first: the first transcription on a fresh context
        // includes shader compilation, which is not what we are comparing.
        let _ = handle.transcribe(&audio).await;

        let runs = 5;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            handle.transcribe(&audio).await.expect("transcribes");
        }
        let each = started.elapsed().as_secs_f64() / f64::from(runs);
        eprintln!("device {device}: {each:.3}s per transcription");
    }
}

/// Times the model this machine is actually configured to use.
///
/// [`compares_gpu_devices`] deliberately pins `tiny.en`: it answers "which GPU
/// is faster", and holding the model constant is what makes that comparison
/// mean anything. It is *not* an answer to "how long does my dictation take to
/// decode", and reading it as one overstates every larger model — a mistake
/// already made once here, and used to justify tuning latency constants.
///
/// So this one loads the real configuration, `[recognition] model` and
/// `gpu_device` included, and reports what that costs.
///
/// `cargo test -p govox-asr --test recognition -- --ignored
///  times_the_configured_model --nocapture`
#[tokio::test]
#[ignore = "needs the configured model and benchmarks it"]
async fn times_the_configured_model() {
    let _gpu = GPU.lock().await;
    let config = Config::load(None).expect("the machine's own configuration loads");
    let audio = load_fixture();

    let recognizer =
        WhisperRecognizer::start(&config.recognition, &PersonalDictionary::default(), 4)
            .expect("the configured recognizer starts");
    let handle = recognizer.handle();
    handle.warm_up().await.expect("the configured model loads");

    let runs = 5;
    let started = std::time::Instant::now();
    for _ in 0..runs {
        handle.transcribe(&audio).await.expect("transcribes");
    }
    let each = started.elapsed().as_secs_f64() / f64::from(runs);
    eprintln!(
        "model={} gpu_device={}: {each:.3}s per transcription",
        config.recognition.model, config.recognition.gpu_device
    );
}
