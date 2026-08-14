#!/usr/bin/env python3
"""Generate the M2 correction/editing parity corpus from the pinned govox-py.

Emits JSONL, one record per call: ``{"stage": ..., "args": {...}, "out": ...}``.
The Rust side replays each record and asserts byte equality.

Run against the *pinned* source, never the live checkout:

    ./tools/parity-gen/pinned-source.sh
    PYTHONPATH=tools/parity-gen/.parity-src/src \\
      <interpreter> tools/parity-gen/correction_corpus.py > corpus/correction.jsonl

# Why it sweeps the tables rather than listing cases

Inputs are built by importing govox's own lookup tables — punctuation phrases,
emoji, number words, the editing grammar — and generating every combination that
matters. Hand-written vectors only cover what someone remembered; a table-driven
sweep covers what is actually there, and grows by itself when the pinned commit
gains an entry. That is the difference between a corpus that catches a missed
table row and one that does not.
"""

from __future__ import annotations

import dataclasses
import itertools
import json
import sys
from typing import Any

from govox.config import CorrectionConfig
from govox.correction import commands as commands_mod
from govox.correction import emoji as emoji_mod
from govox.correction import grammar as grammar_mod
from govox.correction import numbers as numbers_mod
from govox.correction import pipeline as pipeline_mod
from govox.correction import punctuation as punct_mod
from govox.correction.dictionary import apply_replacements, bounded_pattern
from govox.domain import (
    CommandAction,
    EditAction,
    FieldSnapshot,
    KeyAction,
    ModeAction,
    PersonalDictionary,
    TextAction,
    Unit,
)
from govox.editing import editor as editor_mod
from govox.editing import spans as spans_mod

OUT = sys.stdout


def emit(stage: str, args: dict[str, Any], out: Any) -> None:
    json.dump(
        {"stage": stage, "args": args, "out": jsonable(out)},
        OUT,
        ensure_ascii=False,
        sort_keys=True,
    )
    OUT.write("\n")


def jsonable(value: Any) -> Any:
    """Render a domain object as plain JSON the Rust side can compare against."""
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, (list, tuple)):
        return [jsonable(v) for v in value]
    if isinstance(value, dict):
        return {k: jsonable(v) for k, v in value.items()}
    if isinstance(value, TextAction):
        return {"kind": "text", "text": value.text}
    # Explicit, not left to the dataclass fallback: without this KeyAction
    # serialises as a bare {"chords": [...]} with no discriminator, while its
    # siblings all carry one. An EditPlan mixes the two.
    if isinstance(value, KeyAction):
        return {"kind": "keys", "chords": list(value.chords)}
    if isinstance(value, CommandAction):
        return {"kind": "command", "name": value.name}
    if isinstance(value, ModeAction):
        return {"kind": "mode", "command_mode": value.command_mode}
    if isinstance(value, EditAction):
        return {
            "kind": "edit",
            "op": value.op.value,
            "unit": value.unit.value if value.unit else None,
            "direction": value.direction.value if value.direction else None,
            "count": value.count,
            "phrase": value.phrase,
            "replacement": value.replacement,
        }
    if isinstance(value, editor_mod.EditPlan):
        return {
            "actions": [jsonable(a) for a in value.actions],
            "unsupported": value.unsupported,
        }
    if dataclasses.is_dataclass(value):
        return {k: jsonable(v) for k, v in dataclasses.asdict(value).items()}
    raise TypeError(f"cannot serialise {type(value)!r}")


# --------------------------------------------------------------------------
# Input construction
# --------------------------------------------------------------------------

# Text fragments that exercise the awkward corners rather than the happy path:
# whitespace shapes, unicode that changes length when cased, multi-code-point
# emoji, and the word-boundary cases the two backtracking regexes turn on.
BASE_TEXTS = [
    "",
    " ",
    "   ",
    "\n",
    "\n\n\n\n",
    "hello",
    "hello world",
    "  hello   world  ",
    "hello\nworld",
    "hello \n world",
    "hello  ,  world",
    "the the dog",
    "the the the dog",
    "The THE dog",
    "the, the dog",
    "dog dog",
    "um hello",
    "hello um world",
    "Um, hello",
    "you know hello",
    "hello uh uh world",
    "straße",
    "STRASSE",
    "istanbul",
    "café",
    "naïve café",
    "ß",
    "hello world.",
    "hello world!",
    "already ends.",
    "ends with newline\n",
    "a",
    "I",
    "123",
    "hello 5 dogs",
]

