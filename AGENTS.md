# govox-rs — Agent Routing Map

A Wayland-first dictation daemon in Rust: microphone → VAD segmentation → Whisper
recognition → correction pipeline → ydotool/clipboard injection.

## The golden corpus is the contract

`corpus/correction.jsonl.gz` holds ~239k recorded calls — `{stage, args, out}` — and
`crates/govox-core/tests/correction_golden.rs` replays every one. `corpus/config-defaults.json`
does the same for every config key and default.

**A diff in either means govox's behaviour changed.** The only question is whether that was
intended. It is the project's largest safety net and it guards the code that most needs one:
pure logic, an enormous input space, and failures that are silent rather than loud — a
character-vs-byte offset or a stage reordering does not crash, it puts subtly wrong text in
someone's document.

If a change was intended, re-record and read the diff:

```bash
GOVOX_BLESS=1 cargo test -p govox-core --test correction_golden -- --ignored bless
GOVOX_BLESS=1 cargo test -p govox-core --test config_golden -- --ignored bless
```

Blessing recomputes answers for existing inputs and adds records for table-driven inputs not
yet covered, so a new spoken emoji or punctuation phrase gains coverage by being added to its
table. Unchanged records keep their original bytes, so the diff shows only real movement.
Never bless to make a red test green without reading what moved.

## Ground rules

1. **`govox-core` must never depend on `tokio`, an OS binding, or a sibling crate.** CI
   enforces this. It is what keeps the golden harness fast enough to run on every save.
2. **Record behavioural decisions in `docs/parity.md`** in the same change.
   It explains *why* the pipeline behaves as it does — including three cases where an API
   reports success and does nothing — and is the first place to look when something is
   surprising. An unexplained behaviour change is a bug; an explained one is a decision.
3. **Prior work.** govox grew out of an earlier, unpublished Python implementation, which is
   where the golden corpora and much of the reasoning in `docs/parity.md` originally came
   from. That is history, not process: nothing in this repository depends on it, and it is
   not available to you.

## Quick orientation

| Want to… | Start here |
|---|---|
| Understand what govox does, and run it | [README.md](README.md) |
| Understand the layout, crate layering and data flow | [ARCHITECTURE.md](ARCHITECTURE.md) |
| Know why a behaviour is the way it is | the decision record, via [docs/index.md](docs/index.md) |
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

**Use `cargo nextest` for tests.** It is what CI runs, it reports failures from
every binary in one pass instead of stopping at the first, and it is configured
in `.config/nextest.toml`.

```bash
# The inner loop. ~6s.
GOVOX_GOLDEN_SAMPLE=50 cargo nextest run --workspace

# Everything. ~2.5 minutes, and what a merge needs.
cargo nextest run --workspace

cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

The golden corpus replays ~239k recorded calls and is **99% of the run** — 144s
of ~146s — essentially all of it inside `fancy-regex`. So the only thing that
makes a test run fast here is sampling it; nothing else in the workspace is
slower than milliseconds, and parallelism has nothing to work with.

`GOVOX_GOLDEN_SAMPLE=50` takes every 50th record. The stride is not random: the
corpus is written in a stable order, so a stride hits every stage and every
config variant, and the same stride always selects the same records — a failure
found under sampling reproduces without it.

**A sampled run is not a corpus check.** It prints nothing to distinguish itself
from a full one, so sampling is left off by default and named explicitly when
wanted. Run the whole corpus before merging anything that touches
`crates/govox-core/src/correction/`.

```bash
# Doc tests: nextest does not run them at all, so this stays `cargo test`.
cargo test --workspace --doc

# Tests needing real hardware, a model or a desktop session are #[ignore]d.
# Run them where you have the hardware. Exclude the bless tests: they are
# ignored too and fail *by design* without GOVOX_BLESS=1, which is what stops a
# stray --run-ignored rewriting a corpus.
cargo nextest run --workspace --run-ignored ignored-only -E 'not test(bless)'
```

Two of those need the desktop to themselves: `govox-ime`'s live tests register
an IBus engine, so they fail while a `govox` daemon is running and already
holding that registration. Stop the unit first, or expect that one to fail.

CI only *lists* the ignored tests (`cargo nextest list --run-ignored
ignored-only`), so a test that silently stops existing is still visible without
CI needing a microphone.

## Conventions

- **Character offsets, never byte offsets.** Every offset in the editing and span code is a
  code-point index, and AT-SPI reports characters too. Use `CharIdx` and `chars().count()`;
  `str::len()` on user text is a bug.
- **Optional capabilities are trait methods with default impls**, not runtime probes.
- **Make failure modes unrepresentable where the API allows it.** Three desktop APIs here
  *report success and do nothing*; each is encoded as a type that cannot express the
  mistake, with the negative test kept as documentation of why. See the "silent success"
  entries in `docs/parity.md`.
- **No GLib main loops.** The tray, IBus and AT-SPI are all reached over D-Bus, which is
  the single largest simplification the rewrite buys. Do not reintroduce one.
- **The overlay stays a separate process**, so that a crash in the least-tested code in
  the project cannot take dictation down.
