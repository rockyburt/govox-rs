//! M-1(c): does a Rust Silero VAD reproduce govox-py's probability sequence?
//!
//! `govox-py`'s `VadSegmenter` is a pure state machine over a
//! `SpeechProbability` callable, with thresholds (`speech_threshold`,
//! `silence_threshold`, `min_speech_ms`, `hangover_ms`) tuned against Silero's
//! actual output. The state machine ports trivially; what matters is that the
//! numbers feeding it are the same, because a different probability curve means
//! utterances split in different places and the ported VAD tests stop being
//! parity tests.
//!
//! Reference values came from a generator that drove the earlier Python
//! implementation's Silero wrapper over the same WAV. That generator has been
//! retired along with the rest of the port scaffolding; the recorded result is
//! in `docs/spikes/m-1c-silero-vad.md`.
//!
//! Usage: `cargo run --release -- <audio.wav>`

use anyhow::{Context, Result, bail};

/// Silero v5 consumes exactly 512 samples at 16 kHz per call.
const WINDOW: usize = 512;
const SAMPLE_RATE: u32 = 16_000;

fn load_wav_16k_mono(path: &str) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {path}"))?;
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<_, _>>()?
        }
    };
    let channels = spec.channels as usize;
    let mono: Vec<f32> = if channels == 1 {
        raw
    } else {
        raw.chunks(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    if spec.sample_rate == SAMPLE_RATE {
        return Ok(mono);
    }
    // Nearest-neighbour, matching govox-py's `normalize_to_mono` so the sample
    // stream fed to the model is identical and any divergence is the model's.
    let ratio = spec.sample_rate as f64 / SAMPLE_RATE as f64;
    let out_len = (mono.len() as f64 / ratio) as usize;
    Ok((0..out_len)
        .map(|i| mono[((i as f64 * ratio) as usize).min(mono.len() - 1)])
        .collect())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        bail!("usage: silero-probe <audio.wav>");
    }
    let audio = load_wav_16k_mono(&args[1])?;
    println!("samples: {} ({:.2}s)", audio.len(), audio.len() as f64 / SAMPLE_RATE as f64);
    println!("windows: {}\n", audio.len() / WINDOW);

    // The model is compiled into the crate, so there is no separate download
    // and no model path in the config — a real packaging simplification over
    // govox-py, which pulls silero-vad plus torch plus onnxruntime at runtime.
    let mut session =
        silero::Session::bundled().context("building Silero session (ONNX Runtime available?)")?;
    // Carries the recurrent state between calls. govox-py hides this inside a
    // closure over `nonlocal` variables; here it is explicit, and `reset()` is
    // exactly what `VadSegmenter.reset()` needs to call on an utterance edge.
    let mut stream = silero::StreamState::new(silero::SampleRate::Rate16k);

    println!("silero crate v{}", silero::VERSION);
    println!("{:>5}  {:>8}  {:>10}", "win", "t(s)", "p_speech");
    for (i, chunk) in audio.chunks_exact(WINDOW).enumerate() {
        let p = session.infer_chunk(&mut stream, chunk)?;
        println!(
            "{:>5}  {:>8.3}  {:>10.6}",
            i,
            (i * WINDOW) as f64 / SAMPLE_RATE as f64,
            p
        );
    }
    Ok(())
}
