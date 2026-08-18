---
last_verified: 2026-08-16
owner: rockyburt
type: Guide
covers:
  - crates/govox-core/src/eval.rs
  - crates/govox-asr/tests/eval.rs
  - corpus/eval/
  - tools/record-eval.sh
  - tools/cut-take.py
---

# Measuring accuracy

govox shipped with *"word error rate is not measured"* in its known limitations, which
made every accuracy decision a matter of impression. The clearest example: the configured
model moved from `small` to `large-v3-turbo` on 2026-08-14 because of one bad
transcription, at **5.5× the decode cost** — a trade nobody could check.

This is how to check it.

## What it measures

A **regression baseline for the configured model**, not a comparison between models. It
answers two questions:

1. *Did recognition get materially worse?* — word error rate.
2. *Is the personal dictionary still earning its place?* — per-term recall, and the gap
   between the raw and corrected scores.

On its own it does **not** answer whether `large-v3-turbo` is worth its cost. That needs a
sweep across models, and `tools/model-sweep.sh` now does one — the harness takes the model
from config, so a sweep is a loop over `GOVOX__RECOGNITION__MODEL`. The results, and the
two guide claims they overturned, are in [models.md](models.md).

The short version: turbo has the lowest WER, `small` has the better term recall at half the
decode time, and bigger is not monotonically better.

## Two decode paths, and the one nobody was measuring

Everything above, every number in [models.md](models.md), and `corpus/eval/baseline.json`
all score `WhisperHandle::transcribe` — one decode over a whole clip. **Dictation never
does that.** It runs `transcribe_words` over a growing window through `OnlineProcessor`,
committing words on LocalAgreement-2. Different path, different cost, different accuracy.

```bash
GOVOX_EVAL_STREAMING=1 cargo test -p govox-asr --test eval -- --ignored --nocapture
```

Measured on the same 29 clips with `model = "small"`:

| path | raw WER | corrected WER | term recall |
|---|---|---|---|
| utterance | 0.124 | 0.091 | 24/27 |
| streaming, when first measured | 0.247 | 0.206 | 19/27 |
| **streaming now** | **0.146** | **0.115** | **22/27** |

When this was first measured the streaming path was **about twice as bad as every
published figure**, and it lost whole words rather than mangling them — "I need to at the
store on the way home". The cause was word timestamps that move between decodes of
overlapping windows, against a commit filter that assumed they do not; the diagnosis is in
`docs/parity.md` under "Recognising which words a hypothesis already committed", and
`stream_trace` is the tool that found it.

Most of that gap is now closed. What remains is the same acoustic failures the utterance
path has — `Glovertown` as "Govertown", `cache` as "cash". Streaming is still the weaker
path and should be scored on its own before any claim about accuracy.

An earlier version of this section also blamed "windows where whisper.cpp returns no words
at all for audio that plainly contains speech". **That was wrong, and it is worth keeping
the correction visible**, because the mistake is an easy one to repeat: the evidence was a
trace showing three consecutive decodes with an empty `pending`, read as the model
returning nothing. It was not. Those decodes returned full word lists — "We drove out to
the city.", "We drove out to Twillingate." — and the *commit filter* discarded them, which
is the bug fixed above. Re-measured across all 29 clips afterwards: **0 of 252 decodes
returned no words.** An empty caption is not evidence of an empty decode; only the
hypothesis log says which it was.

Streaming runs deliberately leave `baseline.json` alone. It remains the utterance record.

### Tracing one clip

An aggregate score says a word went missing, not where. `stream_trace` replays a single
clip through the streaming path and prints every window the model saw, every word it
returned with its timestamps, and what the agreement buffer did with each one — alongside
the same clip decoded in one pass, which is the standard the streaming result is failing
to meet.

```bash
GOVOX_TRACE_CLIP=prose-groceries \
    cargo test -p govox-asr --test stream_trace -- --ignored --nocapture
```

It loads the configured dictionary, because the bias prompt changes what the model
returns: an early version of this trace used an empty one and produced a clean transcript
for a clip the eval was failing.

**Pass several clips to build a long session**, joined with a beat of silence:

```bash
GOVOX_TRACE_CLIP="prose-longer,twillingate-drive,github-gitlab" \
    cargo test -p govox-asr --test stream_trace -- --ignored --nocapture
```

