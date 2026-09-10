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
start command mode    ├─ any of these to enter
start commands        │
let's command         ┘

text mode             ┐
type mode             │
dictation mode        ├─ any of these to leave
dictate               │
stop commands         │
exit command mode     ┘
```

The examples below use **`text mode`** to leave. It is two short words rather
than a four-syllable one, which is one fewer thing for recognition to get wrong
in the phrase whose whole job is getting you out of a mode. Any of the others
does the same thing, so use whichever you say most naturally.

While in command mode nothing is typed. The tray icon changes, and the mode
outlives the utterance — it stays until you leave it.

Worth knowing what these phrases really do: dictation is not a mode you enter,
it is the state left when no mode is set. Command mode, spelling and sleep are
flags; plain dictation is all of them off. So every phrase in the second group
is *leaving*, not *arriving*, however it is worded.

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

Every example below starts in **dictation** — ordinary typing, no mode set. Each
line is one utterance: say it, then stop speaking. The `[…]` column is the state
you are in *before* saying that line, so you can see exactly when a switch is
needed and when it is not.

`[dictation]` is the state; `text mode` is the phrase that returns you to it.
They differ because the state is named for what govox does and the phrase is
chosen for being easy to say.

Remember the mode is sustained — it stays until you change it. If you are
already in command mode, skip the first line; if you are staying in command mode
for another edit, skip the last.

Take this in the field, caret at the end:

```
We drove out to Twillingate on Saturday afternoon.
```

### Change one word

```
[dictation] command mode
[commands]  replace Saturday with Sunday
[commands]  text mode
[dictation]
→ We drove out to Twillingate on Sunday afternoon.
```

That last line matters more than it looks: leaving command mode is what makes
your *next* sentence get typed instead of being hunted for as a command. Forget
it and the next thing you say is **discarded** rather than typed — in command
mode an utterance matching no command is treated as a misrecognition, on the
grounds that acting on a half-heard instruction is worse than ignoring it.

It does say so. You get *"Not a command, discarded: …"* with the text it heard,
so a sentence that vanishes is telling you which mode you are in.

### Cut a clause

```
[dictation] command mode
[commands]  delete on Sunday afternoon
[commands]  text mode
[dictation]
→ We drove out to Twillingate .
```

Exactly the characters you named, and not one more — which is why there is now a
space before the full stop. Phrase deletion does not tidy up around itself,
because guessing at surrounding whitespace is how an edit surprises you. To fix
the spacing you do **not** need command mode, because that is structural:

```
[dictation] delete previous character
→ We drove out to Twillingate.
```

### Insert in the middle

The caret has to be moved by content, so command mode is needed for that step
only — and you must leave it again before the words you want typed:

```
[dictation] command mode
[commands]  move after Twillingate
[commands]  text mode
[dictation] comma which was packed
→ We drove out to Twillingate, which was packed on Sunday afternoon.
```

### Fix the tail without naming it

No mode change at all. "previous two words" names a position, and structural
commands are always on:

```
[dictation] delete previous two words
→ We drove out to Twillingate on
```

This is worth preferring when it fits. Two fewer utterances, and no mode to
leave behind.

### Select, then overtype

Selecting leaves the selection live, and in most applications typing over a
selection replaces it — so the dictated word has to arrive *after* you are back
in dictation:

```
[dictation] command mode
[commands]  select Twillingate
[commands]  text mode
[dictation] Bonavista
→ We drove out to Bonavista on Sunday afternoon.
```

### Several edits in a row

Enter once, leave once. Everything between is a command, and nothing between is
typed:

```
[dictation] command mode
[commands]  replace Saturday with Sunday
[commands]  delete previous two words
[commands]  move to end of line
[commands]  text mode
[dictation]
```

Note `delete previous two words` works here too. Structural commands are
available in *both* modes — command mode adds the phrase commands rather than
replacing anything.

### Undo any of it

Also always on, so no mode change:

```
[dictation] undo that
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
