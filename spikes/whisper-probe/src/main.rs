//! M-1(a): can whisper-rs give us what streaming needs?
//!
//! Three questions, in order of how much they can hurt the plan:
//!
//! 1. **Per-word timestamps.** LocalAgreement-2 commits the longest common
//!    prefix of the two most recent hypotheses and trims its audio buffer at
//!    committed word boundaries. Without word times there is nothing to trim at.
//! 2. **`no_speech_prob` per segment.** The vendored Python processor drops
//!    words from segments scoring above 0.9.
//! 3. **Whether DTW token timestamps are actually *populated*** — the API
//!    exists, but whisper.cpp needs a per-model `dtw_aheads` preset and DTW is
//!    silently disabled when flash-attention is on.
//!
//! Source reading already answered these "yes" at the API level:
//! `whisper_token_data.t_dtw`, `WhisperState::…no_speech_probability()`, and a
//! `DtwModelPreset` for every standard checkpoint. This binary confirms the
//! values are real at runtime rather than zero.
//!
//! Usage: `cargo run --release -- <model.bin> <audio.wav>`

use anyhow::{Context, Result, bail};
use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters,
};

/// whisper.cpp reports times in centiseconds.
fn cs_to_s(t: i64) -> f64 {
    t as f64 / 100.0
}

fn load_wav_16k_mono(path: &str) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {path}"))?;
    let spec = reader.spec();
    println!(
        "audio: {} Hz, {} channel(s), {:?} {} bit",
        spec.sample_rate, spec.channels, spec.sample_format, spec.bits_per_sample
    );

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

    // Downmix, then nearest-neighbour resample to 16 kHz. Crude on purpose:
    // this is a probe, and govox-core owns the real resampler.
    let channels = spec.channels as usize;
    let mono: Vec<f32> = if channels == 1 {
        raw
    } else {
        raw.chunks(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    if spec.sample_rate == 16_000 {
        return Ok(mono);
    }
    let ratio = spec.sample_rate as f64 / 16_000.0;
    let out_len = (mono.len() as f64 / ratio) as usize;
    Ok((0..out_len)
        .map(|i| mono[((i as f64 * ratio) as usize).min(mono.len() - 1)])
        .collect())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        bail!("usage: whisper-probe <model.bin> <audio.wav>");
    }
    let (model_path, audio_path) = (&args[1], &args[2]);

    let audio = load_wav_16k_mono(audio_path)?;
    println!("samples: {} ({:.2}s)\n", audio.len(), audio.len() as f64 / 16_000.0);

    // DTW must be requested at *context* construction, not per-call, and the
    // preset has to match the checkpoint. This is the part with no analogue in
    // faster-whisper, where word timestamps are just a transcribe() flag.
    let preset = if model_path.contains("small.en") {
        DtwModelPreset::SmallEn
    } else if model_path.contains("base.en") {
        DtwModelPreset::BaseEn
    } else {
        DtwModelPreset::TinyEn
    };
    println!("dtw preset: {preset:?}");

    let mut ctx_params = WhisperContextParameters::default();
    ctx_params.dtw_parameters(DtwParameters {
        mode: DtwMode::ModelPreset { model_preset: preset },
        dtw_mem_size: 1024 * 1024 * 128,
    });

    let ctx = WhisperContext::new_with_params(model_path, ctx_params)
        .context("loading model (is it a GGUF/GGML whisper .bin?)")?;
    let mut state = ctx.create_state()?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    // The trio that makes whisper.cpp emit one segment per word — this is how
    // "word timestamps" are obtained here, versus faster-whisper's word list.
    params.set_token_timestamps(true);
    params.set_max_len(1);
    params.set_split_on_word(true);

    let started = std::time::Instant::now();
    state.full(params, &audio)?;
    let elapsed = started.elapsed();

    let n = state.full_n_segments();
    println!("\n{n} segment(s) in {:.2}s\n", elapsed.as_secs_f64());

    let mut dtw_seen = 0usize;
    let mut tokens_seen = 0usize;

    for seg in state.as_iter() {
        let text = seg.to_str_lossy().unwrap_or_default();
        println!(
            "[{:>6.2} → {:>6.2}] no_speech={:.4}  {:?}",
            cs_to_s(seg.start_timestamp()),
            cs_to_s(seg.end_timestamp()),
            seg.no_speech_probability(),
            text
        );
        for i in 0..seg.n_tokens() {
            let Some(tok) = seg.get_token(i) else { continue };
            let d = tok.token_data();
            tokens_seen += 1;
            if d.t_dtw > 0 {
                dtw_seen += 1;
            }
            if let Ok(s) = tok.to_str_lossy() {
                println!(
                    "      tok {:?} t0={:.2} t1={:.2} t_dtw={:.2} p={:.3}",
                    s,
                    cs_to_s(d.t0),
                    cs_to_s(d.t1),
                    cs_to_s(d.t_dtw),
                    d.p
                );
            }
        }
    }

    println!("\n--- M-1(a) verdict ---");
    println!("segments:              {n}");
    println!("tokens:                {tokens_seen}");
    println!("tokens with t_dtw > 0: {dtw_seen}");
    if dtw_seen > 0 {
        println!("WORD TIMESTAMPS: available. LocalAgreement-2 can trim at word boundaries.");
    } else {
        println!(
            "WORD TIMESTAMPS: t_dtw is zero everywhere. \
             Fall back to segment t0/t1 (still usable at max_len=1) or wall-clock trimming."
        );
    }
    Ok(())
}
