# govox-rs

Speech-to-text dictation for Linux desktops, built for Wayland. Press a key, speak, and
the words appear in whatever application has focus — as underlined provisional text that
revises itself while you talk, then commits when you stop.

Everything runs locally. Audio never leaves the machine, and the daemon works with no
network once the model is cached.

```
┌──────────────┐   ┌─────┐   ┌─────────┐   ┌────────────┐   ┌───────────────┐
│  microphone  │──▶│ VAD │──▶│ Whisper │──▶│ correction │──▶│ focused field │
└──────────────┘   └─────┘   └─────────┘   └────────────┘   └───────────────┘
```

## What it does

- **Dictates into any application.** Text goes in through the input method as provisional
  (underlined) text where the toolkit supports it, and through synthetic keystrokes where
  it does not — so it reaches Chrome, terminals and Electron apps alike.
- **Shows the words as you speak them.** A streaming recogniser commits words once two
  successive decodes agree, so the on-screen text firms up instead of flickering.
- **Corrects as it goes.** Spoken punctuation ("comma", "new paragraph"), numbers and
  units ("twenty five dollars" → "$25"), spoken emoji, filler removal, sentence casing,
  and a personal dictionary for names the model keeps mangling.
- **Takes editing commands.** "delete that", "capitalize that", "undo" — and a command
  mode where nothing is typed unless it matches a command, so a misheard instruction
  cannot land in your document as text.
- **Stays out of the way.** A tray icon, a small HUD that follows the caret, and optional
  chimes. Every one of those degrades independently; none can stop dictation.

## Requirements

- **Linux with a Wayland session.** GNOME is what it is developed against. The HUD needs
  XWayland; without it the overlay is skipped and everything else works.
- **A microphone**, via PipeWire or ALSA.
- **[`ydotool`](https://github.com/ReimuNotMoe/ydotool)** for keystroke injection, with its
  daemon running and your user able to reach it. Wayland has no other way in.
- **A GPU is strongly recommended.** CPU decoding works and is far slower — see
  [Choosing a model](#choosing-a-model).
- Optional: **IBus** for underlined provisional text, **AT-SPI** for reading the focused
  field so `delete that` can verify what it is about to remove.

Build-time: a stable Rust toolchain and `libasound2-dev`.

## Install

```bash
git clone https://github.com/rockyburt/govox-rs && cd govox-rs
cargo build --release          # Vulkan GPU backend, the default
sudo cp target/release/govox target/release/govox-overlay /usr/local/bin/
```

whisper.cpp selects its accelerator at **compile time**, so pick the build that matches
your machine:

```bash
cargo build --release                        # Vulkan — works on AMD, Intel and NVIDIA
cargo build --release --features cuda        # NVIDIA via CUDA; needs the CUDA toolkit
cargo build --release --no-default-features  # CPU only
```

`[recognition] device = "cuda"` on a CPU-only build is a **startup error** that names the
fix, never a silent fallback — a daemon quietly running an order of magnitude too slow is
worse than one that refuses to start.

### Check the machine can support it

```console
govox doctor      # every subsystem, what is missing, and what would fix it
govox devices     # microphones, as govox sees them
govox keys        # prints key names as you press them, for [activation]
```

`doctor` is the fastest way to find a missing `ydotool` daemon or an inaccessible
`/dev/input`.

## Run it

```console
govox run
```

Then press your activation key — by default, tap **Scroll Lock** twice — and speak. Tap
again to stop; the corrected text is committed as one edit.

To run it as a user service:

```bash
cp packaging/systemd/govox.service ~/.config/systemd/user/
systemctl --user enable --now govox
```

## Configure

Settings live in `~/.config/govox/config.toml` and are layered over the built-in
defaults, so you only write the keys you are changing. Every default and its reasoning is
in [`config/default.toml`](config/default.toml) — it is meant to be read.

A small, useful starting point:

```toml
[recognition]
model = "small.en"        # see below
gpu_device = 0

[activation]
mode = "double_tap"       # or "toggle", or "push_to_talk"
toggle_key = "KEY_RIGHTCTRL"

[streaming]
enabled = true            # show words while you speak, rather than only at the end

[ime]
enabled = true            # underlined provisional text instead of typed-then-corrected
```

Unknown keys are rejected at startup rather than ignored, so a typo tells you immediately
instead of silently doing nothing.

### Choosing a model

Models are GGUF builds of Whisper, fetched from Hugging Face on first use and cached, from
`tiny` up to `large-v3-turbo`. Decode cost varies by roughly 20× across that range and is
the main thing to tune — `small.en` is a good default. For the full comparison, and how to
time the options on your own hardware, see the guides indexed from
[docs/index.md](docs/index.md).

## Design

Nine crates, with dependencies running strictly one way. `govox-core` holds the domain
types, the config, and the whole correction and editing pipeline, and depends on **no**
OS binding, no async runtime and no sibling crate — CI enforces this. Everything that
touches the outside world sits behind a trait it defines, which is what lets the logic be
tested with recording fakes and no hardware.

Two choices are unusual enough to flag here: nothing uses GObject introspection, so there
is no GLib main loop and no GTK anywhere; and the HUD runs as a separate process, so that a
crash in the least-tested code in the project cannot take dictation down with it.

For the full directory layout, the crate layering and the path a spoken word takes, see
[ARCHITECTURE.md](ARCHITECTURE.md).

## Status

Usable daily, and used that way. The whole pipeline works: capture, VAD, recognition,
streaming, correction, editing commands, injection, IBus preedit, AT-SPI field reading,
tray, notifications and the HUD.

Rough edges worth knowing before you rely on it:

- **Only tested on one desktop.** GNOME on Wayland, one distribution, one pair of GPUs.
  Anything else is unexplored rather than known-broken. The exact machine, and the full
  list of what has never been run, is recorded in the reference section of
  [docs/index.md](docs/index.md).
- **Whisper hallucinates on silence**, answering with stock phrases like
  `www.github.com` or `Thank you for watching!`. There are guards at both ends of a
  session and a content check; a new variant may still slip through.
- **Word error rate is not yet measured** against a corpus.
- **`ydotool` is required** for the non-IBus path, which means a daemon running as root
  or a udev rule.

## Contributing

Issues and pull requests are welcome. Before opening a PR:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Tests that need real hardware or a downloaded model are `#[ignore]`d and do not run by
default:

```bash
cargo test --workspace -- --ignored
```

The parity ledger records every behavioural decision — what was kept, what was
deliberately changed, and why. It is the first place to look when something behaves
unexpectedly, and changes to behaviour are expected to update it. Find it, and the rest of
the written record, from [docs/index.md](docs/index.md).

## Prior work

govox-rs grew out of `govox-py`, an earlier private Python implementation by the same
author, which established the architecture and worked out most of the hard-won desktop
integration details. This is a clean-room rewrite rather than a translation, and it now
goes well beyond what the original did.

Some tooling in `tools/parity-gen/` and the `REFERENCE` file exist to diff behaviour
against that predecessor. They need a checkout that is not published here, so they will
not run for you — kept because they still serve the original author.

## Licence

MIT. See [LICENSE](LICENSE).
