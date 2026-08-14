---
last_verified: 2026-08-13
owner: rockyburt
type: Spike Result
covers:
  - spikes/ibus-probe/
---

# M-1(b) — IBus from Rust over raw D-Bus

**Date:** 2026-08-13 · **Verdict: PASS.** M10's two unknowns — GVariant serialization and transport
without libibus — are both retired, and the daemon was observed calling back into a
Rust-served factory.

```text
  >>> ibus-daemon called CreateEngine("govox-rs-spike")
```

## Question

`govox-py` reaches IBus through libibus via PyGObject, on a dedicated GLib main loop. The
pure-D-Bus decision removes that loop — but IBus's serializable objects use a bespoke
GVariant layout that libibus normally builds for you and that is **documented nowhere**.
Reproducing it by hand was the single biggest unknown in M10, the riskiest milestone.

## What was established

### 1. The GVariant layouts, recovered exactly

Not guessed — read off the live daemon:

```console
$ gdbus call --address "$IBUS_ADDRESS" --dest org.freedesktop.IBus \
    --object-path /org/freedesktop/IBus \
    --method org.freedesktop.IBus.GetEnginesByNames '["xkb:us::eng"]'
([<('IBusEngineDesc', @a{sv} {}, 'xkb:us::eng', 'English (US)', 'English (US)',
   'en', 'GPL', 'Peng Huang <…>', 'ibus-keyboard', 'us', uint32 50,
   '', '', '', '', '', '', '', '')>],)
```

| Type | Signature | Fields after the `s` tag and `a{sv}` attachments |
|---|---|---|
| `IBusEngineDesc` | `(sa{sv}ssssssssussssssss)` | name, longname, description, language, license, author, icon, layout, **u** rank, hotkeys, symbol, setup, layout_variant, layout_option, version, textdomain, icon_prop_key |
| `IBusComponent` | `(sa{sv}ssssssssavav)` | name, description, version, license, author, homepage, exec, textdomain, **av** observed_paths, **av** engines |

`gdbus introspect`/`call` against the private bus is the technique to reach for whenever
another IBus type is needed. It beats reading libibus's C source.

### 2. `RegisterComponent` accepts a hand-built variant

`zvariant::StructureBuilder` produced `(sa{sv}ssssssssavav)` and the daemon took it without
complaint. **This retires the main risk**: no libibus, no GObject introspection, no GLib
main loop, no `gir`-generated bindings needed. The fallback plans in the risk register
(generate `ibus-sys`, write a C shim, keep a Python sidecar) can be dropped.

### 3. Bus discovery is a real task libibus was hiding

IBus does not use the session bus. `IBus.Bus()` reads
`$XDG_CONFIG_HOME/ibus/bus/<machine-id>-<display>` — and **stale files for dead daemons
are left behind**. This machine has three, of which exactly one is live:

```text
c38d861e94be4cf1ada335aaf83b51a0-unix-wayland-0  pid=9928  ALIVE
c38d861e94be4cf1ada335aaf83b51a0-unix-1          pid=3887  stale
c38d861e94be4cf1ada335aaf83b51a0-unix-0          pid=5256  stale
```

Picking the first file found would connect to a dead socket. The Rust implementation must
parse `IBUS_DAEMON_PID` and check `/proc/<pid>` for liveness, honouring `$IBUS_ADDRESS`
first when it is set.

### 4. A fourth "silent success" trap

**`RegisterComponent` returning OK proves nothing, and `GetEnginesByNames` cannot confirm
it.** That method reads the *static* XML registry built from
`/usr/share/ibus/component/*.xml`, not dynamic registrations — it returns 0 engines for a
registration that succeeded. Confirmed independently: querying the *running govox-py*'s own
`govox` engine also returns 0.

This belongs with `ydotool key <name>`, the synchronous engine switch, and
`PreeditFocusMode.COMMIT` in the parity ledger and in `crates/*/tests/negative_*.rs`. The
only real evidence a component was accepted is ibus-daemon calling back into the factory.

### 5. GNOME forbids per-context engine selection

Attempting to activate the engine on an input context we created ourselves — the safe
route, since it touches nothing the user is typing into — is refused outright:

```text
org.freedesktop.DBus.Error.Failed: Cannot set engines when use-global-engine is enabled.
```

So on this desktop the daemon-wide `SetGlobalEngine` is the **only** way to instantiate an
engine. That is not a design choice govox-py made; it is the only door. It also explains
why the 15-second deadlock in the synchronous variant mattered so much — there is no
alternative path to fall back to.

### 6. The factory callback was observed — the path works end to end

Run with the user's explicit permission, using the global switch (the only door, per 5),
with the previous engine restored immediately afterwards:

```text
falling back to the global switch (previous engine: <none>)
  >>> ibus-daemon called CreateEngine("govox-rs-spike")
SetGlobalEngine returned OK
restored global engine to "xkb:us::eng"
```

ibus-daemon resolved the dynamically-registered component, looked up our bus name, found
the exported `org.freedesktop.IBus.Factory`, and called `CreateEngine`. **No libibus, no
GObject introspection, no GLib main loop anywhere in the process.**

Finding (4) was confirmed a second time on the way out: after the run, `govox` is the
**active global engine** and yet `GetEnginesByNames(["govox"])` still returns `@av []`. An
engine can be live and in use while that method reports it does not exist.

## What was still outstanding — resolved in M10

The **preedit lifecycle** was not exercised here. It was picked up in M10, and the
prediction that the remaining layouts could be recovered the same way held, with one
correction to the technique: `gdbus call` cannot reach `IBusText`, because no IBus method
returns one. Asking libibus to serialize an object is the way in, and it is strictly
better — it answers for any serializable type, not only those a method happens to return:

```console
$ python3 -c 'import gi; gi.require_version("IBus","1.0")
from gi.repository import IBus
print(IBus.Text.new_from_string("hi").serialize_object().print_(True))'
('IBusText', @a{sv} {}, 'hi', <('IBusAttrList', @a{sv} {}, @av [])>)
```

Two things this spike could not have found, both because PyGObject hides them:

* **There is no `UpdatePreeditTextWithMode` on the wire.** The `UpdatePreeditText` signal
  is `(v, u, b, u)` and the focus mode is a required fourth argument. libibus's two calls
  are a local convenience over one signal — which makes the CLEAR guarantee stronger in
  Rust than in Python: the mode cannot be omitted, only chosen.
* **`ContentType` is a write-only property `(uu)`, not a `SetContentType` method.**

Residual risk for M10: none remaining. It shipped.

## Notes for M10

- Claim the bus name and export the factory **before** `RegisterComponent`, so the daemon
  can resolve the component the moment the registration lands. `govox-py` documents the
  same ordering constraint.
- `exec` in the component is empty on purpose: the engine lives inside the daemon process,
  so there is nothing for ibus-daemon to spawn. `packaging/ibus/govox.xml` says the same.
- Set engine `rank = 0` so the engine is never auto-selected as a default input source.
- Registration is tied to the connection; dropping it withdraws the component. That is a
  better cleanup story than `govox-py`'s, which needs an explicit teardown with a
  `GLib.timeout_add(500, quit)` grace period and a 2-second thread join.
- Use `govox-rs` as the engine name until cutover, so both daemons can coexist.
