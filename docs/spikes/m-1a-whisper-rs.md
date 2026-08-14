---
last_verified: 2026-08-13
owner: rockyburt
type: Spike Result
covers:
  - spikes/whisper-probe/
---

# M-1(a) — whisper-rs: word timestamps and `no_speech_prob`

**Date:** 2026-08-13 · **Verdict: PASS.** The largest risk in the port is retired.

## Question

Streaming (M9) reimplements LocalAgreement-2, which commits the longest common prefix of
the two most recent hypotheses and trims its audio buffer at committed *word* boundaries.
`govox-py` gets those boundaries free from faster-whisper (`word.word/start/end`) and also
reads `segment.no_speech_prob` to drop words from segments scoring above 0.9.

whisper.cpp does not have a word-timestamp flag. It has DTW token timestamps, which need
`dtw_token_timestamps` **and** a per-model `dtw_aheads` preset that does not exist for
every checkpoint — so the risk was that streaming would be blocked outright.

## Method

`spikes/whisper-probe/`, deliberately outside the workspace so building whisper.cpp never
slows `cargo test -p govox-core`. Ran `ggml-tiny.en.bin` over `govox-py`'s own fixture,
`tests/fixtures/hello.wav`.

## Result

```text
audio: 44100 Hz, 2 channel(s), Int 16 bit — 1.51s
dtw preset: TinyEn
2 segment(s) in 0.37s

[  0.00 →   0.48] no_speech=0.1404  ""
      tok "[_BEG_]"    t0=0.00 t1=0.00 t_dtw=-0.01 p=0.718
[  0.48 →   2.00] no_speech=0.0000  " Hello"
      tok " Hello"     t0=0.48 t1=1.31 t_dtw=0.94  p=0.606
      tok "[_TT_100]"  t0=2.00 t1=2.00 t_dtw=-0.01 p=0.237
```

- **Word timestamps: available.** `whisper_token_data.t_dtw` is populated (0.94s for the
  real token). Reached via `segment.get_token(i).token_data()`.
- **`no_speech_prob`: available**, per segment, with real values.
- **DTW presets exist for every standard checkpoint** — `TinyEn` … `LargeV3Turbo`.

## Consequences for the plan

1. **M9 is unblocked.** No fallback to wall-clock trimming is needed. The risk-register
   entry for word timestamps can be closed.
2. **DTW is a context-construction parameter, not a per-call flag.** In faster-whisper
   `word_timestamps=True` is an argument to `transcribe()`; here the preset must be chosen
   when the `WhisperContext` is built, which means the model name has to map to a
   `DtwModelPreset` at load time. Getting it wrong is silent — timestamps are just zero.
3. **DTW is silently disabled when flash-attention is on.** Never enable both.
4. **Special tokens must be filtered.** `[_BEG_]` and `[_TT_100]` appear in the token
   stream with `t_dtw = -1` (unset). The real implementation must skip token ids at or
   above the EOT token rather than trusting `t_dtw` alone.
5. **Word segmentation comes from `token_timestamps` + `max_len(1)` + `split_on_word`**,
   which makes whisper.cpp emit one segment per word. That is the idiom to use, not a
   word list on the segment.

## Decode-option parity (source-read, `whisper_params.rs`)

| `govox-py` (faster-whisper) | whisper-rs | Note |
|---|---|---|
| `log_prob_threshold` | `set_logprob_thold` | direct |
| `temperature` | `set_temperature` | direct |
| `initial_prompt` (bias) | `set_initial_prompt` | direct |
| `condition_on_previous_text` | `set_no_context` | **inverted** |
| `compression_ratio_threshold` | `set_entropy_thold` | **approximate** — entropy, not compression ratio. Different semantics; record in the parity ledger rather than pretending it is equivalent. |
| `no_speech_threshold` | `set_no_speech_thold` | **Present but a no-op** — whisper.cpp documents it as "currently not implemented". govox-rs must gate on `no_speech_probability()` itself, in our own code, or the threshold silently does nothing. |

## Unrelated finding worth recording

`whisper-rs` ships `whisper_vad.rs` — whisper.cpp now has a built-in VAD. Not adopted:
the VAD decides utterance segmentation, so swapping it changes where utterances split,
which would make the VAD tests stop being parity tests. Silero stays, per M-1(c).

## Open item this raised: GPU is not optional here

`~/.config/govox/config.toml` sets `device = "cuda"` and `compute_type = "float16"`, so
the reference install is **GPU-backed today**. This machine has an RTX 4070 Laptop (8 GB,
driver 595.84) but **no CUDA toolkit** (`nvcc` absent), while Vulkan 1.4.341 and `glslc`
*are* present.

whisper-rs selects its GPU backend with a cargo feature, so this is a build-time decision
rather than the runtime `device =` key it is in Python. Recommendation: **build the Vulkan
backend**, which works on the hardware here with no toolkit install and is portable across
vendors. The configured model is `small`, which has a GGUF build, so there is no
model-availability gap.