# Every spoken punctuation phrase, in the contexts whose handling differs:
# bare, after a word, after auto-punctuation, after a determiner (the noun
# guard), at the start, and doubled.
def punctuation_inputs() -> list[str]:
    out: list[str] = []
    for phrase in punct_mod.SPOKEN_PUNCTUATION:
        out += [
            phrase,
            f"hello {phrase}",
            f"hello {phrase} world",
            f"hello. {phrase} world",
            f"hello, {phrase} world",
            f"{phrase} world",
            f"hello {phrase} {phrase} world",
            f"hello {phrase.upper()} world",
            f"add a {phrase} here",
            f"insert the {phrase} now",
            f"this {phrase} stays",
        ]
    for determiner in punct_mod.DETERMINERS:
        out.append(f"add {determiner} comma here")
    return out


def emoji_inputs() -> list[str]:
    out: list[str] = []
    for phrase in emoji_mod.SPOKEN_EMOJI:
        out += [
            phrase,
            f"hello {phrase}",
            f"hello {phrase} world",
            f"a {phrase} here",
            f"the {phrase} here",
            f"hello {phrase.upper()}",
        ]
    return out


def number_inputs() -> list[str]:
    out: list[str] = []
    words = list(numbers_mod.NUMBER_WORDS)
    for word in words:
        out += [word, f"{word} dogs", f"i have {word}", f"{word} percent", f"{word}."]
    for word, mult in itertools.product(words[:25], numbers_mod.MULTIPLIERS):
        out.append(f"{word} {mult}")
        out.append(f"{word} {mult} dogs")
    for symbol in numbers_mod.CURRENCY:
        out += [f"{symbol} five", f"five {symbol}", f"twenty {symbol}"]
    for word in numbers_mod.PERCENT_WORDS:
        out += [f"five {word}", f"one hundred {word}"]
    out += [
        "one point five",
        "three point one four",
        "one point two three four",
        "twenty five",
        "one hundred and twenty five",
        "a hundred",
        "hundred",
        "thousand",
        "million",
        "one idea",
        "two",
        "two dogs",
        "delete previous twenty five words",
        "five dollars",
        "5 dollars",
        "5 percent",
        "1,000 dollars",
        "3.5 percent",
    ]
    return out


def grammar_inputs() -> list[str]:
    """The editing grammar's full cross-product, plus its free-form Tier 2."""
    out: list[str] = list(grammar_mod.SIMPLE_EDITS)
    verbs = list(grammar_mod.VERB_OPS)
    units = list(grammar_mod.UNIT_WORDS)
    directions = list(grammar_mod.DIRECTION_WORDS)
    counts = ["", "two ", "twenty five ", "3 "]
    for verb, direction, count, unit in itertools.product(verbs, directions, counts, units):
        out.append(f"{verb} {direction} {count}{unit}")
    for edge, unit in itertools.product(grammar_mod.EDGE_WORDS, units):
        out.append(f"move to {edge} of {unit}")
        out.append(f"move to {edge} of the {unit}")
    out += [
        "select the old file",
        "delete the old file",
        "replace the old file with the new file",
        "move before the old file",
        "move after the old file",
        "select from here to there",
        "command mode",
        "dictation mode",
        "start over",
        "please start over",
        "start again",
        "new line",
        "new paragraph",
        "not a command at all",
    ]
    return out


def all_texts() -> list[str]:
    seen: dict[str, None] = {}
    for text in (
        BASE_TEXTS
        + punctuation_inputs()
        + emoji_inputs()
        + number_inputs()
        + grammar_inputs()
    ):
        seen.setdefault(text, None)
    return list(seen)


# --------------------------------------------------------------------------
# Stages
# --------------------------------------------------------------------------

FILLERS = ["um", "uh", "er", "ah", "erm", "hmm", "mhm", "you know"]

PRECEDINGS = [None, "", " ", "Hello.", "Hello. ", "Hello", "Hello,", "Hello\n", "Hello \t"]

PURPOSES = [
    None,
    "URL",
    "EMAIL",
    "TERMINAL",
    "PASSWORD",
    "PIN",
    "DIGITS",
    "NUMBER",
    "PHONE",
    "FREE_FORM",
    "UNKNOWN_PURPOSE",
]