This is the only way to reach a whole class of behaviour from this corpus. Every clip is
under 8 s and `buffer_trimming_sec` is 10 s, so **no single clip ever trims the buffer** —
and two distinct bugs lived on the other side of that line, invisible to a 29-clip score:
a word committed twice when the model revised something behind the commit point, and the
words immediately after a trim being decoded with no run-up. Both are in `docs/parity.md`.

The scoring corpus cannot be fixed by adding a longer clip, because a clip is one
utterance and this needs a session. Judge a long session by comparing its `STREAMED:` line
against the one-pass decode printed above it; that gap was 0.081 WER and is now 0.032.

### Pin the cadence when comparing accuracy

By default the harness schedules decodes the way `pipeline.rs` does — a ~50% duty cycle,
so the next decode waits one decode-length after the last finishes. That is faithful, and
it makes accuracy **irreproducible**: GPU jitter changes how many decodes a clip gets,
which changes the windows, which changes what commits. Three identical runs scored raw WER
0.253, 0.247 and 0.248 with term recall 20, 19 and 21.

`GOVOX_EVAL_CADENCE_S=0.5` pins the schedule to a fixed interval of audio instead. Windows
become identical between runs and accuracy repeats exactly, while decode times are still
measured — they just no longer feed back into the schedule.

**Compare accuracy with it set, and speed with it unset.** Any accuracy claim from an
unpinned run is worth about ±0.01 WER and ±1 term, which is larger than most changes.

## What was tried to make the decode faster

Dictation's caption cadence is two decodes wide, so decode time sets how fast words
appear. Four levers were measured against the streaming corpus. **One was kept.**

| lever | verdict | evidence |
|---|---|---|
| Reuse `WhisperState` | **kept** | 0.240 s → 0.215 s per decode |
| `temperature_inc = 0.0` | rejected | ~5% faster, raw WER 0.247 → 0.283, recall 20 → 16 |
| `n_threads` 8 or 16 | rejected | 0.235 s and 0.243 s against the default's 0.235 s |
| `audio_ctx` scaled to window | rejected | no speed gain, recall 19 → 13, and it aborts |

Two of these are worth more than their table rows.

**The temperature fallback earns its cost.** whisper.cpp defaults `temperature_inc` to
0.2 and re-decodes at rising temperature when a pass trips `logprob_thold` or
`entropy_thold`. It was the prime suspect for "sometimes it takes too long". Turning it
off is a bad trade: about 5% off a decode for 0.035 WER and four terms.

**`audio_ctx` is the one to stay away from.** Whisper encodes a full 30 s mel window
however little audio it is given, so scaling the encoder to the real window looks like the
obvious big win. It is not. On a Vulkan build it produced **no measurable speedup at all**
— the encoder is not where the time goes — while term recall fell from 19/27 to 13/27. It
is also unsafe with word timestamps: at smaller contexts the decoder emits garbage on a
near-empty span and whisper.cpp's DTW asserts and **kills the process**:

```
WHISPER_ASSERT: whisper.cpp:8772: filter_width < a->ne[2]
```

`medfilt_width` is hardcoded to 7, so DTW needs more than 14 frames in the segment. That
is an abort, not an error return — nothing in Rust can catch it, and in the daemon it
would land mid-sentence.

## The baseline

`large-v3-turbo` on Vulkan, 29 clips, reference machine.

| | first run | after the dictionary audit | after the prompt reshape |
|---|---|---|---|
| Raw WER | 0.124 | 0.128 | **0.097** |
| Corrected WER | 0.094 | 0.075 | **0.067** |
| Term recall | 20 / 27 | 22 / 27 | **22 / 27** |

All three runs are the same 29 clips on the same machine and model; only the dictionary and
the prompt shape changed between them.

### Bias is the lever that works

Measured by ablation — the same corpus with `bias_prompt_token_budget = 0`:

| | bias on | bias off |
|---|---|---|
| Raw WER | 0.124 | 0.155 |
| Term recall | **20 / 27** | **10 / 27** |

