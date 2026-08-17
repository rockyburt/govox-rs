//! Trace one clip through the streaming path, decode by decode.
//!
//! The eval reports what streaming produced. This reports *how*: every window
//! the model saw, every word it returned with its timestamps, and what
//! `HypothesisBuffer` did with them. It exists because the streaming path
//! scores roughly twice the word error rate of the utterance path on the same
//! audio and loses whole words, and an aggregate score cannot say where a word
//! went.
//!
//! ```text
//! GOVOX_TRACE_CLIP=prose-groceries \
//!     cargo test -p govox-asr --test stream_trace -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use govox_asr::streaming::OnlineProcessor;
use govox_asr::whisper::WhisperRecognizer;
use govox_core::config::Config;
use govox_core::domain::{PersonalDictionary, WordRecognizer};

fn repo_root() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    path.canonicalize().unwrap_or(path)
}

/// Read a 16-bit PCM WAV as mono f32 at 16 kHz.
fn load_wav(path: &Path) -> (Vec<f32>, u32) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
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
    if sample_rate == target {
        return (mono, target);
    }
    let ratio = f64::from(sample_rate) / f64::from(target);
    let out_len = (mono.len() as f64 / ratio) as usize;
    (
        (0..out_len)
            .map(|i| mono[((i as f64) * ratio) as usize])
            .collect(),
        target,
    )
}

#[tokio::test]
#[ignore = "needs the configured model and a recorded clip"]
async fn traces_one_clip_through_the_streaming_path() {
    let clip = std::env::var("GOVOX_TRACE_CLIP").unwrap_or_else(|_| "prose-groceries".to_owned());
    let cadence: f64 = std::env::var("GOVOX_EVAL_CADENCE_S")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.5);

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(false)
        .without_time()
        .with_test_writer()
        .try_init()
        .ok();

    // A comma-separated list is joined into one long session, with a beat of
    // silence between clips. Every corpus clip is under 8 s, so on its own none
    // of them reaches `buffer_trimming_sec` (10 s) — trimming, and everything
    // that only goes wrong in a window long enough to be trimmed, is otherwise
    // unreachable from the corpus.
    let mut samples = Vec::new();
    let mut sample_rate = 16_000;
    for (i, one) in clip.split(',').map(str::trim).enumerate() {
        let path = repo_root().join(format!("corpus/eval/audio/{one}.wav"));
        if !path.is_file() {
            eprintln!("SKIPPING: no audio at {}", path.display());
            return;
        }
        let (part, rate) = load_wav(&path);
        sample_rate = rate;
        if i > 0 {
            samples.extend(std::iter::repeat_n(0.0, rate as usize * 3 / 10));
        }
        samples.extend(part);
    }

    let config = Config::load(None).expect("the machine's own configuration loads");
    // The configured dictionary, not an empty one: the bias prompt changes what
    // the model returns, so a trace without it is not the run being debugged.
    let dictionary = {
        let path = config.correction.dictionary_path.trim();
        if path.is_empty() {
            PersonalDictionary::default()
        } else {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            PersonalDictionary::load(Path::new(path), home.as_deref())
                .expect("the configured personal dictionary loads")
        }
    };
    let recognizer = WhisperRecognizer::start(&config.recognition, &dictionary, 4)
        .expect("the configured recognizer starts");
    let handle = recognizer.handle();
    handle.warm_up().await.expect("the model loads");

    eprintln!(
        "\nclip={clip}  {:.2}s  cadence={cadence:.2}s  min_chunk={:.2}s  limit={:.1}s",
        samples.len() as f64 / f64::from(sample_rate),
        config.streaming.min_chunk_size_s,
        config.streaming.buffer_trimming_sec,
    );

    // The whole clip in one pass, as the reference for what the model can do
    // with this audio when nothing streams it.
    let whole = WordRecognizer::transcribe_words(&handle, &samples)
        .await
        .expect("whole-clip decode");
    eprintln!(
        "\nwhole clip in one decode:\n  {}\n",
        whole
            .iter()
            .map(|w| w.text.trim().to_owned())
            .collect::<Vec<_>>()
            .join(" ")
    );

    let mut processor = OnlineProcessor::new(handle.clone(), &config.streaming, sample_rate);
    let frame = (sample_rate as usize / 50).max(1);
    let mut clock_s = 0.0;
    let mut next_decode_s = 0.0;
    let mut committed_total = String::new();
    let mut n = 0;

    for chunk in samples.chunks(frame) {
        processor.push(chunk);
        clock_s += chunk.len() as f64 / f64::from(sample_rate);
        if !processor.ready() || clock_s < next_decode_s {
            continue;
        }
        next_decode_s = clock_s + cadence;
        n += 1;

        eprintln!("decode {n:>2}  t={clock_s:>5.2}s");
        // The raw hypothesis arrives on the `streaming hypothesis` debug line
        // that `process` emits; decoding here to print it would change what the
        // recognizer sees.
        let update = processor.process().await.expect("streaming decode");
        committed_total.push_str(&update.committed);

        if !update.committed.is_empty() {
            eprintln!("   COMMIT: {:?}", update.committed);
        }
        eprintln!("   pending: {:?}", update.pending);
    }

    let tail = processor.finish(true).await;
    eprintln!("\nfinish tail: {tail:?}");
    eprintln!("\nSTREAMED: {:?}", format!("{committed_total}{tail}"));
}
