# Changelog

Notable changes to govox, in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
form. This project follows [semantic versioning](https://semver.org/spec/v2.0.0.html);
before 1.0.0, minor versions may change behaviour.

## [Unreleased]

### Added

- **An About submenu in the tray**, reporting the version and licence alongside the facts
  that actually decide how govox behaves: model, backend and GPU index, injector, preedit
  and field reading. Each says whether the feature is *in effect*, not merely configured —
  so a GPU build running on the integrated card, an IBus engine that never registered, or a
  failed AT-SPI connection is now visible in the menu instead of only in the journal.
  Built from `ksni`, which was already a dependency: no new crate, no second process, and
  no GLib main loop.
- **Spoken symbol names**, so an email address or a path can be dictated at all:
  "rocky at sign gmail dot com" → `rocky@gmail.com`, "usr forward slash local" →
  `usr/local`. 21 phrases in total — `at sign`, `dot`, `slash`, `backslash`,
  `underscore`, `ampersand`, `asterisk`, `tilde`, `hashtag`, braces, and the `<x> sign`
  forms. Words that are also ordinary English (`at`, `plus`, `equals`, `star`, `pound`)
  are deliberately **not** accepted bare.
- **Spoken case control**, off by default under `[correction] case_control`:
  "all caps hello" → `HELLO`, plus `caps <word>`, `no caps <word>`, and an on/off span
  form. A span ends with the utterance that opened it, so there is no mode to get stuck in.
- **`govox commands`**, which lists every phrase govox understands, says which groups are
  switched on, and names the setting that would enable one that is off. Generated from the
  grammar tables, so it cannot drift from the behaviour.
- **An accuracy eval**, which is what 0.1.0's "word error rate is not measured" limitation
  needed. `tools/record-eval.sh` records a 29-clip corpus and
  `cargo test -p govox-asr --test eval -- --ignored` scores the configured model, reporting
  word error rate, per-term recall and the gap between raw and corrected output — that gap
  being what the personal dictionary is worth, which nothing measured before. The corpus is
  built from transcriptions that actually failed, so it is a regression net rather than a
  guess at what is hard. The recordings stay out of git; the manifest and scores are
  tracked. See [docs/guides/accuracy-eval.md](docs/guides/accuracy-eval.md). **No figure is
  published yet** — the harness exists, the clips are not recorded, and quoting a number
  before measuring one is the habit this replaces.
- **Activation keys accept a list**, so `toggle_key = ["KEY_LEFTCTRL", "KEY_RIGHTCTRL"]`
  works and a shortcut no longer has to pick a side of the keyboard. Listed keys share one
  double-tap timer, so left-then-right counts as a double tap.
- **`--version` and the About submenu report the build**, not the manifest:
  `0.1.0+14.a18ad6e` when past a tag, plain `0.1.0` on one. Semver build metadata, so it
  ranks equal to the release rather than below it — which `git describe`'s own
  `0.1.0-14-g…` shape would not, being a prerelease.

### Fixed

- **Dictating twice into a terminal no longer runs the utterances together.** `…it does
  now.this is fun!` — `TERMINAL` is a verbatim purpose, so the pipeline returned before the
  separating space was ever considered. Standing prose rules down and joining utterances
  were one flag doing two jobs; a terminal wants the first and not the second, while a URL
  bar wants both, so that `example` + `dot com` still makes `example.com`.
- **`Ctrl+C` twice in a terminal no longer starts dictation.** Double-tap counted any two
  presses of the key inside the window, so a repeated shortcut — two Controls with a `C`
  between them — read as the gesture. An ordinary key pressed after a tap now cancels it;
  modifiers do not, so `Ctrl+Shift+…` is unaffected. Found when binding Control made it
  reachable, and it is how a running command is interrupted.
- **An emoji no longer fails the whole utterance where `wl-copy` is not installed.** The
  emoji router sent pictographic text to the clipboard without checking there was one, and
  the fallback pasted with `ydotool key ctrl+v` — depending on the backend it existed to
  cover for. A ydotool-only session now types what it can, drops what it cannot,
  renormalises the spacing the removal leaves behind, and says so every time. With neither
  backend available the About row reads `nothing available` instead of naming a working one.
- **The About submenu says what is true.** The licence is read from the manifest rather
  than a literal that could drift; the injection row reports the backend that actually
  carried the text, not the one chosen at startup, so a silent clipboard fallback is
  visible; and the GPU index is labelled `requested`, because whisper.cpp takes it and
  reports nothing back.

- **The HUD no longer sits under the desktop panel.** X11 reports a monitor as its full
  physical rectangle, which takes no notice of panels, so the card was placed 24 px from the
  top of the screen and the GNOME top bar — 45 px on the machine this was reported from —
  covered the top 21 px of it. Placement now respects `_NET_WORKAREA`, which fixes both the
  configured corner and the follow-the-caret position. Where no work area is published, or
  it does not cover the monitor in question, the card falls back to its previous placement.
- **Spoken emoji reached the document.** `ydotool` types by emulating keycodes and no
  keycode produces an emoji, so `ydotool type 👍` exited 0 and typed nothing — meaning
  `[correction] spoken_emoji` looked broken whenever it was switched on. Text containing an
  emoji is now put on the clipboard and pasted. Accented and non-Latin text is unaffected
  and still typed.
- Removed a pointer to `docs/reference/commands.md`, a file that never existed, from the
  `spoken_emoji` comment in `config/default.toml`. `govox commands` is what it should have
  pointed at.

### Changed

- **The default activation shortcut is now double-tapping either Control**, replacing
  `toggle` on `KEY_SCROLLLOCK` — a key most keyboards no longer have somewhere reachable.
  The mode and the key are one decision: Control is pressed constantly, so `toggle` on it
  would start dictation on every copy, and double-tap is what makes an everyday key safe to
  bind. Existing configs are unaffected; `toggle_key` still accepts a single name.
- **Input devices are identified by their backend id rather than their label.**
  `govox devices` now prints the id in brackets — `hw:CARD=Microphones,DEV=0` — and
  `[audio] device` prefers it. Labels turned out to be neither unique nor stable: one
  machine lists five devices all called "Blue Microphones, USB Audio". A label is still
  accepted where it is unambiguous, so existing configs keep working.
- `govox devices` lists considerably more entries than before, because the audio backend
  now enumerates every ALSA PCM variant rather than a summarised set. The bracketed id is
  what tells them apart.

### Internal

- Migrated to cpal 0.18, which split the old `Device::name()` into `Display` for labels and
  `DeviceId` for identity.
- **Tests run under `cargo nextest`**, which is what CI runs and what `AGENTS.md` now
  documents. The golden corpus is 144 s of a 146 s run, so `GOVOX_GOLDEN_SAMPLE=50` — a
  strided sample hitting every stage — is the inner loop at ~6 s, while the full corpus is
  what a merge needs.

## [0.1.0] — 2026-08-14

First release. govox dictates into any application on a Wayland desktop: press a key,
speak, and the words appear in whatever has focus. Everything runs locally — audio never
leaves the machine, and the daemon works with no network once the model is cached.

### Added

- **Dictation into any application.** Text goes in through IBus as provisional (underlined)
  text where the toolkit supports it, and through synthetic keystrokes via `ydotool` where
  it does not, so it reaches GTK, Qt, terminals and Electron alike.
- **Streaming recognition.** Words are committed once two successive decodes agree
  (LocalAgreement-2), so on-screen text firms up instead of flickering.
- **Correction pipeline.** Spoken punctuation, numbers and units ("twenty five dollars" →
  "$25"), spoken emoji, filler removal, sentence casing, and a personal dictionary for
  names the model mishears.
- **Editing commands.** "delete that", "capitalize that", "undo", and a command mode in
  which nothing is typed unless it matches a command — so a misheard instruction cannot
  land in a document as text.
- **Password-field refusal.** Anything spoken into a password field is discarded without
  being typed, routed, or shown as provisional text; the check runs before the action is
  routed, so a misheard "command mode" cannot change govox's state either.
- **Desktop feedback.** A tray icon, a HUD that follows the caret with per-application
  offset rules, notifications, and chimes. Each degrades independently and none can stop
  dictation.
- **CLI.** `govox run`, plus `doctor` (reports every subsystem and what would fix it),
  `devices` and `keys`.
- **Configuration** in `~/.config/govox/config.toml`, layered over documented built-in
  defaults. Unknown keys are rejected at startup rather than ignored.
- **Model management.** GGUF Whisper models from `tiny` to `large-v3-turbo` are fetched on
  first use and cached, with offline and cache-first policies.

### Known limitations

- **Tested on one desktop only** — GNOME 50 on Wayland, Ubuntu 26.04, one pair of GPUs.
  Other desktops, X11 sessions and distributions are unexplored rather than known-broken.
  `docs/reference/environments.md` records exactly what has and has not been run.
- **Word error rate is not measured.** No corpus with reference transcripts exists yet, so
  accuracy claims are deliberately absent.
- **Whisper hallucinates on silence**, answering with stock phrases such as
  `www.github.com`. There are guards at both ends of a session plus a content check, but a
  new variant may still slip through.
- **`ydotool` is required** for the non-IBus path, which means its daemon running with
  access to `/dev/uinput`.
- **The GPU backend is a compile-time choice.** Vulkan is the default; CUDA and CPU-only
  are separate builds. `device = "cuda"` on a CPU build is a startup error naming the fix,
  never a silent fallback — but the CUDA and CPU builds have not been exercised.
- **Streaming preview cadence is bounded by decode time**, so a large model makes the live
  text arrive in larger, less frequent jumps.

### Release artifacts

`govox` and `govox-overlay` for x86_64 Linux, built on Ubuntu 24.04 with the Vulkan
backend so the glibc floor is 2.39 rather than the build host's own. Requires
`libvulkan1`, `libasound2`, `libstdc++6` at runtime, plus `ydotool` and `wl-clipboard`.
Reproduce with `tools/build-release.sh`.

Whisper models are **not** bundled; the first run downloads the configured model from
Hugging Face, so it needs network and several GB of disk once.

[Unreleased]: https://github.com/rockyburt/govox-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/rockyburt/govox-rs/releases/tag/v0.1.0
