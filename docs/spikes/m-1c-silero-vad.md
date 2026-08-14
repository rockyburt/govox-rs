---
last_verified: 2026-08-13
owner: rockyburt
type: Spike Result
covers:
  - spikes/silero-probe/
---

# M-1(c) — Silero VAD in Rust: probability parity and deployment

**Date:** 2026-08-13 · **Reference:** govox-py @ `3ad8c0fb` · **Verdict: PASS**, and it
also closes the ONNX Runtime packaging risk.

Re-verified against the pinned source after `REFERENCE` was introduced: the extracted
tree produces byte-identical probabilities to the live checkout, confirming the recent
upstream overlay work did not touch the VAD path.

## Question

`govox-py`'s `VadSegmenter` is a pure state machine over a `SpeechProbability` callable,
with thresholds (`speech_threshold`, `silence_threshold`, `min_speech_ms`, `hangover_ms`)
tuned against Silero's actual output. The state machine ports trivially. What matters is
whether the *numbers* feeding it are the same, because a different probability curve means
utterances split in different places — and then the ported VAD tests are no longer parity
tests, they are just tests.

Secondary question: how ONNX Runtime reaches the machine. `download-binaries` at build
time is unacceptable for a `.deb`, and `libonnxruntime.so` is not in Ubuntu main.

## Method

Two probes over the same audio, `govox-py/tests/fixtures/hello.wav` (44.1 kHz stereo,
1.51 s → 47 windows of 512 samples at 16 kHz):

- `spikes/silero-probe/` — the `silero` 0.6.0 crate.
- `tools/parity-gen/silero_probs.py` — drives `govox.audio.vad.load_silero()`, i.e. what
  govox-py actually feeds its segmenter, not a fresh call to `silero_vad`.

Both loaders downmix and nearest-neighbour resample identically, matching
`govox.audio.capture.normalize_to_mono`, so the sample stream reaching the model is the
same on both sides and any divergence is the model's.

## Result

**44 of 47 windows are bit-identical at 6 decimal places. The other 3 differ by exactly
1e-6** — last-digit float32 print rounding:

```text
win 18   rust 0.748749   python 0.748750
win 38   rust 0.735768   python 0.735769
win 42   rust 0.915162   python 0.915163
```

Acceptance was ≤1e-4. Actual is 1e-6. The existing VAD thresholds carry over untouched,
and the ported `VadSegmenter` tests remain genuine parity tests.

## Consequences for the plan

1. **The `silero` crate's API is a closer fit than the Python it replaces.**
   `Session::infer_chunk(&mut StreamState, &[f32]) -> f32` *is* the `SpeechProbability`
   callable, and `StreamState::reset()` is exactly what `VadSegmenter.reset()` needs on an
   utterance edge. `govox-py` hides the same recurrent state inside a closure over
   `nonlocal` variables, which is the awkward part of `vad.py`; here it is an explicit
   value the segmenter can own.
2. **Deployment risk closed.** The model is compiled in (`BUNDLED_MODEL`), and ONNX
   Runtime links statically — a 33 MB self-contained binary with no `libonnxruntime.so`
   and no runtime download. Compare govox-py, which needs `silero-vad`, `onnxruntime`
   **and torch** installed at runtime.
3. **torch disappears.** `govox-py`'s `vad.py` imports torch to build the input tensor
   even though the model is ONNX. Nothing in the Rust path needs it. That is the single
   largest dependency removed by the port.
4. `[vad]` gains no new config keys; `speech_threshold` and friends keep their meaning.

## Not adopted

`whisper-rs` ships `whisper_vad.rs` — whisper.cpp now has a built-in VAD. Rejected for the
same reason a different probability curve would be a problem: the VAD decides where
utterances split, so swapping it silently re-tunes segmentation. Silero stays.