def pure_stages(texts: list[str]) -> None:
    for text in texts:
        emit("normalize_spacing", {"text": text}, pipeline_mod.normalize_spacing(text))
        emit("collapse_repeated_words", {"text": text}, pipeline_mod.collapse_repeated_words(text))
        emit(
            "drop_filler_words",
            {"text": text, "fillers": FILLERS},
            pipeline_mod.drop_filler_words(text, FILLERS),
        )
        emit("sentence_case", {"text": text}, pipeline_mod.sentence_case(text))
        emit(
            "ensure_terminal_punctuation",
            {"text": text},
            pipeline_mod.ensure_terminal_punctuation(text),
        )
        emit(
            "apply_spoken_punctuation",
            {"text": text},
            punct_mod.apply_spoken_punctuation(text),
        )
        emit(
            "capitalize_after_terminators",
            {"text": text},
            punct_mod.capitalize_after_terminators(text),
        )
        emit(
            "apply_number_formatting",
            {"text": text},
            numbers_mod.apply_number_formatting(text),
        )
        emit(
            "attach_units_to_digits",
            {"text": text},
            numbers_mod.attach_units_to_digits(text),
        )
        emit("apply_spoken_emoji", {"text": text}, emoji_mod.apply_spoken_emoji(text))
        emit(
            "normalize_command_text",
            {"text": text},
            commands_mod.normalize_command_text(text),
        )
        emit("is_restart_request", {"text": text}, commands_mod.is_restart_request(text))
        emit("match_edit", {"normalized": text}, grammar_mod.match_edit(text))
        emit(
            "match_phrase_edit",
            {"normalized": text},
            grammar_mod.match_phrase_edit(text),
        )
        # Only for non-empty sources: `bounded_pattern("")` indexes source[0]
        # and raises. That is unreachable in practice because
        # `apply_replacements` skips empty sources first ("an empty pattern
        # matches everywhere; it can only do harm"), so the corpus honours the
        # same precondition rather than recording a crash the daemon cannot hit.
        if text:
            emit("bounded_pattern", {"source": text}, bounded_pattern(text))

    for preceding in PRECEDINGS:
        emit(
            "is_continuation",
            {"preceding": preceding},
            pipeline_mod.is_continuation(preceding),
        )
        emit("separator_for", {"preceding": preceding}, pipeline_mod.separator_for(preceding))

    for purpose in PURPOSES:
        emit("is_prose_field", {"purpose": purpose}, pipeline_mod.is_prose_field(purpose))
        for text in ["Hello World", "Rentals.Ca", "Ls -La", "STRASSE", ""]:
            emit(
                "undo_prose_casing",
                {"text": text, "purpose": purpose},
                pipeline_mod.undo_prose_casing(text, purpose),
            )


def command_stages(texts: list[str]) -> None:
    for text, mode_switching, command_mode in itertools.product(texts, [False, True], [False, True]):
        emit(
            "detect_command",
            {"text": text, "mode_switching": mode_switching, "command_mode": command_mode},
            commands_mod.detect_command(
                text, mode_switching=mode_switching, command_mode=command_mode
            ),
        )


def config_variants() -> list[CorrectionConfig]:
    """A spread of correction settings, not the full 2^6 cross-product.

    Each flag is exercised on and off, plus the two configurations that actually
    ship: the defaults, and Rocky's, which turns numbers and emoji on.
    """
    base = dict(
        enabled=True,
        dictionary_path="",
        drop_fillers=True,
        filler_words=FILLERS,
        collapse_repeats=True,
        spoken_punctuation=True,
        spoken_emoji=False,
        number_formatting=False,
    )
    variants = [dict(base)]
    for flag in [
        "drop_fillers",
        "collapse_repeats",
        "spoken_punctuation",
        "spoken_emoji",
        "number_formatting",
    ]:
        flipped = dict(base)
        flipped[flag] = not base[flag]
        variants.append(flipped)
    variants.append({**base, "spoken_emoji": True, "number_formatting": True})
    variants.append({**base, "enabled": False})
    return [CorrectionConfig(**v) for v in variants]


def config_args(config: CorrectionConfig) -> dict[str, Any]:
    return {
        "enabled": config.enabled,
        "drop_fillers": config.drop_fillers,
        "filler_words": list(config.filler_words),
        "collapse_repeats": config.collapse_repeats,
        "spoken_punctuation": config.spoken_punctuation,
        "spoken_emoji": config.spoken_emoji,
        "number_formatting": config.number_formatting,
    }


def rules_stages(texts: list[str]) -> None:
    configs = config_variants()
    for text, config in itertools.product(texts, configs):
        for preceding, prose in [(None, True), ("Hello.", True), ("Hello", True), (None, False)]:
            emit(
                "apply_rules",
                {
                    "text": text,
                    "config": config_args(config),
                    "preceding": preceding,
                    "prose": prose,
                },
                pipeline_mod.apply_rules(text, config, preceding=preceding, prose=prose),
            )


DICTIONARY = PersonalDictionary(
    bias_terms=["Rentals.ca"],
    replacements=[
        ("rentals api", "Rentals-API"),
        ("see plus plus", "C++"),
        ("dot ca", ".ca"),
        ("back slash", "\\"),
        ("group one", r"\1"),
    ],
)


