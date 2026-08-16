#!/usr/bin/env bash
# Record the accuracy eval corpus.
#
# Reads corpus/eval/manifest.toml, prompts each line, and records it to
# corpus/eval/audio/<id>.wav. The audio stays out of git deliberately — the
# repository is public and these are recordings of someone's voice — so this
# script is how the corpus is reconstituted on a new machine.
#
# Usage:
#   tools/record-eval.sh              record every clip that has no audio yet
#   tools/record-eval.sh <id> [<id>…] re-record specific clips
#   tools/record-eval.sh --all        re-record everything
#
# Read at a normal dictation pace. Do not over-enunciate: a corpus recorded in a
# careful "computer voice" measures a way of speaking nobody actually uses, and
# would report accuracy the daily experience does not match.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$repo_root/corpus/eval/manifest.toml"
audio_dir="$repo_root/corpus/eval/audio"

command -v arecord >/dev/null || {
  echo "arecord not found. Install alsa-utils." >&2
  exit 1
}
[[ -f $manifest ]] || {
  echo "no manifest at $manifest" >&2
  exit 1
}

mkdir -p "$audio_dir"

# 44.1 kHz stereo matches tests/fixtures/hello.wav, and the loader resamples to
# whatever the model wants. Recording at 16 kHz here would bake govox's current
# input rate into the corpus and make it wrong the day that changes.
readonly RATE=44100
readonly CHANNELS=2

# Pull `id`, `say` out of the manifest. Deliberately a small awk pass rather
# than a TOML parser: this file's shape is fixed and owned by the same repo, and
# a dependency to read six lines of it would be a poor trade.
mapfile -t entries < <(
  awk '
    /^id *=/     { gsub(/^id *= *"|" *$/, ""); id = $0 }
    /^say *=/    { line = $0
                   sub(/^say *= *"/, "", line); sub(/" *$/, "", line)
                   if (id != "") { print id "\t" line; id = "" } }
  ' "$manifest"
)

(( ${#entries[@]} )) || {
  echo "no clips found in $manifest" >&2
  exit 1
}

wanted=()
record_all=false
if [[ ${1:-} == --all ]]; then
  record_all=true
elif (( $# )); then
  wanted=("$@")
fi

should_record() {
  local id=$1
  if $record_all; then return 0; fi
  if (( ${#wanted[@]} )); then
    local w
    for w in "${wanted[@]}"; do [[ $w == "$id" ]] && return 0; done
    return 1
  fi
  # Default: only what is missing, so an interrupted session resumes.
  [[ ! -f "$audio_dir/$id.wav" ]]
}

todo=0
for entry in "${entries[@]}"; do
  should_record "${entry%%$'\t'*}" && (( ++todo ))
done

if (( todo == 0 )); then
  echo "Nothing to record — every clip already has audio in $audio_dir"
  echo "Re-record one with: tools/record-eval.sh <id>, or all with --all"
  exit 0
fi

echo "Recording $todo clip(s) into $audio_dir"
echo "Press ENTER to start each clip, then ENTER again when you have finished speaking."
echo "Ctrl-C stops; already-recorded clips are kept."
echo

done_count=0
for entry in "${entries[@]}"; do
  id=${entry%%$'\t'*}
  say=${entry#*$'\t'}
  should_record "$id" || continue
  (( ++done_count ))

  while true; do
    echo "── [$done_count/$todo] $id"
    echo "   say:  $say"
    read -r -p "   ENTER to record… " _

    # Record until the next ENTER: a fixed duration either truncates the long
    # clips or leaves silence on the short ones, and trailing silence is the
    # exact shape Whisper hallucinates on.
    arecord -q -f S16_LE -r "$RATE" -c "$CHANNELS" "$audio_dir/$id.wav" &
    local_pid=$!
    read -r -p "   recording… ENTER to stop " _
    kill "$local_pid" 2>/dev/null || true
    wait "$local_pid" 2>/dev/null || true

    read -r -p "   [ENTER] keep, [r] re-record, [p] play back: " choice
    case $choice in
      r|R) continue ;;
      p|P)
        command -v aplay >/dev/null && aplay -q "$audio_dir/$id.wav" || echo "   (aplay not available)"
        read -r -p "   [ENTER] keep, [r] re-record: " again
        [[ $again == r || $again == R ]] && continue
        ;;
    esac
    break
  done
  echo
done

echo "Done. $done_count clip(s) recorded."
echo "Score them with:"
echo "  cargo test -p govox-asr --test eval -- --ignored --nocapture"
