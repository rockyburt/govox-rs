//! Golden corpus: ~239k recorded calls pinning the correction pipeline.
//!
//! Each record in `corpus/correction.jsonl.gz` is one call and its answer:
//! `{stage, args, out}`. The test replays every one and asserts the answer is
//! unchanged. This is the project's largest safety net, and it guards the code
//! that most needs one — pure logic, an enormous input space, and failure modes
//! that are silent rather than loud. A character-vs-byte offset or a stage
//! reordering does not crash; it puts slightly wrong text in a document.
//!
//! **A diff here means govox's behaviour changed.** The only question is
//! whether that was intended. If it was, re-record and read the diff:
//!
//! ```console
//! $ GOVOX_BLESS=1 cargo test -p govox-core --test correction_golden -- --ignored bless
//! ```
//!
//! Blessing recomputes `out` for every existing record from the current code,
//! and adds records for any table-driven input not already covered — so a new
//! spoken emoji or punctuation phrase gains coverage by being added to its
//! table. Inputs are otherwise preserved verbatim, which keeps the diff small
//! enough to actually review.
//!
//! History: the expected values were originally recorded by running an earlier
//! Python implementation, which is how the port was verified. That is
//! provenance now, not process — the corpus regenerates from govox itself and
//! needs nothing outside this repository. See `docs/parity.md` for why
//! individual behaviours are the way they are.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use govox_core::config::CorrectionConfig;
use govox_core::correction::{
    self, Context, CorrectionPipeline, casing, commands, dictionary, emoji, grammar, numbers,
    punctuation, spelling,
};
use govox_core::domain::{
    EditAction, FieldSnapshot, InsertionAction, PersonalDictionary, PipelineAction, TextModel, Unit,
};
use govox_core::editing::{self, spans};
use serde_json::{Value, json};

const CORPUS: &[u8] = include_bytes!("../../../corpus/correction.jsonl.gz");

/// The dictionary the generator used. Kept in step with `correction_corpus.py`.
fn dictionary_fixture() -> PersonalDictionary {
    PersonalDictionary {
        bias_terms: vec!["Rentals.ca".into()],
        replacements: vec![
            ("rentals api".into(), "Rentals-API".into()),
            ("see plus plus".into(), "C++".into()),
            ("dot ca".into(), ".ca".into()),
            ("back slash".into(), "\\".into()),
            ("group one".into(), r"\1".into()),
        ],
    }
}

fn action_json(action: &PipelineAction) -> Value {
    match action {
        PipelineAction::Text(text) => json!({"kind": "text", "text": text}),
        PipelineAction::Command(name) => json!({"kind": "command", "name": name}),
        PipelineAction::Mode { command_mode } => {
            json!({"kind": "mode", "command_mode": command_mode})
        }
        PipelineAction::Sleep { asleep } => json!({"kind": "sleep", "asleep": asleep}),
        PipelineAction::Spelling { enabled } => json!({"kind": "spelling", "enabled": enabled}),
        PipelineAction::Edit(edit) => edit_json(edit),
    }
}

fn edit_json(edit: &EditAction) -> Value {
    json!({
        "kind": "edit",
        "op": op_name(edit),
        "unit": edit.unit.map(unit_name),
        "direction": edit.direction.map(direction_name),
        "count": edit.count,
        "phrase": edit.phrase,
        "replacement": edit.replacement,
    })
}

fn op_name(edit: &EditAction) -> &'static str {
    use govox_core::domain::EditOp as O;
    match edit.op {
        O::Undo => "undo",
        O::Redo => "redo",
        O::DeleteLast => "delete_last",
        O::DeleteUnit => "delete_unit",
        O::DeleteAll => "delete_all",
        O::Cut => "cut",
        O::Copy => "copy",
        O::Paste => "paste",
        O::SelectAll => "select_all",
        O::SelectLast => "select_last",
        O::Deselect => "deselect",
        O::SelectUnit => "select_unit",
        O::MoveUnit => "move_unit",
        O::MoveToEdge => "move_to_edge",
        O::PressKey => "press_key",
        O::UppercaseLast => "uppercase_last",
        O::LowercaseLast => "lowercase_last",
        O::CapitalizeLast => "capitalize_last",
        O::SelectPhrase => "select_phrase",
        O::DeletePhrase => "delete_phrase",
        O::ReplacePhrase => "replace_phrase",
        O::MoveBeforePhrase => "move_before_phrase",
        O::MoveAfterPhrase => "move_after_phrase",
    }
}