def pipeline_stages(texts: list[str]) -> None:
    configs = config_variants()
    for text, config in itertools.product(texts, configs):
        for command_mode, purpose, preceding in [
            (False, None, None),
            (True, None, None),
            (False, "URL", None),
            (False, "TERMINAL", None),
            (False, None, "Hello"),
        ]:
            corrector = pipeline_mod.CorrectionPipeline(
                config,
                dictionary=DICTIONARY,
                mode_switching=True,
                command_mode=lambda cm=command_mode: cm,
                preceding_text=lambda p=preceding: p,
                field_purpose=lambda p=purpose: p,
            )
            result = corrector.correct(text)
            emit(
                "pipeline_correct",
                {
                    "text": text,
                    "config": config_args(config),
                    "command_mode": command_mode,
                    "purpose": purpose,
                    "preceding": preceding,
                },
                {
                    "raw_text": result.raw_text,
                    "corrected_text": result.corrected_text,
                    "action": jsonable(result.action),
                },
            )

    for text in texts:
        emit(
            "apply_replacements",
            {"text": text},
            apply_replacements(text, DICTIONARY),
        )


# --------------------------------------------------------------------------
# Editing
# --------------------------------------------------------------------------

SPAN_TEXTS = [
    "",
    "One sentence.",
    "One. Two. Three.",
    "One! Two? Three…",
    "One.  Two.   Three.",
    'He said "hi." Then left.',
    "Para one.\n\nPara two.\n\nPara three.",
    "Para one.\n\n  Para two.",
    "café. naïve. straße.",
    "no terminator here",
]


class _Model:
    """Stand-in for the daemon's TextModel, for corpus generation only."""

    def __init__(self, last: str | None, snapshot: FieldSnapshot | None) -> None:
        self._last = last
        self._snapshot = snapshot

    def last_insertion(self) -> str | None:
        return self._last

    def record_insertion(self, text: str) -> None:  # pragma: no cover - unused
        self._last = text

    def consume_last(self) -> str | None:
        last, self._last = self._last, None
        return last

    def read_field(self) -> FieldSnapshot | None:
        return self._snapshot

    def reset(self) -> None:  # pragma: no cover - unused
        self._last = None


def span_stages() -> None:
    units = [Unit.SENTENCE, Unit.PARAGRAPH]
    for text, unit in itertools.product(SPAN_TEXTS, units):
        emit(
            "boundaries",
            {"text": text, "unit": unit.value},
            spans_mod.boundaries(text, unit),
        )
        for caret in range(0, len(text) + 1):
            for count in [1, 2, 3]:
                emit(
                    "distance_back",
                    {"text": text, "caret": caret, "unit": unit.value, "count": count},
                    spans_mod.distance_back(text, caret, unit, count),
                )
                emit(
                    "distance_forward",
                    {"text": text, "caret": caret, "unit": unit.value, "count": count},
                    spans_mod.distance_forward(text, caret, unit, count),
                )

    phrase_texts = [
        ("the old file is here", "old file"),
        ("the old file and the old file", "old file"),
        ("THE OLD FILE", "old file"),
        ("the  old   file", "old file"),
        ("café and naïve", "café"),
        ("nothing matches", "absent"),
        ("", "x"),
    ]
    for text, phrase in phrase_texts:
        for caret in range(0, len(text) + 1):
            emit(
                "find_phrase",
                {"text": text, "caret": caret, "phrase": phrase},
                spans_mod.find_phrase(text, caret, phrase),
            )


def editor_stages() -> None:
    """Every EditAction the grammar can produce, against several field states."""
    actions: list[EditAction] = []
    for text in grammar_inputs():
        normalized = commands_mod.normalize_command_text(text)
        for matcher in (grammar_mod.match_edit, grammar_mod.match_phrase_edit):
            action = matcher(normalized)
            if action is not None and action not in actions:
                actions.append(action)

    models = [
        ("no-field-no-last", None, None),
        ("last-only", "hello world", None),
        ("last-unicode", "café ❤️", None),
        ("field-matching", "world", FieldSnapshot(text="hello world", caret=11)),
        ("field-mismatched", "world", FieldSnapshot(text="hello there", caret=11)),
        ("field-no-last", None, FieldSnapshot(text="the old file is here", caret=20)),
        ("field-long", None, FieldSnapshot(text="x" * 900, caret=900)),
    ]

    for action in actions:
        for name, last, snapshot in models:
            plan = editor_mod.compile_edit(action, _Model(last, snapshot))
            emit(
                "compile_edit",
                {"action": jsonable(action), "model": name},
                plan,
            )


def main() -> int:
    texts = all_texts()
    pure_stages(texts)
    command_stages(texts)
    rules_stages(texts)
    pipeline_stages(texts)
    span_stages()
    editor_stages()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
