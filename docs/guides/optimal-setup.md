---
last_verified: 2026-08-21
owner: rockyburt
type: Guide
covers:
  - crates/govox-ime/
  - crates/govox-input/
  - crates/govox-a11y/
  - crates/govox-asr/
  - config/default.toml
---

# The optimal mode, and the one you fall back to

govox has two working shapes, and which one you are in is decided by a single question:
**does text reach the document through an input method, or through synthetic
keystrokes?** Nearly every other difference follows from that answer.

This guide names both modes, then takes each moving part in turn — what it is, what it
is used for, and how it works.

| | Optimal mode | Fallback mode |
|---|---|---|
| Text enters via | IBus preedit | `ydotool` keystrokes, or the clipboard |
| Words appear | in the field, revised in place | typed after the fact |
| Can race your typing | no | yes |
| Escape stops a session | single press | double tap |
| Enter mid-session | ordered after the commit | may land before the words |
| Emoji | typed with everything else | clipboard, or dropped with a notification |
| Needs root or a udev rule | no | **yes** |
| Reaches Chrome | yes | partly |

## The two configurations

Optimal:

```toml
[ime]
enabled = true

[streaming]
enabled = true

[recognition]
model = "small.en"
gpu_device = 0

[activation]
mode = "double_tap"
toggle_key = "KEY_RIGHTCTRL"

[editing]
command_mode = true
read_focused_field = true
```

Fallback — what you get with `[ime] enabled = false`, or with it on and an engine that
never registered:

```toml
[injection]
method = "auto"           # ydotool where it works, clipboard where it does not
```

---

## The input method

**What it is.** An IBus engine govox registers under the name in `[ime] engine_name`
(default `govox`), which the desktop activates while a session runs and swaps back to
`baseline_engine` (default `xkb:us::eng`) afterwards.

**What it is used for.** Putting dictation into the focused field as *provisional* text —
the mechanism macOS Dictation uses — rather than typing it and correcting afterwards.

**How it works.** While a streaming session runs, the running transcript appears
underlined in the field itself and is revised in place as Whisper revises it. Nothing
enters the document until you stop, at which point the whole session commits as a single
insertion. Four consequences follow, and they are the reason this mode is the optimal
one:

- **It cannot race your typing.** There is nothing in the document to race with until
  the commit.
- **It reaches applications that expose no writable accessible text**, Chrome among them.
- **govox can consume a keystroke.** An input method is the only position from which it
  can. This is what makes a single Escape end a session, and what lets Enter, Tab, the
  arrows and Home/End be held back until the commit lands, so they act on text that
  exists rather than on text still pending. Outside an input method those keys reach the
  application first, and Enter puts its newline in front of the words it followed.
  Modified presses are deliberately never consumed — re-issuing would mean rebuilding
  the chord, and dropping a modifier is worse than the reordering it would fix.
- **The field can say what it is.** Clients report a content type only once govox's
  engine is active for them, which is what lets prose rules stand down in a URL bar, and
  what makes the password-field refusal possible at all.

Teardown is queued behind the commit rather than run on the key release: clearing the
preedit first would leave the commit with no live input method, falling back to typing
the text a keystroke at a time — the one thing preedit exists to avoid.

## Injection

**What it is.** The path text takes when there is no input method: `[injection] method`,
one of `ydotool`, `clipboard` or `auto`.

**What it is used for.** Getting text into a window govox cannot reach as an input
method.

**How it works.** `auto` probes capabilities once at startup and selects a backend. If
`ydotool` then rejects a call, a wrapper carries on over the clipboard — built with
pasting **disabled**, because pasting needs `ydotool` and the only reason the fallback is
running is that `ydotool` just failed. Which backend was *selected* and which one last
did the work are different questions, and the tray reports the second.

Three costs are worth knowing before choosing this path:

- **A privileged daemon.** `ydotool` needs `ydotoold` as root, or a udev rule granting
  access to `/dev/uinput`. This is a security decision, not a convenience one, and it is
  the largest operational difference between the two modes.
