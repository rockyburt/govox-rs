---
last_verified: 2026-08-16
owner: rockyburt
type: Guide
covers:
  - crates/govox-core/src/eval.rs
  - crates/govox-asr/tests/eval.rs
  - corpus/eval/
  - tools/record-eval.sh
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

It does **not** answer whether `large-v3-turbo` is worth its cost. That needs a sweep
across models, which the harness is shaped for but does not yet do.

## Running it

```bash
tools/record-eval.sh                                        # once, ~15 minutes
cargo test -p govox-asr --test eval -- --ignored --nocapture
```

Without the recordings the test **skips** with a message naming the script — it is
`#[ignore]`d like everything else needing a model, and a fresh clone has no audio.

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
