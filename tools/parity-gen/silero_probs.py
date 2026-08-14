#!/usr/bin/env python3
"""Dump govox-py's Silero speech probabilities for one WAV, for M-1(c).

Reference generator: this runs against the *Python* implementation so the Rust
probe has something to be compared with. It deliberately reuses
``govox.audio.vad.load_silero`` rather than calling silero_vad directly, so
whatever govox-py actually feeds its ``VadSegmenter`` is what gets measured.

Run from the govox-py checkout:

    uv run --extra dev --extra vad python \\
        ../govox-rs/tools/parity-gen/silero_probs.py tests/fixtures/hello.wav
"""

from __future__ import annotations

import sys
import wave
from pathlib import Path

WINDOW = 512
SAMPLE_RATE = 16_000


def load_wav_16k_mono(path: Path) -> list[float]:
    """Mono, 16 kHz, float. Mirrors the Rust probe's loader exactly.

    Nearest-neighbour resampling on purpose: it is what
    ``govox.audio.capture.normalize_to_mono`` does, so the sample stream reaching
    the model is identical on both sides and any divergence is the model's.
    """
    with wave.open(str(path), "rb") as handle:
        channels = handle.getnchannels()
        width = handle.getsampwidth()
        rate = handle.getframerate()
        frames = handle.readframes(handle.getnframes())

    if width != 2:
        raise SystemExit(f"expected 16-bit PCM, got {width * 8}-bit")

    import array

    raw = array.array("h")
    raw.frombytes(frames)
    samples = [value / 32768.0 for value in raw]

    if channels > 1:
        samples = [
            sum(samples[i : i + channels]) / channels
            for i in range(0, len(samples) - channels + 1, channels)
        ]

    if rate != SAMPLE_RATE:
        ratio = rate / SAMPLE_RATE
        count = int(len(samples) / ratio)
        samples = [samples[min(int(i * ratio), len(samples) - 1)] for i in range(count)]

    return samples


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2

    from govox.audio.vad import load_silero
    from govox.domain import AudioFrame

    audio = load_wav_16k_mono(Path(sys.argv[1]))
    probability = load_silero()

    print(f"samples: {len(audio)} ({len(audio) / SAMPLE_RATE:.2f}s)")
    print(f"windows: {len(audio) // WINDOW}\n")
    print(f"{'win':>5}  {'t(s)':>8}  {'p_speech':>10}")

    for index in range(len(audio) // WINDOW):
        chunk = tuple(audio[index * WINDOW : (index + 1) * WINDOW])
        frame = AudioFrame(samples=chunk, sample_rate=SAMPLE_RATE, timestamp=0.0)
        value = probability(frame)
        print(f"{index:>5}  {index * WINDOW / SAMPLE_RATE:>8.3f}  {value:>10.6f}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