fn unit_name(unit: govox_core::domain::Unit) -> &'static str {
    use govox_core::domain::Unit as U;
    match unit {
        U::Character => "character",
        U::Word => "word",
        U::Sentence => "sentence",
        U::Paragraph => "paragraph",
        U::Line => "line",
        U::Document => "document",
    }
}

fn direction_name(direction: govox_core::domain::Direction) -> &'static str {
    match direction {
        govox_core::domain::Direction::Previous => "previous",
        govox_core::domain::Direction::Next => "next",
    }
}

fn correction_config(args: &Value) -> CorrectionConfig {
    CorrectionConfig {
        enabled: args["enabled"].as_bool().unwrap(),
        dictionary_path: String::new(),
        drop_fillers: args["drop_fillers"].as_bool().unwrap(),
        filler_words: args["filler_words"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect(),
        collapse_repeats: args["collapse_repeats"].as_bool().unwrap(),
        spoken_punctuation: args["spoken_punctuation"].as_bool().unwrap(),
        spoken_emoji: args["spoken_emoji"].as_bool().unwrap(),
        number_formatting: args["number_formatting"].as_bool().unwrap(),
        // Absent from every record written before spoken case control existed.
        // Defaulting to false is what makes those records replay unchanged:
        // the stage is a no-op unless a record explicitly asked for it.
        case_control: args
            .get("case_control")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

fn rules_name(rules: correction::FieldRules) -> &'static str {
    match rules {
        correction::FieldRules::Prose => "prose",
        correction::FieldRules::SingleToken => "single_token",
        correction::FieldRules::SpacedWords => "spaced_words",
    }
}

/// The field rules a record asks for.
///
/// Records written before verbatim fields were split in two carry a `prose`
/// boolean instead. `false` maps to `SingleToken`, which is exactly what the
/// pipeline did when those answers were recorded — one arm for every verbatim
/// purpose, and no separating space in any of them. Mapping it to `SpacedWords`
/// would silently re-record a behaviour those inputs never described.
fn field_rules_from(args: &Value) -> correction::FieldRules {
    match args.get("rules").and_then(Value::as_str) {
        Some("prose") => correction::FieldRules::Prose,
        Some("single_token") => correction::FieldRules::SingleToken,
        Some("spaced_words") => correction::FieldRules::SpacedWords,
        Some(other) => panic!("unknown field rules {other}"),
        None if args["prose"].as_bool().unwrap() => correction::FieldRules::Prose,
        None => correction::FieldRules::SingleToken,
    }
}

fn text(args: &Value, key: &str) -> String {
    args[key].as_str().unwrap_or_default().to_owned()
}

fn optional(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Evaluate one record. `None` means the stage is not implemented here yet, and
/// the caller counts it as skipped rather than passing.
fn evaluate(stage: &str, args: &Value) -> Option<Value> {
    let out = match stage {
        "normalize_spacing" => json!(correction::normalize_spacing(&text(args, "text"))),
        "collapse_repeated_words" => {
            json!(correction::collapse_repeated_words(&text(args, "text")))
        }
        "drop_filler_words" => {
            let fillers: Vec<String> = args["fillers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect();
            json!(correction::drop_filler_words(&text(args, "text"), &fillers))
        }
        "sentence_case" => json!(correction::sentence_case(&text(args, "text"))),
        "ensure_terminal_punctuation" => {
            json!(correction::ensure_terminal_punctuation(&text(args, "text")))
        }
        "apply_spoken_punctuation" => {
            json!(punctuation::apply_spoken_punctuation(&text(args, "text")))
        }
        "capitalize_after_terminators" => {
            json!(punctuation::capitalize_after_terminators(&text(
                args, "text"
            )))
        }
        "apply_number_formatting" => {
            json!(numbers::apply_number_formatting(&text(args, "text")))
        }
        "attach_units_to_digits" => json!(numbers::attach_units_to_digits(&text(args, "text"))),
        "apply_spoken_emoji" => json!(emoji::apply_spoken_emoji(&text(args, "text"))),
        "apply_case_control" => json!(casing::apply_case_control(&text(args, "text"))),
        "normalize_command_text" => {
            json!(commands::normalize_command_text(&text(args, "text")))
        }
        "is_restart_request" => json!(commands::is_restart_request(&text(args, "text"))),
        "match_edit" => match grammar::match_edit(&text(args, "normalized")) {
            Some(edit) => edit_json(&edit),
            None => Value::Null,
        },
        // The `normalized` key predates the split between the folded and the
        // case-preserving normalisations, and is kept so ~2.5k records stay
        // byte-identical. What it now carries is the case-preserving form.
        "match_phrase_edit" => match grammar::match_phrase_edit(&text(args, "normalized")) {
            Some(edit) => edit_json(&edit),
            None => Value::Null,
        },
        "bounded_pattern" => json!(dictionary::bounded_pattern(&text(args, "source"))),
        "apply_replacements" => {
            json!(dictionary::apply_replacements(
                &text(args, "text"),
                &dictionary_fixture()
            ))
        }
        "is_continuation" => json!(correction::is_continuation(
            optional(args, "preceding").as_deref()
        )),
        "separator_for" => json!(correction::separator_for(
            optional(args, "preceding").as_deref()
        )),
        "is_prose_field" => json!(correction::is_prose_field(
            optional(args, "purpose").as_deref()
        )),
        "undo_prose_casing" => json!(correction::undo_prose_casing(
            &text(args, "text"),
            optional(args, "purpose").as_deref()
        )),
        "spell" => match spelling::spell(&text(args, "text")) {
            Some(out) => json!({"text": out.text, "unrecognised": out.unrecognised}),
            None => Value::Null,
        },
        "split_trailing_command" => match commands::split_trailing_command(
            &text(args, "text"),
            args["mode_switching"].as_bool().unwrap(),
        ) {
            Some((prefix, action)) => json!({"prefix": prefix, "action": action_json(&action)}),
            None => Value::Null,
        },
        "detect_command" => action_json(&commands::detect_command(
            &text(args, "text"),
            args["mode_switching"].as_bool().unwrap(),
            args["command_mode"].as_bool().unwrap(),
        )),
        "apply_rules" => json!(correction::apply_rules(
            &text(args, "text"),
            &correction_config(&args["config"]),
            optional(args, "preceding").as_deref(),
            field_rules_from(args),
        )),
        "field_rules" => json!(rules_name(correction::field_rules(
            optional(args, "purpose").as_deref()
        ))),
        "pipeline_correct" => {
            // Memoised on the config: constructing a pipeline compiles the
            // dictionary's patterns, and the corpus holds ~100k records across
            // only a handful of configurations.
            thread_local! {
                static PIPELINES: std::cell::RefCell<BTreeMap<String, CorrectionPipeline>> =
                    const { std::cell::RefCell::new(BTreeMap::new()) };
            }
            let key = args["config"].to_string();
            PIPELINES.with(|cache| {
                let mut cache = cache.borrow_mut();
                let pipeline = cache.entry(key).or_insert_with(|| {
                    CorrectionPipeline::new(
                        correction_config(&args["config"]),
                        dictionary_fixture(),
                        true,
                    )
                });
                let context = Context {
                    command_mode: args["command_mode"].as_bool().unwrap(),
                    preceding_text: optional(args, "preceding"),
                    field_purpose: optional(args, "purpose"),
                    // No custom commands in the corpus. They are loaded from a
                    // config file, so recording them would make every one of
                    // these records depend on one — and the pipeline is built
                    // here without any, which is the state the whole corpus was
                    // recorded in.
                    app: None,
                };
                let result = pipeline.correct(&text(args, "text"), &context);
                json!({
                    "raw_text": result.raw_text,
                    "corrected_text": result.corrected_text,
                    "action": action_json(&result.action),
                })
            })
        }
        "boundaries" => json!(spans::boundaries(
            &text(args, "text"),
            unit_from(args, "unit")
        )),
        "distance_back" => json!(spans::distance_back(
            &text(args, "text"),
            args["caret"].as_u64().unwrap() as usize,
            unit_from(args, "unit"),
            args["count"].as_u64().unwrap() as usize,
        )),
        "distance_forward" => json!(spans::distance_forward(
            &text(args, "text"),
            args["caret"].as_u64().unwrap() as usize,
            unit_from(args, "unit"),
            args["count"].as_u64().unwrap() as usize,
        )),
        "find_phrase" => match spans::find_phrase(
            &text(args, "text"),
            args["caret"].as_u64().unwrap() as usize,
            &text(args, "phrase"),
        ) {
            Some((start, end)) => json!([start, end]),
            None => Value::Null,
        },
        "compile_edit" => {
            let action = edit_from(&args["action"]);
            let model = CorpusModel::named(args["model"].as_str().unwrap());
            let plan = editing::compile_edit(&action, &model);
            json!({
                "actions": plan.actions.iter().map(insertion_json).collect::<Vec<_>>(),
                "unsupported": plan.unsupported,
            })
        }
        _ => return None,
    };
    Some(out)
}

fn insertion_json(action: &InsertionAction) -> Value {
    match action {
        InsertionAction::Text(text) => json!({"kind": "text", "text": text}),
        InsertionAction::Command(name) => json!({"kind": "command", "name": name}),
        InsertionAction::Keys(chords) => json!({"kind": "keys", "chords": chords}),
    }
}

fn unit_from(args: &Value, key: &str) -> Unit {
    match args[key].as_str().unwrap() {
        "character" => Unit::Character,
        "word" => Unit::Word,
        "sentence" => Unit::Sentence,
        "paragraph" => Unit::Paragraph,
        "line" => Unit::Line,
        "document" => Unit::Document,
        other => panic!("unknown unit {other}"),
    }
}

fn edit_from(value: &Value) -> EditAction {
    use govox_core::domain::EditOp as O;
    let op = match value["op"].as_str().unwrap() {
        "undo" => O::Undo,
        "redo" => O::Redo,
        "delete_last" => O::DeleteLast,
        "delete_unit" => O::DeleteUnit,
        "delete_all" => O::DeleteAll,
        "cut" => O::Cut,
        "copy" => O::Copy,
        "paste" => O::Paste,
        "select_all" => O::SelectAll,
        "select_last" => O::SelectLast,
        "deselect" => O::Deselect,
        "select_unit" => O::SelectUnit,
        "move_unit" => O::MoveUnit,
        "move_to_edge" => O::MoveToEdge,
        "press_key" => O::PressKey,
        "uppercase_last" => O::UppercaseLast,
        "lowercase_last" => O::LowercaseLast,
        "capitalize_last" => O::CapitalizeLast,
        "select_phrase" => O::SelectPhrase,
        "delete_phrase" => O::DeletePhrase,
        "replace_phrase" => O::ReplacePhrase,
        "move_before_phrase" => O::MoveBeforePhrase,
        "move_after_phrase" => O::MoveAfterPhrase,
        other => panic!("unknown op {other}"),
    };
    EditAction {
        op,
        unit: value["unit"]
            .as_str()
            .map(|u| unit_from(&json!({"u": u}), "u")),
        direction: value["direction"].as_str().map(|d| match d {
            "previous" => govox_core::domain::Direction::Previous,
            "next" => govox_core::domain::Direction::Next,
            other => panic!("unknown direction {other}"),
        }),
        count: value["count"].as_i64().unwrap_or(1),
        phrase: value["phrase"].as_str().map(str::to_owned),
        replacement: value["replacement"].as_str().map(str::to_owned),
    }
}

/// The field states the generator used, by name.
struct CorpusModel {
    last: Option<String>,
    snapshot: Option<FieldSnapshot>,
}

impl CorpusModel {
    fn named(name: &str) -> Self {
        let field = |text: &str, caret: usize| {
            Some(FieldSnapshot {
                text: text.into(),
                caret,
            })
        };
        match name {
            "no-field-no-last" => Self {
                last: None,
                snapshot: None,
            },
            "last-only" => Self {
                last: Some("hello world".into()),
                snapshot: None,
            },
            "last-unicode" => Self {
                last: Some("café ❤️".into()),
                snapshot: None,
            },
            "field-matching" => Self {
                last: Some("world".into()),
                snapshot: field("hello world", 11),
            },
            "field-mismatched" => Self {
                last: Some("world".into()),
                snapshot: field("hello there", 11),
            },
            "field-no-last" => Self {
                last: None,
                snapshot: field("the old file is here", 20),
            },
            "field-long" => Self {
                last: None,
                snapshot: field(&"x".repeat(900), 900),
            },
            other => panic!("unknown model {other}"),
        }
    }
}

impl TextModel for CorpusModel {
    fn last_insertion(&self) -> Option<String> {
        self.last.clone()
    }
    fn record_insertion(&self, _text: &str) {}
    fn consume_last(&self) -> Option<String> {
        self.last.clone()
    }
    fn read_field(&self) -> Option<FieldSnapshot> {
        self.snapshot.clone()
    }
    fn reset(&self) {}
}

/// Replay every Nth record instead of all of them.
///
/// The full corpus takes ~3 minutes, essentially all of it inside `fancy-regex`
/// (the two patterns needing a backreference and a lookahead are the price of
/// the exact `\b` semantics this pipeline depends on). That is fine on `main`,
/// which always runs the whole thing, but slow for a gate run on every save —
/// so `GOVOX_GOLDEN_SAMPLE=50` gives a representative pass in a couple of
/// seconds.
///
/// Sampling is strided rather than random: the corpus is written in a stable
/// order, so a stride hits every stage and every config variant, and the same
/// stride always selects the same records. A failure found under sampling is
/// reproducible without it.
fn sample_stride() -> usize {
    std::env::var("GOVOX_GOLDEN_SAMPLE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 1)
        .unwrap_or(1)
}

#[test]
fn correction_matches_the_golden_corpus() {
    let reader = BufReader::new(GzDecoder::new(CORPUS));
    let stride = sample_stride();

    let mut seen = 0usize;
    let mut checked = 0usize;
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();
    // Per-stage tallies, so a wholly-broken stage is obvious rather than being
    // buried in the first few reported failures.
    let mut failed_by_stage: BTreeMap<String, usize> = BTreeMap::new();

    for line in reader.lines() {
        let line = line.expect("corpus line reads");
        if line.trim().is_empty() {
            continue;
        }
        seen += 1;
        if stride > 1 && !seen.is_multiple_of(stride) {
            continue;
        }
        let record: Value = serde_json::from_str(&line).expect("corpus line parses");
        let stage = record["stage"].as_str().expect("stage").to_owned();
        let args = &record["args"];

        let Some(actual) = evaluate(&stage, args) else {
            *skipped.entry(stage).or_default() += 1;
            continue;
        };
        checked += 1;

        let expected = &record["out"];
        if &actual != expected {
            *failed_by_stage.entry(stage.clone()).or_default() += 1;
            if failures.len() < 25 {
                failures.push(format!(
                    "{stage}\n    args:     {args}\n    expected: {expected}\n    actual:   {actual}"
                ));
            }
        }
    }

    assert!(checked > 0, "corpus produced no comparable records");

    if !failures.is_empty() {
        let total: usize = failed_by_stage.values().sum();
        let summary: Vec<String> = failed_by_stage
            .iter()
            .map(|(s, n)| format!("{s}: {n}"))
            .collect();
        panic!(
            "{total} of {checked} golden records changed\n\
             per stage: {}\n\n\
             If the change was intended, re-record with:\n  \
             GOVOX_BLESS=1 cargo test -p govox-core --test correction_golden -- --ignored bless\n\n\
             first {} failures:\n  {}",
            summary.join(", "),
            failures.len(),
            failures.join("\n  ")
        );
    }

    if !skipped.is_empty() {
        // Not a failure: editing stages land in the second half of M2. Printed
        // so the gap is visible rather than silently tolerated.
        let summary: Vec<String> = skipped.iter().map(|(s, n)| format!("{s}: {n}")).collect();
        println!(
            "checked {checked}; stages not yet ported: {}",
            summary.join(", ")
        );
    } else if stride > 1 {
        println!("checked {checked} of {seen} records (stride {stride}), every stage ported");
    } else {
        println!("checked {checked} records, every stage ported");
    }
}

/// Where the corpus lives, relative to this crate.
const CORPUS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../corpus/correction.jsonl.gz"
);

/// Inputs generated from the lookup tables themselves, so a phrase added to a
/// table gains golden coverage by being added.
///
/// This is the half the corpus could not previously grow: its inputs were swept
/// from another implementation's tables, so an entry that only exists here could
/// never appear. The templates mirror that original sweep — a phrase alone, in
/// the middle of a sentence, at either end, upper-cased, and behind a determiner
/// (which suppresses the rule).
fn table_driven_inputs() -> Vec<(&'static str, String)> {
    let mut out = Vec::new();

    for (phrase, _) in emoji::SPOKEN_EMOJI {
        for text in [
            (*phrase).to_owned(),
            format!("hello {phrase}"),
            format!("hello {phrase} world"),
            format!("a {phrase} here"),
            format!("the {phrase} here"),
            format!("hello {}", phrase.to_uppercase()),
        ] {
            out.push(("apply_spoken_emoji", text));
        }
    }

    for entry in punctuation::SPOKEN_PUNCTUATION {
        let phrase = entry.0;
        for text in [
            (*phrase).to_owned(),
            format!("hello {phrase}"),
            format!("hello {phrase} world"),
            format!("{phrase} world"),
            format!("hello {phrase} {phrase} world"),
            format!("hello {} world", phrase.to_uppercase()),
            format!("add a {phrase} here"),
        ] {
            out.push(("apply_spoken_punctuation", text));
        }
    }
    for (marker, _) in casing::CASE_MARKERS {
        for text in [
            format!("{marker} hello world"),
            format!("say {marker} hello there"),
            format!("hello {marker}"),
            format!("{marker} on hello there {marker} off world"),
            format!("{marker} on hello there"),
            format!("{} hello", marker.to_uppercase()),
            format!("{marker} on one\ntwo"),
        ] {
            out.push(("apply_case_control", text));
        }
    }

    for determiner in punctuation::DETERMINERS {
        out.push((
            "apply_spoken_punctuation",
            format!("add {determiner} comma here"),
        ));
    }

    for (word, _) in numbers::NUMBER_WORDS {
        for text in [
            (*word).to_owned(),
            format!("{word} dogs"),
            format!("i have {word}"),
            format!("{word} percent"),
            format!("{word}."),
        ] {
            out.push(("apply_number_formatting", text));
        }
    }
    for (symbol, _) in numbers::CURRENCY {
        for text in [
            format!("{symbol} five"),
            format!("five {symbol}"),
            format!("twenty {symbol}"),
        ] {
            out.push(("apply_number_formatting", text));
        }
    }

    out
}

/// Every table-driven record, as `(stage, args)`.
///
/// [`table_driven_inputs`] covers the lookup tables, whose records are all a
/// bare `text`. The rest are written here because they are argument *matrices*
/// rather than tables: the field-rules cases below need a purpose or a
/// preceding-text alongside the text, which that shape cannot express.
fn table_driven_records() -> Vec<(&'static str, Value)> {
    let mut out: Vec<(&'static str, Value)> = table_driven_inputs()
        .into_iter()
        .map(|(stage, text)| (stage, json!({ "text": text })))
        .collect();

    // Every purpose govox knows about, plus one it does not and the absent
    // case, so a purpose moving between the two verbatim sets shows up as a
    // diff rather than as silence.
    for purpose in correction::VERBATIM_PURPOSES
        .iter()
        .copied()
        .chain(["FREE_FORM", "NAME", ""])
    {
        out.push(("field_rules", json!({ "purpose": purpose })));
        out.push(("is_prose_field", json!({ "purpose": purpose })));
    }
    out.push(("field_rules", json!({})));

    // The matrix that was missing, and with it the bug: the corpus recorded
    // verbatim fields only ever at an empty caret, so nothing ever asked what
    // happens when a second utterance lands against existing text. Every
    // combination of rules and caret is now pinned.
    let config = json!({
        "enabled": true,
        "drop_fillers": true,
        "filler_words": ["um", "uh"],
        "collapse_repeats": true,
        "spoken_punctuation": true,
        "spoken_emoji": true,
        "number_formatting": true,
        "case_control": true,
    });
    for rules in ["prose", "single_token", "spaced_words"] {
        for text in ["list files", "dot com", "all caps hello"] {
            for preceding in [
                Value::Null,
                json!(""),
                json!("ls -la"),
                json!("ls "),
                json!("example"),
                json!("Hello."),
                json!("one\n"),
            ] {
                let mut args = json!({
                    "text": text,
                    "config": config,
                    "rules": rules,
                });
                if let Some(preceding) = preceding.as_str() {
                    args["preceding"] = json!(preceding);
                }
                out.push(("apply_rules", args));
            }
        }
    }

    // Every spoken key name, through the grammar and through the front door.
    // The table is the security boundary for a silent-failure API, so every row
    // of it should be recorded rather than sampled.
    for (spoken, _) in grammar::PRESS_KEYS {
        for text in [
            format!("press {spoken}"),
            format!("press the {spoken}"),
            format!("press {spoken} key"),
        ] {
            out.push(("match_edit", json!({ "normalized": text })));
            out.push((
                "detect_command",
                json!({
                    "text": text,
                    "mode_switching": false,
                    "command_mode": false,
                }),
            ));
        }
    }

    // Formatting commands through the front door. "space bar" joined them and
    // nothing had been recording that COMMANDS reaches `detect_command` at all.
    for (phrase, _) in commands::COMMANDS {
        out.push((
            "detect_command",
            json!({
                "text": phrase,
                "mode_switching": false,
                "command_mode": false,
            }),
        ));
    }

    // "numeral <n>" against every number word, since it exists to override the
    // bare-small-number rule and that rule is keyed on the value.
    for (word, _) in numbers::NUMBER_WORDS {
        out.push((
            "apply_number_formatting",
            json!({ "text": format!("numeral {word}") }),
        ));
    }

    // Modifier chords. Every modifier against one key, and every chord key
    // against one modifier — the cross product would be 9 × 49 records to pin
    // a `join("+")`, while these two axes catch a table row that does not
    // translate, which is the failure that matters.
    for (spoken, _) in grammar::MODIFIER_WORDS {
        out.push((
            "match_edit",
            json!({ "normalized": format!("press {spoken} s") }),
        ));
    }
    for (spoken, _) in grammar::CHORD_KEYS {
        for text in [
            format!("press control {spoken}"),
            // The bare form, which must stay text.
            format!("press {spoken}"),
        ] {
            out.push(("match_edit", json!({ "normalized": text })));
        }
    }
    for text in [
        "press control shift z",
        "press control control s",
        "press shift tab",
        "press control enter",
        "press the control s",
    ] {
        out.push(("match_edit", json!({ "normalized": text })));
        out.push((
            "detect_command",
            json!({ "text": text, "mode_switching": false, "command_mode": false }),
        ));
    }

    // Trailing commands. The scan is what makes a command work mid-session
    // under streaming, and its risk is the mirror image: prose that happens to
    // end in command words. Both sides are recorded.
    for text in [
        "so i said hello command mode",
        "here is the text delete previous three words",
        "some words kill last word",
        "some words undo that",
        "some words press enter",
        "some words press control s",
        "some words new line",
        "some words space bar",
        "some words dictate",
        "some words move to end of the document",
        "Hello there. Delete that.",
        // Prose that must survive untouched.
        "this is just a sentence",
        "i pressed the button",
        "the last word was hers",
        "we should select a venue",
        "one two three four five six seven eight nine words",
        "the quick brown fox delete the old draft",
        // Whole-utterance forms, which are not splits.
        "command mode",
        "delete that",
    ] {
        for mode_switching in [true, false] {
            out.push((
                "split_trailing_command",
                json!({ "text": text, "mode_switching": mode_switching }),
            ));
        }
    }

    // The spelling phrases, and the alphabet itself.
    for (phrase, _) in commands::SPELLING_COMMANDS {
        for mode_switching in [true, false] {
            out.push((
                "detect_command",
                json!({
                    "text": phrase,
                    "mode_switching": mode_switching,
                    "command_mode": false,
                }),
            ));
        }
    }
    for text in [
        "alpha bravo charlie",
        "romeo oscar charlie kilo yankee",
        "capital alpha bravo",
        "alpha dash one two",
        "alpha at bravo dot charlie",
        "a b c",
        "alpha wibble bravo",
        "the quick brown fox jumped",
    ] {
        out.push(("spell", json!({ "text": text })));
    }

    // The sleep phrases. Ungated by `mode_switching` — they are honoured
    // whatever the mode, because while asleep waking is the only thing that
    // works — so both settings must agree.
    for (phrase, _) in commands::SLEEP_COMMANDS {
        for mode_switching in [true, false] {
            out.push((
                "detect_command",
                json!({
                    "text": phrase,
                    "mode_switching": mode_switching,
                    "command_mode": false,
                }),
            ));
        }
        out.push((
            "split_trailing_command",
            json!({ "text": format!("some words {phrase}"), "mode_switching": true }),
        ));
    }

    // Every mode phrase, at both settings of the switch that enables them. A
    // mode phrase is matched in either mode, so adding one silently takes that
    // phrase away from ordinary dictation — the corpus should say so.
    for (phrase, _) in commands::MODE_COMMANDS {
        for mode_switching in [true, false] {
            out.push((
                "detect_command",
                json!({
                    "text": phrase,
                    "mode_switching": mode_switching,
                    "command_mode": false,
                }),
            ));
        }
    }

    // The tier 2 slots against case, which the corpus had no record of: every
    // `match_phrase_edit` input in it was already folded, so nothing had ever
    // asked what happens to a capital. That is what let the replacement slot be
    // lower-cased for as long as it was — the command could not fix a name, and
    // 239k records agreed it was fine. `phrase` must fold (it is a search key)
    // while `replacement` must not (it is typed into the document), and only a
    // cased input can tell the two apart.
    for text in [
        "replace rocky with Rocky",
        "replace rentsync with RentSync",
        "Replace the old file with the New File",
        "replace the old file with the new file",
        "Delete The Old Draft",
        "Select The Heading",
        "move before The Table",
        "move after The Table",
    ] {
        out.push(("match_phrase_edit", json!({ "normalized": text })));
        for command_mode in [true, false] {
            // Through the front door as well: tier 2 is gated, and the gate is
            // what decides whether the cased form is ever reached.
            out.push((
                "detect_command",
                json!({
                    "text": text,
                    "mode_switching": false,
                    "command_mode": command_mode,
                }),
            ));
        }
    }

    out
}

/// Re-record the corpus from the current implementation.
///
/// Ignored because it rewrites a checked-in fixture: blessing is a deliberate
/// act whose diff is the thing being reviewed, never a side effect of running
/// the suite. `GOVOX_BLESS=1` is required on top of `--ignored`, so neither
/// `cargo test -- --ignored` nor a stray `--include-ignored` can rewrite the
/// corpus by accident.
///
/// Records whose answer is unchanged keep their original line **byte for byte**.
/// Re-serialising all ~239k would reformat every line and bury the handful that
/// actually changed, which would defeat the review this exists for.
#[test]
#[ignore = "rewrites corpus/correction.jsonl.gz; run deliberately with GOVOX_BLESS=1"]
fn bless_the_golden_corpus() {
    assert!(
        std::env::var("GOVOX_BLESS").is_ok(),
        "refusing to rewrite the corpus without GOVOX_BLESS=1"
    );

    let reader = BufReader::new(GzDecoder::new(CORPUS));
    let mut lines: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut rerecorded: BTreeMap<String, usize> = BTreeMap::new();
    let mut kept = 0usize;

    for line in reader.lines() {
        let line = line.expect("corpus line reads");
        if line.trim().is_empty() {
            continue;
        }
        let mut record: Value = serde_json::from_str(&line).expect("corpus line parses");
        let stage = record["stage"].as_str().expect("stage").to_owned();
        seen.insert(format!("{stage}\u{0}{}", record["args"]));

        match evaluate(&stage, &record["args"]) {
            Some(actual) if actual != record["out"] => {
                *rerecorded.entry(stage).or_default() += 1;
                record["out"] = actual;
                lines.push(serde_json::to_string(&record).expect("record serialises"));
            }
            // Unchanged, or a stage this test does not evaluate: keep the
            // original bytes so the diff shows only real movement.
            _ => {
                kept += 1;
                lines.push(line);
            }
        }
    }

    let mut added: BTreeMap<String, usize> = BTreeMap::new();
    for (stage, args) in table_driven_records() {
        if !seen.insert(format!("{stage}\u{0}{args}")) {
            continue;
        }
        let out = evaluate(stage, &args).expect("table-driven stages are all evaluated");
        *added.entry(stage.to_owned()).or_default() += 1;
        lines.push(
            serde_json::to_string(&json!({"stage": stage, "args": args, "out": out}))
                .expect("record serialises"),
        );
    }

    let file = std::fs::File::create(CORPUS_PATH).expect("corpus is writable");
    let mut encoder = GzEncoder::new(file, Compression::best());
    for line in &lines {
        writeln!(encoder, "{line}").expect("corpus line writes");
    }
    encoder.finish().expect("corpus finishes");

    let summary = |m: &BTreeMap<String, usize>| {
        m.iter()
            .map(|(s, n)| format!("{s}: {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!(
        "wrote {} records to {CORPUS_PATH}\n  unchanged: {kept}\n  re-recorded: {}\n  added: {}",
        lines.len(),
        if rerecorded.is_empty() {
            "none".to_owned()
        } else {
            summary(&rerecorded)
        },
        if added.is_empty() {
            "none".to_owned()
        } else {
            summary(&added)
        },
    );
}