Twillingate, Glovertown, Gambo, Notre Dame Bay, Newfoundland, Labrador, Rentsync,
Rentals-API and Domum are recognised *only* because they are biased. Run this ablation
after any model change: it is the cheapest way to find out whether the word list still
earns the prompt space it takes from the streaming context.

### Every `replace` rule was dead

Not one of the eleven fired on any clip. They were written as *predictions* of how Whisper
would mangle these words, under a different model; the words they targeted now come out
correctly on their own because they are biased. Ten were deleted. The survivor, `lol`, is
a casing fix rather than a mis-hearing fix and no clip exercises it.

**Do not read a raw-versus-corrected gap as the dictionary earning its place.** `raw` is
scored against `say` — the words literally spoken — and `corrected` against `expect`. When
Whisper writes "Rentals.ca" for the spoken "rentals dot ca", that scores as a raw error and
a corrected success without any rule firing. The honest measure of the dictionary is the
ablation above.

### What the model still gets wrong

| Expected | produced | rule? |
|---|---|---|
| Lewisporte | "Losort" | no — the whole sentence collapsed; "the ferry" became "the prairie" |
| Rentsync | "Durant Sink deploy" | no — recognised correctly in the *other* Rentsync clip |
| Jira | "Jiriborg" | no |
| Gander | "Ganner" | no |
| Appleton | "Hamilton" | **never** — Hamilton is a real place, and the rule would corrupt honest uses |

Each was seen once, and a rule built on a single observation of a non-deterministic model
is a guess wearing evidence's clothes. The standard is the same thing wrong the same way
more than once.

### The prompt is a sentence, not a word list

Whisper conditions `initial_prompt` as if it were the transcript *preceding* this audio.
The prompt used to be a bare word salad — `Newfoundland Labrador Gander Gambo …` — which is
unlike anything the model was trained on. Wrapping the same words in a sentence, changing no
term:

| prompt shape | raw WER | corrected WER | recall |
|---|---|---|---|
| `Gander Gambo …` (was) | 0.128 | 0.075 | 22/27 |
| `Gander, Gambo, …` | 0.130 | 0.077 | 21/27 |
| `This transcript mentions Gander, Gambo, ….` | 0.098 | 0.068 | 21/27 |
| **`This transcript mentions Gander Gambo ….`** | **0.097** | **0.067** | **22/27** |

The sentence frame is worth ~24% relative on raw WER; commas cost a term in both variants
that used them, so the list stays space-joined inside the sentence. Two clips improved at
the document level, none regressed.

This is a change to the *mechanism*, not to any term — which is the only kind of tuning this
corpus can honestly support. Fitting the word list until the misses disappear would turn a
29-clip test set into a training set.

### The five that resist

`Appleton`, `Lewisporte`, `Gander`, `Rentsync` and `Jira` are all biased and all still
missed. The raw output says why they are not a prompting problem:

| clip | produced |
|---|---|
| `glovertown-appleton` | "runs past **Hamilton**" — a real place, confidently substituted |
| `lewisporte-ferry` | "**The prairie** leaves from **Losort** to the morning" |
| `gander-gambo` | "from **Ganner** to Gambo" — while `Gambo` beside it survives |
| `rentsync-deploy` | "The **Rancink** deploy" — while `Rentsync` in its other clip is exact |
| `jira-standup` | "the **jiribor** up" |

Two of these are decisive. `Gander` fails in a clip where `Gambo` — equally rare, equally
biased, one second later — is recognised. `Rentsync` fails here and is perfect in
`rentsync-jira`. A term that succeeds in one clip and fails in another is not a vocabulary
gap; it is that recording. `lewisporte-ferry` puts it beyond doubt by also turning "the
ferry" into "the prairie" — ordinary words, no bias involved.

The honest reading is that these five are acoustic, and the remedies are re-recording those
clips (which selects for easy audio and makes the corpus flatter) or a per-term `replace`
rule (which the dictionary's own standard forbids on a single observation, and which for
`Hamilton` would corrupt a real place name). Neither is worth doing. They are left failing
on purpose: a corpus with no failures left in it has stopped being a measurement.

### The one dictionary fix that worked

`ultra filtered milk` — the phrase that prompted this whole exercise, arriving as
"ultra-fiddle" — had never been *biased*, only guessed at with a replacement. Adding it to
`bias` took both its clips from failing to exact:

