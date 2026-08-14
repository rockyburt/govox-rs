---
last_verified: 2026-08-14
owner: rockyburt
type: Guide
covers:
  - crates/govox-asr/
---

# Choosing a recognition model

Models are GGUF builds of Whisper, fetched from Hugging Face on first use and cached.
`tiny`, `base`, `small`, `medium`, `large-v1/2/3` and `large-v3-turbo` are available, each
with an English-only `.en` variant that is more accurate than its multilingual twin at the
same size.

Set the model in `~/.config/govox/config.toml`:

```toml
[recognition]
model = "small.en"
gpu_device = 0
```

## The trade-off

Decode cost varies enormously — roughly 20× between the smallest and largest on the same
GPU — and it is the main thing to tune. `small.en` is a good default; `large-v3-turbo` buys
noticeably better accuracy if you can afford the latency.

Latency matters more than it looks. While streaming is on, the live preview can only update
about as often as a decode takes, so a slow model does not merely delay the final text — it
makes the words appear in larger, less frequent jumps as you speak.

## Measure it on your own hardware

Published numbers are worth little here, because decode time depends on the GPU, the
backend the binary was compiled with, and the length of the utterance. Measure directly:

```bash
cargo test -p govox-asr --test recognition \
  -- --ignored times_the_configured_model --nocapture
```

This times whichever model your config currently names, so change `model` and re-run to
compare. It is `#[ignore]`d because it needs a real model download and hardware.

## Which accelerator

whisper.cpp selects its accelerator at **compile time**, not from config, so the model
choice interacts with how the binary was built — see the build variants in
[README.md](../../README.md). `[recognition] device = "cuda"` on a CPU-only build is a
startup error naming the fix, never a silent fallback to CPU.

Note that Vulkan and CUDA enumerate devices differently: Vulkan sees every GPU including an
integrated one, so `gpu_device` may need to be 1 on a machine where CUDA would have called
the discrete card 0.
