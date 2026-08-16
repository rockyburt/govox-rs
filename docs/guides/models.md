---
last_verified: 2026-08-16
owner: rockyburt
type: Guide
covers:
  - crates/govox-asr/
  - tools/model-sweep.sh
---

# Choosing a recognition model

Models are GGUF builds of Whisper, fetched from Hugging Face on first use and cached.
`tiny`, `base`, `small`, `medium`, `large-v1/2/3` and `large-v3-turbo` are available, each
with an English-only `.en` variant.

Set the model in `~/.config/govox/config.toml`:

```toml
[recognition]
model = "small"
gpu_device = 0
```

## Measured, on the eval corpus

`tools/model-sweep.sh` scores every candidate against
[the accuracy eval](accuracy-eval.md). On the reference machine (RTX 4070, Vulkan,
29 clips, 2026-08-16):

| model | raw WER | corrected WER | decode | term recall |
|---|---|---|---|---|
| `tiny.en` | 0.169 | 0.162 | 0.09 s | 20/27 |
| `base.en` | 0.220 | 0.210 | 0.12 s | 18/27 |
| `small.en` | 0.151 | 0.106 | 0.23 s | 23/27 |
| **`small`** | 0.124 | 0.091 | **0.24 s** | **24/27** |
| `medium.en` | 0.158 | 0.106 | 0.51 s | 24/27 |
| **`large-v3-turbo`** | **0.097** | **0.067** | 0.53 s | 22/27 |

Three things in that table contradict what this guide used to say.

**Bigger is not monotonically better.** `base.en` is worse than `tiny.en`. `medium.en` is
worse than `small` on every axis including speed.

**The `.en` variants are not reliably better.** Plain `small` beats `small.en` on WER
(0.124 against 0.151) and on recall (24 against 23), at the same decode cost — they are the
same size, and both decode in 0.23–0.24 s. The claim that an English-only build is more
accurate than its multilingual twin was inherited, not measured, and does not hold here.
The gap is reproducible: both models return byte-identical figures on repeat runs, because
`temperature = 0.0` with `beam_size = 1` makes decoding deterministic.

**Accuracy in this table is exact; the timings are not.** A model's first decode after a
download or a cold cache carries warm-up, and one such reading put `small.en` at 0.38 s in
the first sweep — impossible, given it is the same size as `small`. Re-running with every
model cached gave 0.23 s and changed nothing else. Run the sweep twice and trust the second
pass, or pre-fetch the models.

**The two metrics disagree, and which one matters depends on what you dictate.**
`large-v3-turbo` has the lowest WER by a clear margin. `small` has the better *term recall*
— it gets `Appleton`, `Gander` and `Rentsync` right where turbo does not, missing only
`Glovertown` that turbo catches. Turbo's stronger language model appears to be the cause
rather than a weakness in its acoustics: `Appleton` becomes "Hamilton", a confident
substitution of a common place name for a rare one. For dictation full of unusual proper
nouns, a weaker language model can be an advantage.

So: **`large-v3-turbo` for the lowest overall error, `small` for proper nouns at half the
latency.** Both are defensible. `small` is the shipped default.

Treat these figures as one machine, one voice, 29 clips — enough to rank the models, not
enough to resolve a one-point difference. Re-run the sweep on your own corpus.

## The trade-off

Decode cost varies enormously — roughly 20× between the smallest and largest on the same
GPU — and it is the main thing to tune.

Latency matters more than it looks. While streaming is on, the live preview can only update
about as often as a decode takes, so a slow model does not merely delay the final text — it
makes the words appear in larger, less frequent jumps as you speak.

## Measure it on your own hardware

Published numbers are worth little here, because decode time depends on the GPU, the
backend the binary was compiled with, and the length of the utterance — and, as the table
above shows, accuracy rankings do not survive contact with a specific vocabulary either.

To rank models on your own corpus, once it is recorded:

```bash
systemctl --user stop govox-rs-dev     # it holds the GPU and the microphone
tools/model-sweep.sh                   # or: tools/model-sweep.sh tiny.en small
```

Each run leaves `corpus/eval/baseline.json` holding whichever model went last; the script
says so, and `git checkout -- corpus/eval/baseline.json` puts it back.

To time one model without scoring it:

```bash
cargo test -p govox-asr --test recognition \
  -- --ignored times_the_configured_model --nocapture
```

Both are `#[ignore]`d because they need a real model download and hardware.

## Which accelerator

whisper.cpp selects its accelerator at **compile time**, not from config, so the model
choice interacts with how the binary was built — see the build variants in
[README.md](../../README.md). `[recognition] device = "cuda"` on a CPU-only build is a
startup error naming the fix, never a silent fallback to CPU.

Note that Vulkan and CUDA enumerate devices differently: Vulkan sees every GPU including an
integrated one, so `gpu_device` may need to be 1 on a machine where CUDA would have called
the discrete card 0.