| Clip | before | after |
|---|---|---|
| `ultra-filtered-milk` | "ultra-filtered milk" (0.250) | **0.000** |
| `ultra-filtered-milk-long` | "ultra-fizzled milk" (0.182) | **0.000** |

Which is the loop this corpus exists for: change one thing, re-run, watch the number move.

## Running it

```bash
tools/record-eval.sh                                        # once, ~15 minutes
cargo test -p govox-asr --test eval -- --ignored --nocapture

# the path dictation actually uses, with accuracy pinned so it repeats
GOVOX_EVAL_STREAMING=1 GOVOX_EVAL_CADENCE_S=0.5 \
    cargo test -p govox-asr --test eval -- --ignored --nocapture
```

Without the recordings the test **skips** with a message naming the script — it is
`#[ignore]`d like everything else needing a model, and a fresh clone has no audio.

### When `record-eval.sh` cannot run

It prompts per clip with `read`, so it needs an interactive terminal and exits at the
first clip when stdin is not a TTY — a CI runner, a pipeline, an agent harness. The
recorded corpus was made the other way: a handful of sentences per take, split afterwards.

```bash
arecord -f S16_LE -r 44100 -c 2 take.wav          # Ctrl-C when finished
tools/cut-take.py take.wav corpus/eval/audio \
    twillingate-drive:8 twillingate-bonavista:8 glovertown-appleton:7
```

Each argument is `<clip-id>:<word-count>` in reading order. `cut-take.py` segments on RMS
energy against a threshold derived from the recording — an absolute dB threshold fails
silently when the room changes, and background music alone moved the noise floor 20 dB
here — then refuses to write anything unless it finds exactly as many segments as ids and
each one plausibly holds its sentence. Stop the daemon first: it holds the microphone and
watches for the activation key.

## The corpus

`corpus/eval/manifest.toml` is both the script to read aloud and the reference to score
against. Nothing in it is invented: every `term` is a word that has actually been
mis-recognised on this machine, and each has a matching `replace` rule in the personal
dictionary — that file is a logbook of real failures, which makes it the right source for
a regression corpus.

Each clip carries two fields, because there are two questions:

| Field | Scores | Notes |
|---|---|---|
| `say` | the **raw** recogniser output | literally what was spoken |
| `expect` | the **corrected** output | what should land in the document; defaults to `say` |

They differ exactly where govox does work — you say "rentals dot ca" and the document
should read `rentals.ca`; you say "comma" and a comma should appear. Scoring both against
one string would either mark correct behaviour as an error or hide the dictionary doing
its job.

## Where the audio lives, and why not in git

`corpus/eval/audio/` is **gitignored**. This repository is public and the clips are
recordings of a specific person's voice saying place names and work vocabulary. The
manifest and the scores are text and are tracked, so the numbers stay reviewable in git
history without publishing the recordings; `tools/record-eval.sh` reconstitutes the audio
on a new machine.

## Reading the output

- **Per-term recall is the metric that matters.** On a six-word sentence a single wrong
  word is ~16% WER, so the aggregate moves in visible steps and will not resolve a
  one-or-two-point difference. "Did Twillingate survive" is a fact; the WER it contributes
  is a ratio diluted by every word around it.
- **A WER above 1.0 is not a bug.** It means more wrong words than there were words to get
  right — usually a hallucination — and it is deliberately not clamped, because that is the
  case most worth seeing.
- **`correction + dictionary closed N WER`** is the line that says whether the dictionary
  is worth its file. If a term shows no gap between raw and corrected, its rule is either
  dead or no longer needed. Both are worth acting on.

## Adding a clip

Add a `[[clip]]` to the manifest and run `tools/record-eval.sh <id>`. Two non-ignored
tests guard the file and run in CI with no model and no audio:

- `the_eval_manifest_is_well_formed` — unique, filename-safe ids, and every declared term
  actually appears in its own reference. A clip whose term is absent from its own
  reference can never pass, however good the model is.
- `the_spoken_punctuation_targets_are_reachable_without_a_model` — the `spoken-*` clips'
  targets are produced by running `say` through the correction pipeline, so a wrong
  `expect` is caught before anyone speaks into a microphone rather than looking like a
  permanent model failure afterwards.
