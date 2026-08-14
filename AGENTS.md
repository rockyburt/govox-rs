# govox-rs — Agent Routing Map

A Wayland-first dictation daemon in Rust: microphone → VAD segmentation → Whisper
recognition → correction pipeline → ydotool/clipboard injection.

This is a clean-room rewrite of `../govox-py`, which is the **reference implementation**.

> `govox-py` is unpublished. If you are working in a checkout without it beside you, the
> parity harness and `tools/parity-gen/` cannot run — skip them, and treat
> `docs/parity.md` as the written record of what they would have told
> you. Nothing else in this file depends on having it.

## The reference is pinned

This port is a port of **one specific commit** of govox-py, named in
[`REFERENCE`](REFERENCE) — currently `3ad8c0fb` (2026-08-13, branch `develop`).

govox-py is under active development by someone else. The M2 parity harness generates its
expected values *by running govox-py*, so an unpinned reference would silently re-baseline
the corpus against whatever landed that afternoon, and a real divergence would look like a
passing test.

```bash
./tools/parity-gen/pinned-source.sh    # extract govox-py@REFERENCE (read-only)
PYTHONPATH=tools/parity-gen/.parity-src/src <interpreter> tools/parity-gen/<gen>.py
```

The extract uses `git archive`, which does **not** create a worktree, move HEAD, or write
anything under `govox-py/.git`. Never point a generator at the live checkout.

Moving the pin is deliberate and reviewable: bump the SHA, regenerate, read the diff.
A corpus diff *without* a pin change is a bug in govox-rs. A corpus diff *with* one is
govox-py's behaviour moving, and every hunk needs a decision in `docs/parity.md`.

## Ground rules

1. **Do not modify `../govox-py`.** It stays working and untouched until parity is
   reached — no edits, no commits, no branches, and no `git worktree add` against it. The
   only permitted interaction is reading, plus the `git archive` extract above. (If you
   create a worktree there by accident because your shell's cwd drifted, remove it.)
2. **Consult `govox-py` before inventing behaviour.** Nearly every non-obvious line there
   has a comment explaining which failure it prevents. Read the comment before deciding
   it is redundant.
3. **Update `docs/parity.md` in the same change** that ports, alters or
   drops a `govox-py` behaviour. An undocumented divergence is a bug; a documented one is
   a decision.
4. **`govox-core` must never depend on `tokio`, an OS binding, or a sibling crate.** CI
   enforces this. It is what keeps the differential parity harness fast enough to run on
   every save.

## Quick orientation

| Want to… | Start here |
|---|---|
| Understand what govox does, and run it | [README.md](README.md) |
| Understand the layout, crate layering and data flow | [ARCHITECTURE.md](ARCHITECTURE.md) |
| See what diverges from Python, and why | the parity ledger, via [docs/index.md](docs/index.md) |
| Read the M-1 feasibility results | [docs/spikes/index.md](docs/spikes/index.md) |
| Advise on model size, latency or `gpu_device` | [docs/guides/index.md](docs/guides/index.md) |
| Know what hardware or desktop a claim was verified on | [docs/reference/index.md](docs/reference/index.md) |
| See what shipped in a release, and its known limitations | [CHANGELOG.md](CHANGELOG.md) |
| Change domain types or traits | `crates/govox-core/src/domain.rs` |
| Change config schema | `crates/govox-core/src/config.rs`, `config/default.toml` |
| Change correction rules | `crates/govox-core/src/correction/` |
| Change injection | `crates/govox-input/` |
| Change recognition | `crates/govox-asr/` |
| Change the tray or notifications | `crates/govox-ui/` |
| Change orchestration or reload | `crates/govox-daemon/` |

## Testing

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check

# The correction parity test replays ~239k recorded calls and takes ~2 minutes,
# almost all of it inside fancy-regex. CI always runs the whole corpus; for a
# fast inner loop, take a strided sample instead (~3s, still hits every stage):
GOVOX_PARITY_SAMPLE=50 cargo test --workspace

# Tests needing real hardware, a model or a desktop session are #[ignore]d,
# mirroring govox-py's @pytest.mark.integration.
cargo test --workspace -- --ignored
```

## Conventions

- **Character offsets, never byte offsets.** Every offset ported from `govox-py` is a
  Python code-point index, and AT-SPI reports characters too. Use `CharIdx` and
  `chars().count()`; `str::len()` on user text is a bug.
- **Optional capabilities are trait methods with default impls**, not runtime probes.
  This is what replaces `govox-py`'s `hasattr`/`getattr` duck-typing across Protocol
  boundaries.
- **Make failure modes unrepresentable where the API allows it.** Three behaviours in
  `govox-py` guard against calls that *report success and do nothing*; each is encoded
  here as a type that cannot express the mistake, with the negative test kept as
  documentation of why. See the "silent success" entries in the parity ledger.
- **No GLib main loops.** The tray, IBus and AT-SPI are all reached over D-Bus, which is
  the single largest simplification the rewrite buys. Do not reintroduce one.
- **The overlay stays a separate process**, so that a crash in the least-tested code in
  the project cannot take dictation down.