- **Keystroke ordering.** govox observes evdev without grabbing it, so a key it acts on
  *also* reaches the application. That is why the stop key takes a double tap here: a
  single Escape would end dictation and still reach the app.
- **Emoji.** `ydotool` types by emulating keycodes and no keycode produces an emoji, so
  `ydotool type 👍` exits 0 and types nothing. Pictographic text is therefore *routed* to
  the clipboard before anything is attempted — a router, not a fallback, because the
  failure it avoids is not one `ydotool` reports. Where no clipboard exists, govox drops
  what it cannot type, renormalises the spacing the removal leaves behind (`Thanks 🙂.`
  becomes `Thanks.`, not `Thanks .`), types the rest, and notifies every time.

## The microphone

**What it is.** `[audio] device`, empty by default.

**What it is used for.** Choosing an input when the system default is not the one you
want. govox does not guess between inputs.

**How it works.** Everything downstream — the voice-activity gate, the segmenter, the
decode — works from whatever this device heard, so the microphone sets a ceiling nothing
later can raise. A headset or directional microphone is worth more than any model change.

## The model and the GPU

**What they are.** `[recognition] model` and `[recognition] gpu_device`.

**What they are used for.** Trading decode cost against accuracy, and selecting an
accelerator.

**How they work.** Models are GGUF builds of Whisper, fetched on first use and cached.
Decode cost varies by roughly 20× across the range, and on the streaming path decode cost
*is* preview cadence — a slower model does not merely finish later, it makes words appear
in larger, later jumps.

whisper.cpp selects its accelerator at **compile time**, so `gpu_device` means different
things in different builds: the Vulkan build enumerates devices in a different order than
CUDA does. The number you set is not portable between builds. See
[models.md](models.md) for the full ordering problem and the measured cost table.

## Field reading

**What it is.** `[editing] read_focused_field`, off by default. Needs `python3-gi` and
`gir1.2-atspi-2.0`.

**What it is used for.** Letting "delete that" and its relatives verify their target
before acting.

**How it works.** govox reads the focused widget over AT-SPI and confirms that the text
before the caret is still what it typed, refusing if it is not — closing the window that
`last_insertion_ttl_s` can only bound. Where the field cannot be read the commands behave
exactly as they do with this off, which is most of the time: GTK applications expose
their text, terminals do not, Chromium needs `--force-renderer-accessibility`, and
Electron applications expose nothing at all. It is off by default because it can only
ever turn a command that would have run into one that refuses — which is also why it is
safe to turn on.

## Command mode

**What it is.** `[editing] command_mode`, off by default.

**What it is used for.** Stopping a half-heard command from scattering words through a
document mid-edit.

**How it works.** While command mode is on, an utterance that matches no command is
announced and discarded instead of typed. The cost is the reason it is opt-in: it is also
a way to lose a sentence you meant to dictate. With it false the mode phrases do nothing
at all — "command mode" dictates as ordinary text, so no dormant state can surprise
someone who never turned it on.

## Telling which mode you are actually in

Configured and in effect are different facts, and the gap between them is this daemon's
characteristic failure: a GPU build running on the integrated card, `[ime] enabled = true`
with an engine that never registered, `read_focused_field = true` with an AT-SPI
connection that quietly failed.

Two places answer honestly:

- **The tray's About menu** reports model, backend and GPU index, injector, preedit and
  field reading, each distinguishing configured from in effect.
- **`govox doctor`** reports the same ground truth as checks with remedies. It reserves
  FAIL for "cannot dictate at all", so a non-listening `ydotoold` is a WARN — the
  clipboard fallback still gets text into the window, and reporting a disabled optional
  subsystem as a failure trains people to ignore the output.

## Related

- [models.md](models.md) — model sizes, the decode-cost spread, and `gpu_device` ordering
  between the Vulkan and CUDA builds.
- [accuracy-eval.md](accuracy-eval.md) — measuring whether a change helped.
- [../parity.md](../parity.md) — why each behaviour here is the way it is, including every
  silent-success trap named above.
