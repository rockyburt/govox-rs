---
last_verified: 2026-09-09
owner: rockyburt
type: Guide
---

# The personal dictionary

Two mechanisms live in `~/.config/govox/dictionary.toml` and they are not
interchangeable. Knowing which one you want is most of the work.

| | `bias` | `replace` |
|---|---|---|
| When | **Before** Whisper decodes | **After**, last in the correction pipeline |
| How | An `initial_prompt` nudging the decoder toward a spelling | Literal find/replace, in the order written |
| Strength | A hint. It cannot force anything | Deterministic. It always fires |
| Costs | Budget, capped at `bias_prompt_token_budget` | Nothing |

Reach for `bias` first. It is what the accuracy eval shows working — term recall
20/27 with bias against 10/27 without — and it cannot put a word in your document
that you did not say. A `replace` rule can, which is why the bar for adding one is
higher: no collision with a legitimate meaning, observed failing more than once,
and a note saying why.

## Getting the budget spent well

`[recognition] bias_prompt_token_budget` is 180 whitespace-separated words. Past
it, terms are dropped **in list order** — so position is priority, and a term
containing a space costs one unit per word.

Three things fill that budget, in this order:

1. **`bias`** — always on, whatever has focus. Never dropped to make room for
   anything else.
2. **Discovered terms** — enumerated from the machine, below.
3. **The matching `[[dictionary.bias_group]]`** — appended per focused window,
   with room reserved so it cannot evict the two above.

```toml
[dictionary]
bias = ["Rentals.ca", "ydotool"]

[[dictionary.bias_group]]
while_using = "*RentalsCa*"          # same patterns as feedback.app_rules
terms = ["Rentsync", "Jobber"]

[[dictionary.replace]]
from = "rentals api"
to = "Rentals-API"
```

First match wins; groups are not merged. A window that matches nothing, or one
govox cannot name, gets the core alone.

## Discovery

The words that change weekly should not have to be typed out. Add the table and
govox enumerates them at startup, at every reload, and whenever a repository is
cloned or a branch checked out:

```toml
[dictionary.discover]
repo_roots = ["~/dev/*/repos"]
```

That is the whole of a working configuration. Everything else has a default:

| Key | Default | What it does |
|---|---|---|
| `repo_roots` | `[]` | Directories whose children are checkouts. `~` and `*` are expanded |
| `dir_roots` | `[]` | Directories whose children are named regardless of whether they are checkouts |
| `term_files` | `[]` | Files listing terms. Same globbing as the roots |
| `providers` | all eight | Which sources to run |
| `max_repos` | `64` | Newest first, by when `HEAD` last moved |
| `max_dirs` | `32` | Same ordering. Lower, because nothing vouches for a plain directory |
| `max_branches_per_repo` | `8` | Most recently touched first |
| `max_terms` | `0` | A hard cap on discovered terms; `0` means "whatever the budget allows" |

The eight providers, in the priority order the budget spends them:

- **`files`** — terms from `term_files`, one per line or whitespace-separated,
  with `#` starting a comment. First, because it is the only source you wrote
  out on purpose: the hand-written `bias` list's standing, kept somewhere else.
  Use it for vocabulary nothing can infer — clients, people, jargon.
- **`repos`** — the directory name, plus the repository and org names from
  `origin`.
- **`branches`** — the *words* inside each branch. `feature/rentals-dashboard`
  contributes "rentals" and "dashboard": nobody dictates the slug, so biasing it
  would spend budget on a token Whisper will never emit. Scaffolding (`feature`,
  `fix`, `main`) and ticket ids are dropped.
- **`dirs`** — plain directory names under `dir_roots`, for project folders that
  were never checkouts. `repos` requires a `.git` and reads config, refs and
  `HEAD` out of it, so it skips these entirely. Dotfile directories are ignored,
  and the cap is lower than `max_repos` because a `.git` is evidence somebody
  works there and a bare directory is not.
- **`hostname`** — this machine, and the first label if it is an FQDN.
- **`ssh_hosts`** — every alias on a `Host` line in `~/.ssh/config`. Wildcards
  are skipped, and `Include` is not followed.
- **`systemd_units`** — your own unit filenames, extension and template `@`
  stripped.
- **`audio_devices`** — the configured `[audio] device` and what the backend
  reports, split into words with the backend's own furniture (`USB`, `Audio`,
  `hw`) dropped.

Anything written by hand outranks anything found, and a discovered term that
duplicates a hand-written one keeps your spelling and costs nothing.

### Discovered replacements

Discovery also generates a few `replace` rules, because some spellings are out
of bias's reach. Bias decides *which words* are decoded; it cannot make Whisper
join two of them. A checkout called `RentalsCa` therefore comes back as
"Rentals CA" no matter how heavily it is biased, and only a rule applied after
recognition produces the exact string:

```
rentals ca  → RentalsCa
rentals-ca  → RentalsCa
```

Which spelling is correct cannot be guessed from the sound — "rentals API" is
`Rentals-API` while "rentals CA" is `RentalsCa` — so the directory listing is
the only source, and a hand-written list would go stale on the first rename.

**Only joined names get a rule.** `Rentals-API` and `govox-rs` already carry
their separator, recognition can produce them unaided, and generating rules for
them is where the harm is: `Rentals-DO` would rewrite the ordinary phrase
"rentals do". A joined name whose parts are all function words — `DoIt` → "do
it" — is refused for the same reason.

Hand-written rules always win: a generated rule for a phrase you have already
written a rule about is discarded rather than left to lose a race. What was
generated is listed in the tray under **About → Replacements discovered**.

`.git` is read directly — `config`, `HEAD`, `packed-refs`, `refs/heads` — so no
`git` binary is needed and nothing forks on the reload path.

## Seeing what happened

Discovery reports itself. Per-provider counts, the size of the planned list, and
anything that did not fit:

```bash
journalctl --user -u govox-rs-dev | grep -i discover
```

A `WARN` naming dropped terms means the budget is full. Either raise
`[recognition] bias_prompt_token_budget` or narrow the input with `max_repos` /
`max_branches_per_repo`. Silent truncation is exactly what this warning exists to
prevent, so do not ignore it: the dropped words are ones you asked for.

## When it will not start

A malformed `[dictionary.discover]` is **fatal**, like the rest of the file. A
misspelled provider name refuses to start and lists the valid ones, because
enumerating three quarters of what you asked for and saying nothing is worse than
stopping.

A machine that will not answer is **never** fatal. A repository root that does not
exist, no `~/.ssh/config`, a checkout mid-rebase — each is an empty answer and a
`debug!` line. None of them are instructions, and none change what gets typed
except by omission.

## Related

- [accuracy-eval.md](accuracy-eval.md) — measuring whether any of this earns its
  place. The eval deliberately loads the hand-authored dictionary only: a score
  that moves when you check out a branch is not a measurement.
- `docs/parity.md` — why each of the above behaves as it does.
