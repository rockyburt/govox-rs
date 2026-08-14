# Packaging Notes

govox-rs ships as a **single compiled binary** plus two user systemd units. That is the whole
of it, and it is most of the reason the rewrite exists: there is no interpreter, no virtual
environment, no extras list, and no `sys.path` bridging to reach the system PyGObject.

## What is *not* a dependency any more

Worth stating explicitly, because these are the runtime installs govox-py needs and this
package does not:

| govox-py needs | govox-rs |
|---|---|
| `python3`, a venv, `uv`, five `--extra` groups | nothing — one binary |
| `python3-gi`, `gir1.2-ibus-1.0` | IBus is reached over raw D-Bus |
| `libatspi`, GObject introspection | AT-SPI is reached over raw D-Bus |
| `onnxruntime`, `silero-vad`, `torch` | the Silero model and ORT are compiled in |
| GTK3 for the overlay | `x11rb` + a pure-Rust renderer |

What remains: `ydotool` for injection and `wl-clipboard` for the fallback. Both are already
in Ubuntu main.

## Build

```bash
cargo build --release --features vulkan     # the shipped variant
cargo build --release --features cuda       # the govox-cuda variant
```

The GPU backend is a **compile-time** choice, because whisper-rs selects it with cargo
features. That is a real divergence from govox-py, where `[recognition] device` is runtime
config, and it is why two package variants exist rather than one. `device = "cuda"` on a CPU
build is a hard startup error naming the fix, never a silent fall back to CPU — a daemon an
order of magnitude too slow that reports success is the failure mode this project spends most
of its comments on.

Sketch:

```bash
fpm -s dir -t deb \
  -n govox-rs \
  -v 0.1.0 \
  -a amd64 \
  -d ydotool -d wl-clipboard \
  --provides govox --conflicts govox-py --replaces govox-py \
  target/release/govox=/usr/bin/govox \
  target/release/govox-overlay=/usr/libexec/govox/govox-overlay \
  packaging/systemd/govox.service=/usr/lib/systemd/user/govox.service \
  packaging/systemd/ydotoold.service=/usr/lib/systemd/user/ydotoold.service
```

`Conflicts`/`Replaces` are reciprocal with govox-py, which reserved the `govox-rs` name in its
own `debian/control` before this repository existed. Exactly one of the two can be installed,
which is what stops two daemons grabbing the same evdev devices and registering the same IBus
engine. During the parity period both may still be *run* from source; the bus name
`org.freedesktop.IBus.Govox` is the runtime guard there, and the second process to start says
so by name rather than failing obscurely.

The tracked `systemd/govox.service` uses `ExecStart=%h/.local/bin/govox run` for the
from-source workflow. A distro package substitutes `/usr/bin/govox`.

## After installing

```bash
systemctl --user enable --now ydotoold.service
systemctl --user enable --now govox.service
govox doctor
```

`govox doctor` is the acceptance test: every section OK or SKIP, no FAIL. It exits non-zero
only on a FAIL, so a post-install script can branch on it — warnings describe a degraded but
working system and must not fail the install.

Two things it will tell you that nothing else will:

* **`input devices — no readable devices in /dev/input`** means the user is not in the `input`
  group. The remedy it prints includes logging out, which is the part people get stuck on:
  group changes do not apply to a running session.
* **`ydotool — ydotoold is not listening`** is a warning, not an error. Dictation still works
  through the clipboard fallback; it just needs a paste.

## Running it during development

`systemd/govox-rs-dev.service` runs the **debug** binary out of the shared target
directory, so the daemon under test is whichever worktree you last built from:

```bash
install -Dm644 packaging/systemd/govox-rs-dev.service \
    ~/.config/systemd/user/govox-rs-dev.service
systemctl --user daemon-reload
tools/dev-restart.sh            # build, restart, show the last log lines
tools/dev-restart.sh --follow   # ...and then tail the journal
```

Use the script rather than `systemctl --user restart` on its own: restarting without
building relaunches the *old* binary, which looks exactly like a change that did not work.

Debug rather than release is deliberate. `[profile.dev.package."*"] opt-level = 2` already
optimises every dependency, whisper.cpp and the Vulkan backend included, so the only
unoptimised code is govox's own glue — which is not where the time goes. A release build
would add LTO and `codegen-units = 1` to every edit-run cycle for no audible gain.

The unit declares `Conflicts=govox.service`, so starting it **stops govox-py** and vice
versa. That is the coexistence guard made automatic rather than remembered: the bus name
refuses a second IBus registration, but evdev does not, so two daemons would both see the
activation key and dictate twice.

It is deliberately **not** `enable`d. Neither govox unit auto-starts at login on the
reference machine, and enabling this one would quietly promote a development build to the
daily driver. To switch back to the Python implementation:

```bash
systemctl --user start govox        # Conflicts= stops govox-rs-dev for you
```

## Optional: a permanent input source

`packaging/ibus/govox.xml` installs govox as an entry under Settings ▸ Keyboard ▸ Input
Sources that survives the daemon not running. It is genuinely optional — govox registers the
same component at runtime — and is only worth it if you want the input source to exist when
govox does not. See the comments in the file.

Flatpak packaging is deferred until portal-based injection and hotkey paths exist.
