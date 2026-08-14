# Architecture

How govox-rs is put together: the directory layout, the path a spoken word takes, and the
three structural rules that the rest of the design follows from.

For what the daemon *does*, see [README.md](README.md). For how it differs from its Python
predecessor and why, see the parity ledger indexed from [docs/index.md](docs/index.md).

## Directory layout

| Path | Holds |
|---|---|
| `crates/govox-core/` | Domain types, traits, config, correction, editing, spans, the VAD and activation state machines, the LocalAgreement buffer. No OS bindings, no async runtime, no sibling crate. |
| `crates/govox-audio/` | cpal capture with its reconnect supervisor, resampling, chime synthesis. |
| `crates/govox-vad/` | Silero speech probability over ONNX Runtime. |
| `crates/govox-asr/` | whisper-rs recognition, GGUF model fetch and cache policy, the streaming processor. |
| `crates/govox-input/` | evdev hotkey listening, `ydotool` and clipboard injection. |
| `crates/govox-ime/` | The IBus engine, hand-rolled over raw D-Bus. |
| `crates/govox-a11y/` | AT-SPI focus tracking and field reading, over raw D-Bus. |
| `crates/govox-ui/` | Tray (`ksni`), notifications, and the client half of the overlay protocol. |
| `crates/govox-daemon/` | Orchestration, the event loop, reload, diagnostics, telemetry. |
| `bin/govox/` | The CLI: `run`, `doctor`, `devices`, `keys`. |
| `bin/govox-overlay/` | The HUD renderer — x11rb and tiny-skia, its own process. |
| `config/default.toml` | Every default and the reasoning for it. Embedded with `include_str!`, so there is no runtime path to resolve. |
| `corpus/` | Golden corpora and the recorded baseline the parity tests replay. |
| `tools/parity-gen/` | Generators that extract the pinned govox-py and record its behaviour. Need a checkout that is not published. |
| `spikes/` | Throwaway probes from the pre-implementation spikes, kept as evidence. |
| `packaging/` | systemd units, IBus component XML, Debian notes. See [packaging/README.md](packaging/README.md). |
| `tests/` | Cross-crate fixtures. |

## The path of a spoken word

```
microphone ─▶ capture ─▶ VAD ─▶ segmenter ─▶ recogniser ─▶ correction ─▶ injection
  (cpal)      (ring     (silero) (state      (whisper)     (pipeline)    (IBus preedit
              buffer)             machine)                                or ydotool)
```

Capture runs on a blocking thread and feeds a lock-free ring buffer; everything downstream
is a tokio task. Recognition is the one CPU-bound stage, so the Whisper model lives on a
dedicated thread behind an mpsc channel with oneshot replies — `WhisperState` is not `Sync`,
and a thread-actor is what makes `transcribe` awaitable without sharing it.

When streaming is enabled the recogniser is driven continuously rather than per-utterance,
and words are committed only once two successive decodes agree (LocalAgreement-2). Un-agreed
words are shown as provisional text and can still change.

## Three structural rules

**`govox-core` depends on nothing.** Not tokio, not an OS binding, not a sibling crate. Every
other crate depends on it and nothing else; `govox-daemon` is the only crate that depends on
all of them. This is enforced in CI, not by convention — `.github/workflows/ci.yml` walks
`cargo tree` and fails the build on a forbidden edge. The reason is the differential parity
harness: it must stay runnable on any machine with no hardware and no desktop session, which
is what keeps it cheap enough to run on every save.

Everything that touches the outside world sits behind a trait `govox-core` defines
(`Recognizer`, `Corrector`, `Injector`, `PreeditSink`, `TextModel`), so the logic is tested
against recording fakes rather than hardware.

**No GObject introspection anywhere.** The tray, the IBus engine and AT-SPI all speak D-Bus
directly. There is no GLib main loop, no GTK, and no bridging to a system Python's PyGObject —
which is the single largest simplification the rewrite buys, and the reason the package has no
interpreter dependency. The GVariant layouts IBus expects are undocumented; they were
recovered from a running `ibus-daemon` and are written down in
[`crates/govox-ime/src/variant.rs`](crates/govox-ime/src/variant.rs).

**The overlay is a separate process.** Not for backend reasons — for blast radius. It is the
least-tested, most crash-prone code in the project, and out-of-process means an overlay crash
cannot take dictation down with it. The two halves speak a newline-delimited text protocol
(`show`/`hide`/`level`/`caption`/`anchor`/`compact` in, `stop` out), which also makes the
renderer drivable by hand for debugging.

## Shared state and reload

State that the tray, the pipeline and the correction stages all read is built *before* the
daemon and handed to each, rather than reached through a back-reference to the daemon. Config
and the personal dictionary live in an `ArcSwap`: readers do a wait-free load, and a reload
does one store, triggered by a message on a channel so the swap happens on the daemon's own
task.

## Degradation

Every desktop integration is optional and fails alone. No tray, no IBus, no AT-SPI, no
XWayland for the overlay — each of those degrades to a working dictation path rather than a
dead daemon. `govox doctor` reports which are present, and exits non-zero only on a genuine
failure, never on a degraded-but-working subsystem.
