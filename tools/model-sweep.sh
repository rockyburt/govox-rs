#!/usr/bin/env bash
# Score every candidate model against the eval corpus, and print them side by side.
#
# The eval harness takes the model from config, so a sweep is a loop over
# `GOVOX__RECOGNITION__MODEL` rather than a change to the harness. Nothing here
# is govox code; it is the thing that turns one measurement into a comparison.
#
# Why this exists: the configured model moved from `small` to `large-v3-turbo`
# on 2026-08-14 because of a single bad transcription, at roughly 5.5x the
# decode cost. That trade was never checked. Decode time also sets the streaming
# preview cadence — the preview cannot update faster than the model decodes — so
# the cost is not just GPU seconds, it is how immediate dictation feels.
#
# Usage:
#   tools/model-sweep.sh                     # the default candidate set
#   tools/model-sweep.sh tiny.en small.en    # specific models
#
# Stop the daemon first: it holds the GPU and the microphone.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# English-only variants: `[recognition] language` is "en" here, and the `.en`
# builds are more accurate than their multilingual twins at the same size.
# `small` (multilingual) is included because it is the shipped default.
models=("$@")
if (( ${#models[@]} == 0 )); then
  models=(tiny.en base.en small.en small medium.en large-v3-turbo)
fi

# A model's first decode after a download or a cold cache carries warm-up, and
# that lands in the mean. It is not a small effect: the first sweep timed
# `small.en` at 0.38s and a cached re-run gave 0.23s, which is what it must be —
# it is the same size as `small`. Accuracy is unaffected (decoding is
# deterministic at temperature 0 with beam size 1), so only the timings lie.
# Warn per model rather than silently reporting a number that cannot be right.
cache="${HF_HOME:-$HOME/.cache/huggingface}/hub"
cold=()
for model in "${models[@]}"; do
  if ! find "$cache" -name "ggml-${model}.bin" -print -quit 2>/dev/null | grep -q .; then
    cold+=("$model")
  fi
done
if (( ${#cold[@]} )); then
  echo "note: not yet cached — their decode times will include warm-up and read high:" >&2
  echo "      ${cold[*]}" >&2
  echo "      Re-run the sweep once everything is cached and trust the second pass." >&2
  echo >&2
fi

results=()
for model in "${models[@]}"; do
  echo "── $model" >&2
  out=$(GOVOX__RECOGNITION__MODEL="$model" \
        cargo test -p govox-asr --test eval -- --ignored --nocapture 2>&1) || {
    echo "   FAILED — see output below" >&2
    echo "$out" | tail -20 >&2
    results+=("$model|failed|||")
    continue
  }

  agg=$(grep -E "^aggregate:" <<<"$out" || true)
  rec=$(grep -E "^term recall:" <<<"$out" || true)
  if [[ -z $agg ]]; then
    echo "   no aggregate line — did the audio load?" >&2
    results+=("$model|no-audio|||")
    continue
  fi

  raw=$(sed -E 's/.*raw WER ([0-9.]+).*/\1/' <<<"$agg")
  corr=$(sed -E 's/.*corrected WER ([0-9.]+).*/\1/' <<<"$agg")
  secs=$(sed -E 's/.*mean decode ([0-9.]+)s.*/\1/' <<<"$agg")
  terms=$(sed -E 's/term recall: ([0-9]+\/[0-9]+).*/\1/' <<<"$rec")
  results+=("$model|$raw|$corr|$secs|$terms")
  echo "   raw $raw  corrected $corr  ${secs}s  recall $terms" >&2
done

echo
printf '%-16s %10s %12s %10s %9s\n' model "raw WER" "corr. WER" "decode s" recall
printf '%-16s %10s %12s %10s %9s\n' ---------------- ---------- ------------ ---------- ---------
for row in "${results[@]}"; do
  IFS='|' read -r m raw corr secs terms <<<"$row"
  printf '%-16s %10s %12s %10s %9s\n' "$m" "$raw" "$corr" "$secs" "$terms"
done

echo
echo "The eval rewrites corpus/eval/baseline.json on every run, so it now holds"
echo "whichever model ran last. Restore it with:"
echo "  git checkout -- corpus/eval/baseline.json"
