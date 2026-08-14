---
last_verified: 2026-08-14
owner: rockyburt
type: Reference
---

# Where govox has actually run

This is the honest coverage record: the hardware and desktop stack govox has been
exercised on, and — more usefully — everything it has *not*. Read it before assuming a
behaviour is general. Nothing here is a support matrix; it is a list of one machine and a
large set of untested unknowns.

The distinction that matters throughout: **exercised** means run in real daily dictation,
not that a test asserts it.

## The reference machine

Every measurement in this repository — `corpus/baseline.json`, the decode timings in the
guides, the GPU comparison behind `gpu_device` — was taken here.

| | |
|---|---|
| Model | Lenovo ThinkPad P1 Gen 7 (`21KVCTO1WW`) |
| CPU | Intel Core Ultra 9 185H — 16 cores, 22 threads |
| Memory | 60 GiB |
| Discrete GPU | NVIDIA GeForce RTX 4070 Laptop, 8188 MiB, driver 595.84 |
| Integrated GPU | Intel Arc Graphics (Meteor Lake-P), Mesa 26.0.3 |
| OS | Ubuntu 26.04 LTS, kernel 7.1.3 |
| Desktop | GNOME Shell 50.1 on Wayland |
| Toolchain | rustc 1.95.0 |

### Both GPUs are present, and that is the point

Vulkan enumerates three devices on this machine, in this order:

| Index | Device |
|---|---|
| 0 | Intel Arc Graphics (MTL), Vulkan 1.4.335 |
| 1 | NVIDIA GeForce RTX 4070 Laptop, Vulkan 1.4.329 |
| 2 | `llvmpipe` — software rasteriser |

This ordering is why `[recognition] gpu_device = 1` is correct here and why leaving it
unset is a performance trap rather than a neutral default: index 0 is the iGPU, so an
unconfigured install runs Whisper on integrated graphics and feels inexplicably slow.

**A CUDA build would number these differently.** CUDA enumerates only NVIDIA devices, so
the 4070 becomes index 0 and `gpu_device = 1` would be wrong. The value is not portable
across backends — it is a property of the build, not of the machine.

The default build here is Vulkan, and deliberately so: `nvcc` is **absent** on this
machine, so the CUDA variant has never been compiled or run. See
[the model guide](../guides/models.md) for what the backend choice costs.

## Desktop stack exercised

| Component | Version | Notes |
|---|---|---|
| IBus | 1.5.34-rc2 | Preedit path. All GVariant layouts were recovered against this daemon. |
| PipeWire | 1.6.2 | Capture via its ALSA compatibility layer. |
| Xwayland | 24.1.10 | Required by the overlay; no native Wayland renderer exists. |
| `ydotool` | unversioned build | Injection fallback. Has no `--version`. |
| `wl-clipboard` | `/usr/bin/wl-copy` | Clipboard fallback. |
| GNOME extensions | `ubuntu-appindicators` | **Provides the `StatusNotifierWatcher` the tray needs.** GNOME has no built-in tray, so without an extension of this kind the icon silently never appears. |

Audio input is a USB Blue microphone at 32 kHz, resampled to the 16 kHz Whisper expects. A
Logitech C922 webcam microphone is also present and selectable.

Three monitors, in a mixed portrait/landscape layout (two 3840×2160 landscape, one
2160×3840 portrait). This is worth recording because overlay placement and caret-following
are exactly the kind of code that works by accident on a single screen.

## What has genuinely been exercised

Run in real daily use on the machine above:

- Capture, VAD segmentation, recognition, correction, editing commands.
- Streaming with provisional text, at `min_chunk_size_s = 0.25`.
- IBus preedit into GTK and Electron applications; `ydotool` fallback where preedit is
  unavailable.
- AT-SPI focus tracking and field reading.
- The overlay, including caret following and per-application offset rules.
- Tray icon, notifications and chimes.
- `large-v3-turbo`, `medium.en`, `small.en` and `small`, all on the 4070 via Vulkan.

## What has never been run

Stated plainly, because the README's "only tested on one desktop" deserves specifics:

- **Any other desktop environment.** KDE, Sway, Hyprland, XFCE — untested. KDE is the
  interesting case: it ships `StatusNotifierWatcher` natively, so the tray should work
  *better* there, while the overlay's X11 assumptions are the likelier problem.
- **X11 sessions.** Wayland only. The overlay is X11 code, but it has only ever run as an
  XWayland client, never on a real X server.
- **Any other distribution**, and no older Ubuntu. Both IBus and GNOME are recent versions
  here; the IBus D-Bus interface is stable, but this has not been demonstrated.
- **CUDA and CPU-only builds.** Neither has been compiled on this machine. The CPU path is
  expected to work and to be far slower; that expectation is untested.
- **AMD graphics**, and NVIDIA on the proprietary driver in any other configuration.
- **Non-English dictation.** Every model in use is `.en` or run with `language = "en"`.
- **A machine with a single GPU.** `gpu_device` has only ever been exercised where the
  choice was consequential.
- **Word error rate**, on any hardware — see `corpus/baseline.json`, still
  `NOT YET MEASURED`.

## Adding a machine

When govox is run somewhere new, add a row and say what broke. A second machine is worth
more to this file than any amount of reasoning about what *should* work — particularly a
non-GNOME desktop, an X11 session, or a single-GPU system, since each of those exercises a
path that has never once executed.

Collect the equivalent facts with:

```console
. /etc/os-release && echo "$PRETTY_NAME / $(uname -r)"
echo "$XDG_CURRENT_DESKTOP / $XDG_SESSION_TYPE"
vulkaninfo --summary | grep -E "deviceName|driverInfo"
govox doctor
```

`govox doctor` is the important one: it reports every subsystem as OK, WARN or FAIL and
names what would fix each, so its output is the most compact description of a new
environment that exists.
