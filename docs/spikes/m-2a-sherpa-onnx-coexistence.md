---
last_verified: 2026-08-16
owner: rockyburt
type: Spike
covers:
  - spikes/parakeet-probe/
  - crates/govox-vad/
---

# M-2(a): can sherpa-onnx and the Silero VAD share one binary?

**Answer: only with shared linking, and that costs the self-contained binary.**

## Why this was asked

The ASR layer review proposed replacing Whisper with NVIDIA Parakeet TDT via sherpa-onnx,
and argued the integration was cheap because *"`ort` is already in the tree for Silero VAD,
so the heavy native dependency is paid for."*

That premise is **false**, and it is the sort of false that only shows up when you try it:

| | reaches ONNX Runtime via | version |
|---|---|---|
| `silero 0.6` | `ort 2.0.0-rc.13` | its own, ~1.24 |
| `sherpa-onnx 1.13.5` | `sherpa-onnx-sys` (C API) | its own, **1.28.0** |

sherpa-onnx does not use `ort` at all. Adopting it while keeping the current VAD means two
independent ONNX Runtimes in one process, not one shared dependency.

## What the probe did

`spikes/parakeet-probe` depends on **both** crates and, in one process, calls
`sherpa_onnx::onnxruntime_version()`, builds a `silero::Session::bundled()` exactly as
`govox-vad` does, and runs one 512-sample inference through it. Building both is not the
test — the linker drops what nothing calls, so both sides must actually execute.

## Result

**Static linking — sherpa-onnx's default — does not link at all.**

```text
mold: error: duplicate symbol: libsherpa_onnx_sys.rlib(onnx-ml.pb.cc.o):
             libort_sys.rlib(onnx-ml.pb.cc.o): onnx::FunctionProto::~FunctionProto()
```

Both `-sys` crates statically embed `onnx-ml.pb.cc.o`, the ONNX protobuf definitions.
Hundreds of duplicate `onnx::*` symbols; the link fails outright. This is a hard stop, not a
warning.

**Shared linking works.** With `default-features = false, features = ["shared"]`:

```text
sherpa-onnx      : linked, ONNX Runtime 1.28.0
silero via ort   : session created
silero inference : ran, speech probability [0.0016697943]
```

Both runtimes initialise, and Silero produces a correct answer — near-zero speech
probability on a window of silence. No runtime symbol clash.

## The cost, which is the actual finding

`docs/parity.md` records the current arrangement as a deliberate win:

> The `silero` crate compiles the model in and links ORT statically: **a self-contained
> binary, no `libonnxruntime.so`, no download.**

Shared linking gives that up. The probe binary declares `libsherpa-onnx-c-api.so`, and
`ldd` reports it as **not found**. It runs under `cargo run` only because cargo injects
`LD_LIBRARY_PATH`; invoked directly it fails:

```text
error while loading shared libraries: libsherpa-onnx-c-api.so:
cannot open shared object file: No such file or directory
```

That is precisely the class of failure this project refuses elsewhere — works for the
developer, breaks on the installed machine. Shipping it means installing 5.2 MB of shared
objects and getting the loader path right, plus a **193 MB** prebuilt archive fetched at
build time into `target/sherpa-onnx-prebuilt/`.

## What this means for a Parakeet backend

It is not blocked, but it is not free either, and the cheap version does not exist:

1. **Keep both, shared-linked.** Proven to work. Costs the self-contained binary, adds a
   packaging step, and carries two ONNX Runtimes (1.28.0 and ~1.24) in one process — twice
   the runtime, and two sets of global state.
2. **Drop `silero`, use sherpa-onnx's own Silero VAD.** One runtime, static linking back on
   the table, self-contained binary preserved. But `docs/parity.md` records the VAD as a
   verified parity surface — 44/47 windows bit-identical, 3 differing by 1e-6 — and
   whisper.cpp's built-in VAD was already **dropped** for exactly this reason: *"the VAD
   decides where utterances split, so swapping it silently re-tunes segmentation and the
   ported VAD tests stop being parity tests."* Since sherpa runs the same Silero model, the
   probabilities may well match; that is a measurable question and the obvious next spike.
3. **Do nothing.** The trait seam from the `WordRecognizer` work means this stays additive
   whenever it is picked up.

Option 2 is the only route that keeps what the current design bought. It should be decided
by measuring sherpa's VAD probabilities against the existing fixtures, not by argument.

## Reproducing

```bash
cd spikes/parakeet-probe
cargo run                                    # shared: works
# then, to see the static failure:
#   sherpa-onnx = "1.13"   (default features)
cargo build                                  # duplicate onnx::* symbols
```

Outside the workspace, like the whisper and silero probes, so linking ONNX Runtime never
enters the `govox-core` test loop.
