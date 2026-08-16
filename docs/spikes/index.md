---
last_verified: 2026-08-16
owner: rockyburt
type: Index
---

# Feasibility spikes

Probes run before any implementation code, each gating a decision that would have been
expensive to discover late. The probe crates themselves are in `spikes/`; these are the
written results.

## M-1 — before the port

- **[m-1a-whisper-rs.md](m-1a-whisper-rs.md)** — whether whisper-rs can supply per-word
  timestamps (`dtw_token_timestamps`, per-model `dtw_aheads` presets) and `no_speech_prob`,
  which streaming's LocalAgreement-2 needs; plus decode-option parity against
  faster-whisper's `compression_ratio_threshold` / `log_prob_threshold` /
  `condition_on_previous_text`, and why GPU stopped being optional.
- **[m-1b-ibus-zbus.md](m-1b-ibus-zbus.md)** — the undocumented `IBusText` / `IBusAttrList`
  GVariant layouts recovered byte-exact, `RegisterComponent` from a hand-built variant,
  `IBUS_ADDRESS` bus discovery that libibus was hiding, a fourth silent-success trap, and
  GNOME's refusal of per-context engine selection.
- **[m-1c-silero-vad.md](m-1c-silero-vad.md)** — reproducing the Python wrapper's
  probability sequence to 1e-4 with Silero v5's explicit `state` tensor carried between
  calls, and the ONNX Runtime linking story that decides whether a `.deb` is shippable.

## M-2 — a second recognition engine

- **[m-2a-sherpa-onnx-coexistence.md](m-2a-sherpa-onnx-coexistence.md)** — whether
  `sherpa-onnx` (the route to NVIDIA Parakeet) can share a binary with the `silero` VAD.
  It does **not** use `ort`, so the two carry separate ONNX Runtimes: 1.28.0 against
  ~1.24. Static linking dies on duplicate `onnx::*` protobuf symbols; `features =
  ["shared"]` links and runs, at the cost of `libsherpa-onnx-c-api.so` on the install path
  and a 193 MB prebuilt fetch — giving up the self-contained binary the current design
  bought.
- **[m-2b-sherpa-vad-parity.md](m-2b-sherpa-vad-parity.md)** — whether sherpa's own Silero
  VAD could replace the `silero` crate and get back to one runtime. It cannot: neither the
  safe API nor the C API exposes a per-window probability, so m-1c's 1e-4 comparison has
  nothing to compare; `SileroVadModelConfig` has a single `threshold` where `[vad]` needs
  speech/silence hysteresis; and it wants `silero_vad.onnx` on disk, so the self-contained
  binary is lost either way.
