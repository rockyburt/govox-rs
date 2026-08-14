---
last_verified: 2026-08-14
owner: rockyburt
type: Index
---

# M-1 spikes

Three feasibility probes run before any implementation code, each gating a decision that
would have been expensive to discover late. The probe crates themselves are in `spikes/`;
these are the written results.

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
