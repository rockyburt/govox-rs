---
last_verified: 2026-08-14
owner: rockyburt
type: Index
okf_version: "0.1"
external_docs:
  - packaging/README.md
---

# govox-rs documentation

Start here. Design and directory layout live one level up in
[ARCHITECTURE.md](../ARCHITECTURE.md); this tree holds the decision record and the
pre-implementation evidence.

## In this directory

- **[parity.md](parity.md)** — the behavioural ledger: every govox-py behaviour marked
  ported, deliberately-changed or dropped, with the reason. Covers the three silent-success
  traps (`ydotool key` by name, synchronous `set_global_engine` deadlock,
  `PreeditFocusMode.COMMIT` typing into documents), character-vs-byte offsets,
  `collapse_repeated_words`, `NullNotifier`, CT2→GGUF model changes, and the
  compile-time-vs-runtime GPU device divergence.

## Sections

- **[guides/](guides/index.md)** — picking a Whisper model size and `.en` variant, the decode-cost
  spread and its effect on streaming preview cadence, timing your own hardware, and the
  `gpu_device` difference between the Vulkan and CUDA builds.
- **[reference/](reference/index.md)** — the exact machine every measurement came from
  (ThinkPad P1 Gen 7, RTX 4070, Intel Arc, Ubuntu 26.04, GNOME 50.1 Wayland, IBus 1.5.34),
  Vulkan device ordering and `gpu_device`, and the list of desktops, backends and languages
  govox has never run on.
- **[spikes/](spikes/index.md)** — the M-1 feasibility probes run before any Rust was
  written: whisper-rs word timestamps and DTW aheads, IBus GVariant layouts over zbus, and
  Silero v5 state-tensor probability parity against the Python wrapper.

Packaging notes live beside the packaging files rather than in this tree; they are
registered in this file's `external_docs` and linked from
[ARCHITECTURE.md](../ARCHITECTURE.md).
