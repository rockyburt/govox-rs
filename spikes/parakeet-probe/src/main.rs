//! Two questions about putting sherpa-onnx in this binary.
//!
//! **M-2(a): do sherpa-onnx and the `silero` crate coexist?** Building both is
//! not the test — the linker drops what nothing calls — so this initialises both
//! ONNX Runtimes and runs real inference through the Silero one.
//!
//! **M-2(b): could sherpa's own Silero VAD replace the `silero` crate?** That was
//! m-2a's one route to a single runtime. It cannot, and the reasons are visible
//! without a model file: sherpa's VAD emits *segments*, never the per-window
//! probability `govox_core::vad` is built on, and it wants that model on disk.

use anyhow::Result;

/// `govox-vad` feeds `govox_core::vad`'s state machine one probability per
/// 512-sample window. Anything replacing it has to supply that same number.
const WINDOW: usize = 512;

fn main() -> Result<()> {
    println!("== parakeet-probe ==\n");

    // --- M-2(a): two runtimes in one process ------------------------------
    let sherpa_ort = sherpa_onnx::onnxruntime_version();
    println!("sherpa-onnx      : linked, ONNX Runtime {sherpa_ort}");

    let mut session = silero::Session::bundled()
        .map_err(|e| anyhow::anyhow!("silero session (ort) failed: {e}"))?;
    let mut stream = silero::StreamState::new(silero::SampleRate::Rate16k);
    let p = session
        .process_stream(&mut stream, &vec![0.0f32; WINDOW])
        .map_err(|e| anyhow::anyhow!("silero inference failed: {e}"))?;
    println!("silero via ort   : inference ran, probability {p:?}");
    println!("  -> two ONNX Runtimes coexist under `features = [\"shared\"]`.\n");

    // --- M-2(b): can sherpa's VAD stand in for the silero crate? ----------
    //
    // The `silero` crate bundles its model; sherpa's takes a path. Constructing
    // it with no model is the cheapest demonstration that adopting it puts a
    // file on the install path — the very property m-2a's option 2 existed to
    // preserve.
    let config = sherpa_onnx::VadModelConfig {
        silero_vad: sherpa_onnx::SileroVadModelConfig {
            model: None,
            threshold: 0.5,
            min_silence_duration: 0.25,
            min_speech_duration: 0.25,
            window_size: WINDOW as i32,
            max_speech_duration: 20.0,
        },
        sample_rate: 16_000,
        num_threads: 1,
        ..Default::default()
    };

    match sherpa_onnx::VoiceActivityDetector::create(&config, 30.0) {
        Some(_) => println!("sherpa VAD       : constructed with no model path (unexpected)"),
        None => println!("sherpa VAD       : refuses to construct without a model file"),
    }

    println!(
        "\nsherpa's VAD emits SpeechSegment {{ start, samples, n }} and `detected()`.\n\
         Neither it nor the C API exposes a per-window probability, so the m-1c\n\
         parity test — comparing probability curves at 1e-4 — cannot be run at all.\n\
         It is a whole VAD, not a `SpeechProbability`: adopting it replaces\n\
         `govox_core::vad`'s state machine, and its single `threshold` cannot\n\
         express the speech/silence hysteresis govox is tuned with."
    );
    Ok(())
}
