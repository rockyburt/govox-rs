# Changelog

Notable changes to govox, in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
form. This project follows [semantic versioning](https://semver.org/spec/v2.0.0.html);
before 1.0.0, minor versions may change behaviour.

## [Unreleased]

### Changed

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
