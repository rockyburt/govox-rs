# Changelog

Notable changes to govox, in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
form. This project follows [semantic versioning](https://semver.org/spec/v2.0.0.html);
before 1.0.0, minor versions may change behaviour.

## [Unreleased]

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
- **The eval can score the streaming decode path**, which is the one dictation runs and
  which nothing had ever measured. `GOVOX_EVAL_STREAMING=1` feeds each clip through
  `OnlineProcessor` in frames and reproduces the daemon's duty-cycle throttle, reporting
  decode count alongside decode time.

  It is about **twice as bad as the published figures**. Same 29 clips, `model = "small"`:
  the utterance path scores raw WER 0.124 and 24/27 term recall, the streaming path
  **0.247 and 19/27** — and it drops whole words rather than mangling them. Every accuracy
  number in this project until now described a code path the daemon does not take. This is
  a measurement, not a fix; the LocalAgreement gap it exposes is still open.

  `GOVOX_EVAL_CADENCE_S` pins the decode schedule so accuracy repeats between runs. Left
  unpinned, GPU jitter changes how many decodes a clip gets and three identical runs scored
  0.253, 0.247 and 0.248.

  Most of that gap is closed under Fixed, below; the measurement is what found it.
- **`stream_trace`**, which replays one clip through the streaming path and prints every
  window, every word the model returned with its timestamps, and what the agreement buffer
  did with it. An aggregate score says a word is missing; this says where it went.

### Fixed

- **Streaming dictation no longer drops words out of the middle of a sentence.** "I need to
  stop at the store" was landing as "I need to at the store" — not a mangled word, a
  missing one, on audio the model transcribes perfectly in a single pass.

  whisper.cpp's word timestamps move between decodes of overlapping windows: the same
  "the" was reported at 1.28–1.57 s in one decode and 0.80–0.99 s in the next. The commit
  filter discarded anything starting more than 0.1 s behind the commit point as
  already-typed, so a word that had never been committed was thrown away — permanently,
  since its audio stays in the window but the word is never offered again. A second defect
  compounded it: the repeat guard only looked five words back, and stopped working once a
  sixth word was committed while the window still held it.

  The already-committed prefix is now identified by matching text against the committed
  tail — longest match first, no length cap, case and edge punctuation ignored — with
  timestamps demoted to a sanity check at the join. On the corpus at a pinned cadence:
  **raw WER 0.227 → 0.160, corrected 0.200 → 0.124, term recall 19/27 → 22/27**, and no
  word is duplicated on any of the 29 clips.

- **Long dictation sessions no longer type words twice or lose the words after a trim.**
  Two defects that only exist once the audio buffer passes `buffer_trimming_sec`, so no
  single corpus clip could reach them — every clip is under 8 s against a 10 s limit.

  A session dictated as "but the pipeline runs in GitLab" came out as "but **the the**
  pipeline **runs runs** in GitLab". The already-committed prefix was matched by requiring
  the whole committed region to line up from its first word, and the model revising any one
  word behind the commit point broke it — "Demir" became "Demerr" six words back. Only the
  last few committed words are matched now, and where they land in the hypothesis is
  searched for rather than assumed.

  Separately, trimming cut flush at the last committed word, leaving the *uncommitted*
  words after it to be re-decoded with no run-up: "runs in GitLab" was correct before a
  trim and came back from the 1.1 s fragment afterwards as "In GitHub", then never
  recovered. The cut now stays 2 s behind the commit point, yielding to a backstop that
  keeps the buffer inside its limit.

  Long-session word error against a one-pass decode of the same audio: **0.081 → 0.032**,
  at no measurable decode cost. The corpus improved too — **raw WER 0.160 → 0.146,
  corrected 0.124 → 0.115** — because the tail match is more robust on short clips as well.

  `stream_trace` now takes a comma-separated list of clips and joins them into one session,
  which is what made both of these visible.

- **Five recurring mis-hearings now corrected**: `Glovertown` arriving as "Govertown",
  `Lewisporte` as "Lewisworth", `Jira board` as "Gerobord", `BuildingStack` as
  "BuildingSack", `Rentsync` as "Durensync". These are personal-dictionary `replace` rules,
  so they live in `~/.config/govox/dictionary.toml` rather than in this repository.

  The dictionary's standard for a rule is "the same thing wrong the same way more than
  once", and until now that was untestable — the streaming decode did not repeat between
  runs, so every observation was a sample of one. Pinning the decode schedule
  (`GOVOX_EVAL_CADENCE_S`) fixed that: each string above was produced *identically* at three
  different cadences, and all but one on the whole-utterance path as well.

  Streaming corrected WER 0.115 → **0.094**, term recall 22/27 → **27/27**; utterance
  0.091 → **0.082**, 24/27 → 26/27. Raw WER unchanged on both paths, which is the point —
  a rule runs after recognition and cannot flatter the recogniser.

  A sixth rule fixes `cached` → "cash" as a **phrase** (`cash version`), because `cash` is
  an ordinary word and a bare rule would rewrite "I paid cash for it"; whole-word matching
  applies to the entire source, so only the phrase matches. Streaming corrected WER falls
  further to **0.089**.

  `Appleton` → "Hamilton", the other long-standing failure, turned out to need no rule at
  all: it was `large-v3-turbo` behaviour and does not occur under `small`, the configured
  model since 2026-08-16. A rule for it would have risked a real place name to fix nothing.

  Adding these terms to the *bias* prompt instead was measured and rejected — it fixed
  individual clips and cost more elsewhere, taking raw WER 0.146 → 0.160 (`Jira board`) and
  0.146 → 0.156 (`cache`).

- **Swapping keyboards no longer silently stops dictation.** Keyboards were enumerated once
  at startup. Unplugging one ended its reader with a "keyboard disconnected" warning and
  nothing else; plugging one in was never noticed at all. The daemon carried on looking
  perfectly healthy — running, model loaded, tray normal — while being unable to see the
  activation key, and the only way back was a restart.

  A supervisor now rescans when a reader dies and when the contents of `/dev/input` change,
  so a keyboard plugged in mid-session is picked up within about a second. Verified against
  a real device created through `/dev/uinput`: appearance detected in 0.1 s, removal logged
  with the device path, and the other keyboards unaffected.

  Startup still fails hard when no keyboard can emit the activation key — that is usually a
  missing `input` group and worth stopping for — but a keyboard going away later is now a
  warning, because it can come back.

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

- **The recogniser reuses one `WhisperState` instead of allocating per decode**, cutting a
  streaming decode from 0.240 s to 0.215 s. Because captions cannot arrive faster than a
  decode completes, that is visible as words keeping up rather than as a GPU statistic.

  It is not behaviour-neutral. Given identical windows, 6 of 29 clips now decode to
  different text — a small net gain here (raw WER 0.242 → 0.227, corrected 0.229 → 0.200,
  term recall 20/27 → 19/27), reproducible run to run, but decode output now depends on
  decode history. The worker also leaks the model at shutdown rather than freeing it:
  freeing raced process exit and segfaulted inside the Vulkan driver.

  Three other candidates were measured and **rejected**: `temperature_inc = 0.0` (~5%
  faster, costs 0.035 WER and four terms), raising `n_threads` (no effect on a GPU build),
  and scaling `audio_ctx` to the window — which gave no speedup at all and aborts the
  process through whisper.cpp's DTW assert. See `docs/guides/accuracy-eval.md`.

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

- **`tools/model-sweep.sh`** scores every candidate model against the eval corpus.
  The switch to `large-v3-turbo` was made on one bad transcription at ~5.5× the decode
  cost, and had never been checked. It holds up on word error rate — turbo is the most
  accurate at 0.067 — but **`small` has better term recall (24/27 against 22/27) at half
  the decode time**, getting `Appleton`, `Gander` and `Rentsync` right where turbo does
  not. Turbo substitutes "Hamilton" for `Appleton`: a stronger language model overriding
  rare proper nouns with common ones.

  The sweep also overturned two claims in the model guide: bigger is not monotonically
  better (`base.en` is worse than `tiny.en`, `medium.en` worse than `small`), and the
  `.en` builds are not reliably better than their multilingual twins — plain `small` beats
  `small.en` on accuracy at identical decode cost. Both claims were inherited rather than
  measured.
- **The recognition bias prompt is a sentence, not a word list.** Whisper reads
  `initial_prompt` as the transcript preceding the audio, and `Newfoundland Labrador Gander
  Gambo …` is unlike anything it was trained on. The same terms wrapped in a sentence take
  raw WER from 0.128 to **0.097** and corrected WER from 0.075 to **0.067** on the eval
  corpus — two clips better, none worse, no term added or removed. Comma-separating them
  undoes the gain, so the list stays space-joined.
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
