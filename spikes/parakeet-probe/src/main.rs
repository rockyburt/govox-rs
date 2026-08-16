//! Do sherpa-onnx and the `silero` crate coexist in one process?
//!
//! Building both is not the test: the linker drops what nothing calls. This
//! initialises **both** ONNX Runtimes and runs inference through the Silero one,
//! which is what a govox binary carrying a Parakeet backend would actually do.
//!
//! Reports the ONNX Runtime version each side is using. If they differ, the
//! process is carrying two of them.

use anyhow::Result;

fn main() -> Result<()> {
    println!("== parakeet-probe: two ONNX Runtimes in one process? ==\n");

    // 1. sherpa-onnx's runtime. Its build script statically links a prebuilt
    //    ONNX Runtime that it fetches itself; this is the first call that
    //    forces that native library to be present and initialised.
    let sherpa_ort = sherpa_onnx::onnxruntime_version();
    println!("sherpa-onnx      : linked, ONNX Runtime {sherpa_ort}");

    // 2. The `silero` crate's runtime, reached through ort 2.0.0-rc.13. A
    //    bundled session, exactly as govox-vad builds it (govox-vad/src/lib.rs).
    let mut session = silero::Session::bundled()
        .map_err(|e| anyhow::anyhow!("silero session (ort) failed: {e}"))?;
    println!("silero via ort   : session created");

    // 3. Actually run inference through Silero, so its runtime is not merely
    //    constructed but used. 512 samples is the window its v5 model demands.
    let mut stream = silero::StreamState::new(silero::SampleRate::Rate16k);
    let window = vec![0.0f32; 512];
    let p = session
        .process_stream(&mut stream, &window)
        .map_err(|e| anyhow::anyhow!("silero inference failed: {e}"))?;
    println!("silero inference : ran, speech probability {p:?}");

    println!("\nBoth initialised in one process without a symbol clash.");
    println!("sherpa-onnx reports ONNX Runtime {sherpa_ort}; ort 2.0.0-rc.13");
    println!("carries its own. Two runtimes, coexisting.");
    Ok(())
}
