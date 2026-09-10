---
last_verified: 2026-09-10
owner: rockyburt
type: Guide
---

# Editing by voice

`govox commands` prints every phrase and which are switched on. This guide is
the part that list cannot show: how the pieces fit, and why a command sometimes
refuses.

## The one distinction that explains everything

There are two kinds of editing command, and confusing them is the main way this
gets frustrating.

| | **Structural** | **Phrase-targeted** |
|---|---|---|
| Names its target by | position — "previous two words" | content — "the old file" |
| Available | always | **only in command mode** |
| Needs to read the field | only for sentence/paragraph | **always** |
| Example | `delete previous three words` | `delete the old file` |

The second kind is what you want for changing a sentence you already wrote, and
it is the kind that is switched off until you say **"command mode"**. If you
have tried "delete the old file" during ordinary dictation and watched govox
type it as text, that is the reason — a free-form phrase is a sentence, not an
instruction, so it only counts as a command once you have said it is.

## Getting into command mode

```
command mode          ┐
start command mode    ├─ any of these
start commands        │
let's command         ┘

dictation mode        ┐
dictate               ├─ any of these to leave
stop commands         ┘
```

While in command mode nothing is typed. The tray icon changes, and the mode
outlives the utterance — it stays until you leave it.

## Structural editing: position

Always available, no mode needed. The grammar composes:

```
<verb> <direction> [count] <unit>

verb        delete, kill, select, extend selection, move
direction   previous, last, back, backward(s), next, forward(s)
count       a number, optional — defaults to one
unit        character(s), letter(s), word(s), sentence(s),
            paragraph(s), line(s), document
```

So all of these are valid, and none of them needs to be memorised as a phrase:

```
kill last word
delete previous three words
select next two sentences
extend selection back four characters
move forward one paragraph
move to beginning of the line
move to end of document
```

**Word, character, line and document** compile to ordinary keystrokes
(`ctrl+shift+left` and friends), so they work anywhere a keyboard works.

**Sentence and paragraph do not.** No toolkit binds them — GTK4 maps
`ctrl+up`/`ctrl+down` to paragraph motion but Chromium, Electron and Qt disagree
— so govox measures the real text instead and walks the caret one character at a
time. That means sentence and paragraph motion **need field reading**
(`[editing] read_focused_field = true` and an application that exposes its text
over AT-SPI). Where it cannot read, it says so rather than guessing: a caret
that lands somewhere unexpected and then receives a delete is the worst failure
available here.

## Phrase editing: content

Command mode only. Five commands:

```
select <phrase>
delete <phrase>
replace <phrase> with <phrase>
move before <phrase>
move after <phrase>
```

Matching is **case-insensitive** and forgiving about spacing, so "the old file"
finds "The  Old   File". The search order is worth knowing, because it is what
makes these predictable in a long document:

1. the nearest match **strictly behind the caret**, searching backwards;
2. failing that, the first match **at or after** the caret.

In other words it prefers what you just wrote. If the phrase appears twice,
`move after` it once and the next command finds the other.

All five need to read the field. In an application that does not expose its
text, you get *"cannot find X — this application does not expose its text"*
rather than a silent no-op.

## Worked examples

Take this in the field, caret at the end:

```
We drove out to Twillingate on Saturday afternoon.
```

**Change one word.** Say `command mode`, then:

```
replace Saturday with Sunday
→ We drove out to Twillingate on Sunday afternoon.
```

**Cut a clause.**

```
delete on Sunday afternoon
→ We drove out to Twillingate .
```

Exactly the characters you named, and not one more — which is why there is now a
space before the full stop. Phrase deletion does not tidy up around itself,
because guessing at surrounding whitespace is how an edit surprises you. Follow
it with `delete previous character` if the spacing matters.

**Insert in the middle.** Put the caret where you want it, leave command mode,
and dictate:

```
move after Twillingate      (command mode)
dictation mode
comma which was packed      → We drove out to Twillingate, which was packed on Sunday afternoon.
```

**Fix the tail without naming it.** No mode change needed, since this is
structural:

```
delete previous two words
→ We drove out to Twillingate on
```

**Select, then overtype.** Selecting leaves the selection live, and in most
applications typing over a selection replaces it:

```
select Twillingate          (command mode)
dictation mode
Bonavista                   → We drove out to Bonavista on Sunday afternoon.
```

**Undo any of it.**

```
undo that
```

## The "that" commands are a third thing

`delete that`, `scratch that`, `uppercase that`, `capitalize that` do not act on
the caret or on a phrase. They act on **the last thing govox typed**, which has
two consequences:

- They cannot touch text you typed by hand. Case transforms in particular
  *retype* the text rather than pressing a key, because no toolkit binds a case
  change — so they only work on govox's own output.
- They expire. `[editing] last_insertion_ttl_s` is 60 seconds here; after that
  govox has no record and says so.

They also verify before acting when field reading is on: if the text before the
caret is no longer what govox typed, `delete that` refuses instead of eating
whatever is there now. That check is the reason `read_focused_field` is worth
having on.

## When something refuses

Every refusal names its cause. The three you are most likely to meet:

| Message | Means |
|---|---|
| *"this application does not expose its text"* | No AT-SPI. Phrase commands and sentence/paragraph motion are unavailable here; word and character motion still work. |
| *"X is not in the field"* | The phrase was read correctly but is not present — check what actually landed, not what you meant. |
| *"nothing dictated to delete"* | The last insertion expired or was never govox's. |

To see what govox heard versus what it did:

```bash
journalctl --user -u govox-rs-dev -f
```

## Related

- [optimal-setup.md](optimal-setup.md) — `command_mode` and `read_focused_field`,
  the two opt-ins these depend on.
- [dictionary.md](dictionary.md) — making govox recognise the words in the first
  place.
