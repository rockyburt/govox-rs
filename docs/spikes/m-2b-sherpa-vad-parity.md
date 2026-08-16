---
last_verified: 2026-08-16
owner: rockyburt
type: Spike
covers:
  - spikes/parakeet-probe/
  - crates/govox-vad/
  - crates/govox-core/src/vad.rs
---

# M-2(b): could sherpa-onnx's Silero VAD replace the `silero` crate?

**Answer: no, and the parity test that would have decided it cannot be run.**

## Why this was asked

[m-2a](m-2a-sherpa-onnx-coexistence.md) established that `sherpa-onnx` carries its own ONNX
Runtime, that statically linking it alongside `ort` fails on duplicate `onnx::*` symbols,
and that shared linking works but forfeits the self-contained binary. It named one route
that would keep what the current design bought:

> **Drop `silero`, use sherpa-onnx's own Silero VAD.** One runtime, static linking back on
> the table, self-contained binary preserved.

The stated gate was measuring sherpa's probabilities against the existing fixtures, the way
[m-1c](m-1c-silero-vad.md) did — 44/47 windows bit-identical, the rest within 1e-6, against
a ≤1e-4 bar.

That gate turns out to be ungateable, for a more basic reason than the numbers.

## Finding 1 — there is no probability to compare

`govox_core::vad` is a state machine over a `SpeechProbability`: one float per 512-sample
window. That is the seam a replacement has to fill.

sherpa-onnx's VAD does not expose one. Its entire output surface is:

```rust
pub struct SpeechSegment { start: i32, samples: &[f32], n: i32 }
impl VoiceActivityDetector {
    fn accept_waveform(&self, samples: &[f32]);
    fn detected(&self) -> bool;
    fn front(&self) -> Option<SpeechSegment>;
    // pop, clear, reset, flush, is_empty
}
```

Checked against the crate source rather than the docs page, and against
`sherpa-onnx-sys` too: **neither the safe wrapper nor the underlying C API has any
probability or score accessor for VAD.** The only `score` fields in the `-sys` crate belong
to speaker embedding, ASR hotwords, TTS and keyword spotting.

So m-1c's method — compare probability curves at 1e-4 — has nothing to compare. Not "the
numbers differ": there are no numbers.

## Finding 2 — it is a whole VAD, not a probability source

Swapping it in would not replace `govox-vad`'s backend. It would replace
`govox_core::vad`'s state machine as well, because sherpa brings its own:

| `[vad]` in govox | `SileroVadModelConfig` |
|---|---|
| `speech_threshold` — enter speech | `threshold` — **one, for both directions** |
| `silence_threshold` — leave speech | **not expressible** |
| `min_speech_ms` | `min_speech_duration` |
| `hangover_ms` | `min_silence_duration` (related, not equivalent) |
| — | `max_speech_duration`, `window_size` |

govox uses **hysteresis**: a higher bar to enter speech than to leave it, which is what
stops a wavering probability from chopping one utterance into several. sherpa has a single
threshold and cannot express that at all. Two of the four tuned `[vad]` keys would become
meaningless, and they are user-facing configuration.

This is the same objection already recorded against whisper.cpp's built-in VAD, and it
applies with more force here, since that entry's reasoning was only about *where utterances
split*:

> the VAD decides where utterances split, so swapping it silently re-tunes segmentation and
> the ported VAD tests stop being parity tests.

## Finding 3 — it would not deliver the self-contained binary anyway

Which was the entire point of the route. The `silero` crate compiles its model in; sherpa's
`SileroVadModelConfig.model` is an `Option<String>` path, and construction fails without it:

```text
vad-model-config.cc:Validate:60 Please provide one VAD model.
c-api.cc:SherpaOnnxCreateVoiceActivityDetector:1349 Errors in config
sherpa VAD       : refuses to construct without a model file
```

So the trade is not "lose `libsherpa-onnx-c-api.so`, keep a self-contained binary". It is
"lose the shared object, gain `silero_vad.onnx`". Smaller, but the property m-2a's option 2
existed to preserve is gone either way.

## What this leaves

m-2a listed three routes. This removes the middle one:

1. **Keep both, shared-linked.** Still viable, still costs the self-contained binary, two
   ONNX Runtimes in one process.
2. ~~Drop `silero` for sherpa's VAD.~~ **Ruled out.** No probability to compare, no
   hysteresis to express, and a model file on the install path regardless.
3. **Do nothing.** The `WordRecognizer` seam keeps a Parakeet backend additive whenever it
   is picked up.

A Parakeet backend therefore costs the self-contained binary, or it does not happen. That
is a product decision rather than a technical one, and it should be made knowing the
accuracy case is unproven on this vocabulary: the [accuracy eval](../guides/accuracy-eval.md)
shows bias carrying term recall from 10/27 to 20/27, and sherpa-onnx has no `initial_prompt`
equivalent to carry it.

## Reproducing

```bash
cd spikes/parakeet-probe
cargo run
```
