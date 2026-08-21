---
last_verified: 2026-08-21
owner: rockyburt
type: Index
---

# Guides

Task-oriented guidance that is too detailed for the README.

- **[optimal-setup.md](optimal-setup.md)** — the IBus preedit mode versus the `ydotool`
  and clipboard fallback: single-vs-double-tap Escape, Enter ordering around the commit,
  `ydotool type` and emoji, the `/dev/uinput` udev rule and root `ydotoold`, and what
  `[ime] enabled`, `[injection] method`, `[audio] device`, `read_focused_field` and
  `command_mode` each do. Configured versus in effect, via About and `govox doctor`.
- **[models.md](models.md)** — picking between `tiny`/`base`/`small`/`medium`/`large-v3-turbo`
  and their `.en` variants, the ~20× decode-cost spread and what it does to streaming preview
  cadence, timing your own hardware with `times_the_configured_model`, and why `gpu_device`
  differs between the Vulkan and CUDA builds.
- **[accuracy-eval.md](accuracy-eval.md)** — measuring word error rate and per-term recall
  with `tools/record-eval.sh` and `corpus/eval/manifest.toml`, why the audio is gitignored
  while the scores are tracked, the `say`/`expect` split separating what was spoken from
  what should land in the document, and reading the raw-versus-corrected gap that says
  whether the personal dictionary still earns its place.
