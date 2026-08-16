#!/usr/bin/env python3
"""Cut one continuous recording into per-clip WAVs for the eval corpus.

The companion to `record-eval.sh`, for the case where that script cannot run:
it prompts per clip with `read`, so it needs an interactive terminal and does
nothing useful when stdin is not a TTY (a CI runner, a pipeline, an agent
harness). This takes the other approach — record a handful of sentences in one
pass with a pause between each, then split it here.

    arecord -f S16_LE -r 44100 -c 2 take.wav      # Ctrl-C when finished
    tools/cut-take.py take.wav corpus/eval/audio \
        twillingate-drive:8 twillingate-bonavista:8 ...

Each argument is `<clip-id>:<word-count>`, in the order the sentences were
read. The word count is not decoration: it is what lets a mis-split be caught
before it reaches the corpus.

Segmentation is by RMS energy against a threshold derived from the recording
itself, rather than an absolute dB figure. The speech-to-room-noise gap varies
per take — background music alone moved it by 20 dB here — and an absolute
threshold fails silently when it is wrong, which is the one outcome a ground
truth corpus cannot tolerate.

Nothing is written unless every check passes. A questionable clip is not worth
having: it would be indistinguishable from a recognition failure in every score
computed from it afterwards.
"""
import subprocess
import sys
import wave
from pathlib import Path

try:
    import numpy as np
except ImportError:
    sys.exit("needs numpy: pip install numpy (or run tools/record-eval.sh in a terminal)")

WIN = 0.05        # analysis window
PAD = 0.30        # keep the first consonant and the final fricative
MIN_SPEECH = 1.0  # shortest real clip here runs ~1.3s; blips are far below
BRIDGE = 0.35     # dips shorter than this are inside a sentence, not between two
# Seconds per word outside which a segment does not plausibly hold its sentence.
# Wide on purpose: it is a guard against a mis-split, not a delivery critique.
MIN_PER_WORD, MAX_PER_WORD = 0.19, 0.75


def envelope(path):
    with wave.open(str(path), "rb") as w:
        rate, frames, channels = w.getframerate(), w.getnframes(), w.getnchannels()
        raw = w.readframes(frames)
    samples = np.frombuffer(raw, dtype=np.int16).astype(np.float32)
    if channels > 1:
        samples = samples.reshape(-1, channels).mean(axis=1)
    samples /= 32768.0
    step = int(rate * WIN)
    rms = np.array([np.sqrt(np.mean(samples[i * step:(i + 1) * step] ** 2) + 1e-12)
                    for i in range(len(samples) // step)])
    return 20 * np.log10(rms + 1e-12), len(samples) / rate


def segments(db):
    floor, peak = np.percentile(db, 20), np.percentile(db, 95)
    voiced = db > floor + (peak - floor) * 0.35

    bridge, i = int(BRIDGE / WIN), 0
    while i < len(voiced):
        if not voiced[i]:
            j = i
            while j < len(voiced) and not voiced[j]:
                j += 1
            if 0 < j - i <= bridge and i > 0 and j < len(voiced):
                voiced[i:j] = True
            i = j
        else:
            i += 1

    out, i = [], 0
    while i < len(voiced):
        if voiced[i]:
            j = i
            while j < len(voiced) and voiced[j]:
                j += 1
            out.append((i * WIN, j * WIN))
            i = j
        else:
            i += 1
    return out, floor, peak


def main():
    if len(sys.argv) < 4:
        sys.exit(__doc__)
    take, outdir, *spec = sys.argv[1:]
    take, outdir = Path(take), Path(outdir)
    ids = [s.rsplit(":", 1)[0] for s in spec]
    words = [int(s.rsplit(":", 1)[1]) for s in spec]
    want = len(ids)

    db, duration = envelope(take)
    segs, floor, peak = segments(db)
    kept = [(s, e) for s, e in segs if e - s >= MIN_SPEECH]

    print(f"{take.name}: {duration:.1f}s | floor {floor:.1f} dB | speech {peak:.1f} dB "
          f"| gap {peak - floor:.1f} dB")

    if len(kept) > want:
        # A false start, a cough, or saying "next" out loud leaves an extra
        # segment set off by a gap far larger than the deliberate pauses. Take
        # the run of `want` consecutive segments whose gaps are most uniform:
        # that run is the reading, and anything outside it was not part of it.
        best, tightest = 0, None
        for i in range(len(kept) - want + 1):
            window = kept[i:i + want]
            widest = max((window[k + 1][0] - window[k][1] for k in range(want - 1)),
                         default=0.0)
            if tightest is None or widest < tightest:
                best, tightest = i, widest
        outside = kept[:best] + kept[best + want:]
        print("  ignored outside the reading: "
              + ", ".join(f"{s:.2f}-{e:.2f}" for s, e in outside)
              + f"  (widest gap kept: {tightest:.2f}s)")
        kept = kept[best:best + want]

    if len(kept) != want:
        print(f"  REFUSING: found {len(kept)} segments, need {want}. Re-record this take.")
        for k, (s, e) in enumerate(kept, 1):
            print(f"    {k:2d}. {s:6.2f} -> {e:6.2f} ({e - s:5.2f}s)")
        return 1

    rows, bad = [], []
    for (s, e), cid, count in zip(kept, ids, words):
        per = (e - s) / count
        suspect = not (MIN_PER_WORD <= per <= MAX_PER_WORD)
        bad.append(cid) if suspect else None
        rows.append((cid, s, e, per, suspect))
        print(f"  {cid:32s} {e - s:5.2f}s  {per:.2f} s/word"
              + ("  <-- does not match its sentence" if suspect else ""))

    if bad:
        print(f"  REFUSING: {bad} do not match their sentence lengths. Re-record this take.")
        return 1

    outdir.mkdir(parents=True, exist_ok=True)
    for cid, s, e, _, _ in rows:
        subprocess.run(
            ["ffmpeg", "-hide_banner", "-v", "error", "-y", "-i", str(take),
             "-ss", f"{max(0.0, s - PAD):.3f}", "-t", f"{(e - s) + 2 * PAD:.3f}",
             "-c", "copy", str(outdir / f"{cid}.wav")], check=True)
    print(f"  wrote {len(rows)} clips to {outdir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
