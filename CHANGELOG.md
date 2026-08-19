# Changelog

Notable changes to govox, in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
form. This project follows [semantic versioning](https://semver.org/spec/v2.0.0.html);
before 1.0.0, minor versions may change behaviour.

## [Unreleased]

## [0.2.0] — 2026-08-19

The release that put numbers on itself. 0.1.0 shipped a working pipeline and said, in as
many words, that its accuracy was not measured. This one measures it — and the measurement
immediately found that every accuracy figure the project had published described a code
path the daemon does not take. Closing that gap is most of what is below.

The rest is the edit-and-try loop: saving a config or dictionary file now applies it, the
tray reports what is actually in effect rather than what was configured, and `govox
commands` lists every phrase govox understands.

### Added

- **An About submenu in the tray** — version, licence, model, backend and GPU index,
  injector, preedit, field reading. Each row reports what is *in effect*, not what is
  configured, so an IBus engine that never registered is visible in the menu.
- **Spoken symbol names**: "rocky at sign gmail dot com" → `rocky@gmail.com`, "usr forward
  slash local" → `usr/local`. 21 phrases. Words that are also ordinary English are not
  accepted bare.
- **Spoken case control**, off by default under `[correction] case_control`: "all caps
  hello" → `HELLO`, plus `caps`, `no caps`, and an on/off span that ends with its
  utterance, so no mode can stick.
- **`govox commands`**, listing every phrase govox understands and the setting that enables
  one that is off. Generated from the grammar tables, so it cannot drift.
- **An accuracy eval, and its first figures.** `cargo test -p govox-asr --test eval --
  --ignored` scores the configured model for word error rate, per-term recall, and the
  raw-versus-corrected gap. The clips come from transcriptions that actually failed.
  Recordings stay out of git; scores are tracked in `corpus/eval/baseline.json`.

  On the reference machine with `large-v3-turbo`: **raw WER 0.124, corrected 0.094**,
  term recall 20/27, mean decode 0.80 s. This supersedes 0.1.0's "word error rate is not
  measured" limitation.

  It also showed that **every personal-dictionary `replace` rule was dead** — not one
  fired on any clip. They were predictions of how a *different* model would mangle these
  words; the words now come out correctly on their own because they are biased. An
  ablation (`bias_prompt_token_budget = 0`) puts a number on which lever matters: term
  recall falls from 20/27 to 10/27 without bias.

  Acting on that took corrected WER from 0.094 to **0.075** and recall from 20/27 to
  **22/27** — mostly by biasing "ultra filtered milk", the phrase that prompted the eval
  in the first place, which had never been biased at all. Both its clips went from wrong
  to exact. See [docs/guides/accuracy-eval.md](docs/guides/accuracy-eval.md).
- **`tools/cut-take.py`**, for recording the eval corpus where `record-eval.sh` cannot
  run. That script prompts per clip, so it needs a TTY and exits immediately without one.
  This splits one continuous recording into clips instead, refusing to write unless every
  segment matches its expected sentence.
- **Activation keys accept a list**: `toggle_key = ["KEY_LEFTCTRL", "KEY_RIGHTCTRL"]`.
  Listed keys share one double-tap timer, so left-then-right counts as a double tap.
- **`--version` reports the build**, not the manifest: `0.1.0+14.a18ad6e` past a tag,
  `0.1.0` on one.

### Fixed

- **Two utterances into a terminal no longer run together** — `…it does now.this is fun!`.
  One flag governed both prose rules and utterance joining. A terminal needs prose rules
  off but the space kept; a URL bar needs neither, so `example` + `dot com` still makes
  `example.com`.
- **`Ctrl+C` twice in a terminal no longer starts dictation.** Double-tap counted any two
  presses inside the window, so a repeated shortcut read as the gesture. An ordinary key
  now cancels a pending tap; modifiers do not.
- **An emoji no longer fails the utterance where `wl-copy` is absent.** The router used the
  clipboard without checking there was one. A ydotool-only session now types what it can,
  drops what it cannot, and says so. With neither backend the About row reads
  `nothing available`.
- **The About submenu says what is true.** The licence comes from the manifest; the
  injection row names the backend that carried the text, not the one chosen; the GPU index
  is labelled `requested`, because whisper.cpp reports nothing back.
- **The HUD no longer sits under the desktop panel.** The GNOME top bar covered its top
  21 px, because X11 reports a monitor as its full rectangle. Placement now respects
  `_NET_WORKAREA`.
- **Spoken emoji reach the document.** No keycode produces an emoji, so `ydotool type 👍`
  exited 0 and typed nothing. Emoji now go by the clipboard.
- Removed a pointer to `docs/reference/commands.md`, a file that never existed, from
  `config/default.toml`.
- **The README no longer promises that a mistyped config key is caught.** An unknown
  *section* is rejected, but an unknown *key inside* one is accepted and ignored, so
  `beem_size` silently did nothing while the docs said it would be reported.

### Changed

- **Double-tapping either Control is the default shortcut**, replacing `toggle` on
  `KEY_SCROLLLOCK`. The mode and the key are one decision: Control is pressed constantly,
  so `toggle` on it would start dictation on every copy. Existing configs are unaffected.
- **Input devices are identified by backend id rather than label.** `govox devices` prints
  the id — `hw:CARD=Microphones,DEV=0` — and `[audio] device` prefers it, because labels
  are neither unique nor stable. An unambiguous label still works. The listing is longer
  now: the backend enumerates every ALSA PCM variant, and the id tells them apart.
- **Streaming is on by default.** Words now appear as provisional text while you speak,
  rather than only when the utterance ends. Set `[streaming] enabled = false` for the old
  behaviour.
- **`[recognition] compute_type` is gone.** It was a CTranslate2 setting that whisper.cpp
  cannot honour — quantization is baked into the GGUF file — so it never took effect. A
  config that still sets it keeps loading and now says so at startup. Point
  `[recognition] model_dir` at a quantized model instead.
- The `model` and `download_policy` defaults stay conservative — `small` and `offline` —
  and `config/default.toml` now explains why rather than leaving it to look like drift.

### Internal

- Migrated to cpal 0.18, which split `Device::name()` into `Display` and `DeviceId`.
- **Tests run under `cargo nextest`**, as CI does. The golden corpus is 144 s of a 146 s
  run, so `GOVOX_GOLDEN_SAMPLE=50` gives a ~6 s inner loop.
- **The streaming processor decodes through a trait**, `WordRecognizer`, replacing a dead
  `Recognizer` that nothing implemented. Its window trimming and timestamp arithmetic are
  now tested against a scripted recognizer instead of needing a loaded model — a wrong
  offset there does not crash, it silently drops words from the session. A second
  recognition engine becomes an added implementation rather than a rewrite.
- **A failed decode reports as `RecognitionFailed`**, not `InjectionRejected`. Model faults
  used to be logged as injection faults, sending a reader to the wrong subsystem.

### Known limitations

- **Streaming still trails a whole-utterance decode**, though no longer by much: corrected
  WER 0.089 against 0.082 on the same 29 clips under `small`. It was 0.247 against 0.124
  when first measured. The remaining gap is a LocalAgreement question and is open.
- **Tested on one desktop only** — GNOME on Wayland, Ubuntu 26.04, one pair of GPUs.
  Other desktops, X11 sessions and distributions are unexplored rather than known-broken.
  `docs/reference/environments.md` records exactly what has and has not been run.
- **Whisper hallucinates on silence**, answering with stock phrases such as
  `www.github.com`. There are guards at both ends of a session plus a content check, but a
  new variant may still slip through.
- **`ydotool` is required** for the non-IBus path, which means its daemon running with
  access to `/dev/uinput`.
- **The GPU backend is a compile-time choice.** Vulkan is the default; CUDA and CPU-only
  are separate builds, and neither has been exercised. `device = "cuda"` on a CPU build is
  a startup error naming the fix, never a silent fallback.
- **Streaming preview cadence is bounded by decode time**, so a large model makes the live
  text arrive in larger, less frequent jumps.
- **The accuracy corpus is personal.** Its 29 clips are transcriptions that failed on this
  machine, in one voice and one accent, and the recordings stay out of git. The scores are
  a regression signal for changes to this pipeline, not a claim about whisper in general.

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

[Unreleased]: https://github.com/rockyburt/govox-rs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/rockyburt/govox-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/rockyburt/govox-rs/releases/tag/v0.1.0
