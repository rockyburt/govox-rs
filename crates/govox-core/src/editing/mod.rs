//! Compile an editing *intent* into actuator keystrokes.
//!
//! Ported from `editing/editor.py`. The daemon routes an `EditAction` through
//! here before any injector sees it, so the actuator layer never has to know
//! what a "sentence" is.
//!
//! An intent the editor cannot satisfy returns an `unsupported` reason and
//! stops. It never degrades into typing the command phrase as text.

pub mod spans;

use crate::domain::{Direction, EditAction, EditOp, InsertionAction, TextModel, Unit};

/// Above this many characters a span command is refused rather than executed.
///
/// ydotool inserts a delay between keys, so a 600-character paragraph becomes a
/// multi-second keystroke storm the user cannot interrupt and cannot undo in one
/// step. Refusing is the kinder failure, and it is honest about why.
pub const MAX_SPAN_CHARS: usize = 400;

/// The actuator actions for one edit, or why there are none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditPlan {
    pub actions: Vec<InsertionAction>,
    pub unsupported: Option<String>,
}

impl EditPlan {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.unsupported.is_none()
    }

    fn keys(chords: Vec<String>) -> Self {
        Self {
            actions: vec![InsertionAction::Keys(chords)],
            unsupported: None,
        }
    }

    fn refuse(reason: impl Into<String>) -> Self {
        Self {
            actions: Vec::new(),
            unsupported: Some(reason.into()),
        }
    }
}

fn unit_name(unit: Unit) -> &'static str {
    match unit {
        Unit::Character => "character",
        Unit::Word => "word",
        Unit::Sentence => "sentence",
        Unit::Paragraph => "paragraph",
        Unit::Line => "line",
        Unit::Document => "document",
    }
}

/// Chords with no slots.
fn simple_chords(op: EditOp) -> Option<&'static [&'static str]> {
    Some(match op {
        EditOp::Undo => &["ctrl+z"],
        EditOp::Redo => &["ctrl+shift+z"],
        EditOp::Cut => &["ctrl+x"],
        EditOp::Copy => &["ctrl+c"],
        EditOp::Paste => &["ctrl+v"],
        EditOp::DeleteAll => &["ctrl+a", "backspace"],
        EditOp::SelectAll => &["ctrl+a"],
        // Collapses a selection to its trailing edge. There is no "deselect"
        // key; a plain arrow is how every toolkit drops a selection.
        EditOp::Deselect => &["right"],
        _ => return None,
    })
}

/// Per-unit `(backward, forward)` chords.
///
/// "Line" means two different things here, and both are right for their verb.
/// Deleting or selecting *the line* works from the caret to the line edge, which
/// is what a person means by "delete the line" mid-sentence. Moving *by a line*
/// changes line. Reconciling them would make one of the two surprising.
///
/// Sentence and paragraph are deliberately absent: no toolkit binds them, so
/// they fall through to span motion, which measures the real field.
fn unit_motion_chords(
    op: EditOp,
    unit: Unit,
) -> Option<(&'static [&'static str], &'static [&'static str])> {
    let table: &[(Unit, &[&str], &[&str])] = match op {
        EditOp::DeleteUnit => &[
            (Unit::Character, &["backspace"], &["delete"]),
            (Unit::Word, &["ctrl+backspace"], &["ctrl+delete"]),
            (
                Unit::Line,
                &["shift+home", "backspace"],
                &["shift+end", "delete"],
            ),
        ],
        EditOp::SelectUnit => &[
            (Unit::Character, &["shift+left"], &["shift+right"]),
            (Unit::Word, &["ctrl+shift+left"], &["ctrl+shift+right"]),
            (Unit::Line, &["shift+home"], &["shift+end"]),
        ],
        EditOp::MoveUnit => &[
            (Unit::Character, &["left"], &["right"]),
            (Unit::Word, &["ctrl+left"], &["ctrl+right"]),
            (Unit::Line, &["up"], &["down"]),
        ],
        _ => return None,
    };
    table
        .iter()
        .find(|(u, _, _)| *u == unit)
        .map(|(_, b, f)| (*b, *f))
}

fn is_unit_motion(op: EditOp) -> bool {
    matches!(
        op,
        EditOp::DeleteUnit | EditOp::SelectUnit | EditOp::MoveUnit
    )
}

/// Caret destinations for "move to beginning/end of &lt;unit&gt;".
///
/// Sentence and paragraph are absent for a second reason on top of the usual
/// one: GTK4 binds ctrl+up/ctrl+down to paragraph motion but Chromium, Electron
/// and Qt do not agree, and a caret that lands somewhere unexpected and then
/// receives a delete is the worst failure available here.
fn edge_chords(unit: Unit) -> Option<(&'static [&'static str], &'static [&'static str])> {
    match unit {
        Unit::Line => Some((&["home"], &["end"])),
        Unit::Document => Some((&["ctrl+home"], &["ctrl+end"])),
        _ => None,
    }
}

/// Per-character chords for the units no toolkit binds.
fn character_chords(op: EditOp) -> Option<(&'static str, &'static str)> {
    match op {
        EditOp::DeleteUnit => Some(("backspace", "delete")),
        EditOp::SelectUnit => Some(("shift+left", "shift+right")),
        EditOp::MoveUnit => Some(("left", "right")),
        _ => None,
    }
}

fn verb_name(op: EditOp) -> &'static str {
    match op {
        EditOp::DeleteUnit => "deleting",
        EditOp::SelectUnit => "selecting",
        EditOp::MoveUnit => "moving",
        _ => "editing",
    }
}

fn is_phrase_op(op: EditOp) -> bool {
    matches!(
        op,
        EditOp::SelectPhrase
            | EditOp::DeletePhrase
            | EditOp::ReplacePhrase
            | EditOp::MoveBeforePhrase
            | EditOp::MoveAfterPhrase
    )
}

/// Uppercase the first letter of each word, leaving the rest alone.
///
/// Not `str.title()`, which mangles apostrophes ("don't" → "Don'T"), and not a
/// full title-case, which would flatten acronyms the dictionary or the speaker
/// deliberately produced ("API key" must not become "Api Key").
fn capitalize_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, part) in text.split_inclusive(char::is_whitespace).enumerate() {
        let _ = index;
        // Split each chunk into its non-whitespace head and whitespace tail so
        // the transformation matches `re.sub(r"\S+", fix, text)`.
        let split = part.find(char::is_whitespace).unwrap_or(part.len());
        let (word, trailing) = part.split_at(split);
        out.push_str(&capitalize_first_alpha(word));
        out.push_str(trailing);
    }
    out
}

fn capitalize_first_alpha(word: &str) -> String {
    for (index, char) in word.char_indices() {
        if char.is_alphabetic() {
            let mut out = String::with_capacity(word.len());
            out.push_str(&word[..index]);
            out.extend(char.to_uppercase());
            out.push_str(&word[index + char.len_utf8()..]);
            return out;
        }
    }
    word.to_owned()
}

fn case_transform(op: EditOp, text: &str) -> Option<String> {
    match op {
        EditOp::UppercaseLast => Some(text.to_uppercase()),
        EditOp::LowercaseLast => Some(text.to_lowercase()),
        EditOp::CapitalizeLast => Some(capitalize_words(text)),
        _ => None,
    }
}

/// Repeat `chords` `count` times, matching Python's tuple repetition.
fn repeat(chords: &[&str], count: i64) -> Vec<String> {
    let count = usize::try_from(count).unwrap_or(0);
    let mut out = Vec::with_capacity(chords.len() * count);
    for _ in 0..count {
        out.extend(chords.iter().map(|c| (*c).to_owned()));
    }
    out
}

/// Plain arrow presses moving the caret `distance` characters.
fn travel(distance: i64) -> Vec<String> {
    if distance > 0 {
        repeat(&["right"], distance)
    } else {
        repeat(&["left"], -distance)
    }
}

/// One `Keys` action, unless it would be an uninterruptible keystroke storm.
fn bounded(chords: Vec<String>) -> EditPlan {
    if chords.is_empty() {
        return EditPlan::default();
    }
    if chords.len() > MAX_SPAN_CHARS {
        return EditPlan::refuse(format!(
            "that would take {} keystrokes; govox only spans up to {MAX_SPAN_CHARS}",
            chords.len()
        ));
    }
    EditPlan::keys(chords)
}

/// The last insertion, checked against the field when the field can be read.
///
/// Every "… that" command works by counting characters govox remembers typing
/// and firing that many keystrokes at the caret. If the caret has moved since,
/// that count is aimed at the wrong text — the failure the TTL bounds but cannot
/// prevent, because nothing on Wayland tells an unprivileged process that focus
/// changed.
///
/// A readable field closes that gap. If the field cannot be read, the remembered
/// text is returned unverified: that is the pre-existing behaviour and the
/// common case, and it degrades silently on purpose.
fn verified_last(model: &dyn TextModel, verb: &str) -> Result<String, String> {
    let last = model
        .last_insertion()
        .filter(|l| !l.is_empty())
        .ok_or_else(|| {
            format!("nothing dictated to {verb} — govox has no record of the last insertion")
        })?;

    let Some(snapshot) = model.read_field() else {
        return Ok(last);
    };

    if snapshot.preceding(last.chars().count()) != last {
        return Err(format!(
            "cannot {verb} it — the text before the caret is no longer what govox typed, \
             so the caret has moved"
        ));
    }
    Ok(last)
}

#[must_use]
pub fn compile_edit(action: &EditAction, model: &dyn TextModel) -> EditPlan {
    if let Some(chords) = simple_chords(action.op) {
        return EditPlan::keys(chords.iter().map(|c| (*c).to_owned()).collect());
    }

    match action.op {
        EditOp::DeleteLast => return compile_delete_last(model),
        EditOp::SelectLast => return compile_select_last(model),
        _ => {}
    }

    if case_transform(action.op, "").is_some() {
        return compile_case_transform(action, model);
    }
    if is_unit_motion(action.op) {
        return compile_unit_motion(action, model);
    }
    if action.op == EditOp::MoveToEdge {
        return compile_move_to_edge(action);
    }
    if is_phrase_op(action.op) {
        return compile_phrase_edit(action, model);
    }

    EditPlan::refuse(format!("unhandled edit operation {:?}", action.op))
}

fn compile_delete_last(model: &dyn TextModel) -> EditPlan {
    match verified_last(model, "delete") {
        // One backspace per character govox itself typed. Exact, and needs no
        // ability to read the field — reading only makes it safer.
        Ok(last) => EditPlan::keys(repeat(&["backspace"], last.chars().count() as i64)),
        Err(reason) => EditPlan::refuse(reason),
    }
}

fn compile_select_last(model: &dyn TextModel) -> EditPlan {
    match verified_last(model, "select") {
        Ok(last) => EditPlan::keys(repeat(&["shift+left"], last.chars().count() as i64)),
        Err(reason) => EditPlan::refuse(reason),
    }
}

/// Retype the last utterance with its case changed.
///
/// No toolkit binds a case transform to a key, so unlike every other Tier 1
/// command this cannot be a pure keystroke. It works because the target is text
/// govox itself typed. That is also its limit.
fn compile_case_transform(action: &EditAction, model: &dyn TextModel) -> EditPlan {
    let last = match verified_last(model, "change") {
        Ok(last) => last,
        Err(reason) => return EditPlan::refuse(reason),
    };
    let transformed = case_transform(action.op, &last).expect("op is a case transform");
    if transformed == last {
        // Nothing to do; doing it anyway would blank and retype for no reason —
        // visible flicker, and a pointless race with the user.
        return EditPlan::default();
    }
    EditPlan {
        actions: vec![
            InsertionAction::Keys(repeat(&["backspace"], last.chars().count() as i64)),
            InsertionAction::Text(transformed),
        ],
        unsupported: None,
    }
}

fn compile_unit_motion(action: &EditAction, model: &dyn TextModel) -> EditPlan {
    let verb = verb_name(action.op);
    let (Some(unit), Some(direction)) = (action.unit, action.direction) else {
        return EditPlan::refuse(format!("{verb} needs both a unit and a direction"));
    };

    if let Some((backward, forward)) = unit_motion_chords(action.op, unit) {
        let per_unit = if direction == Direction::Previous {
            backward
        } else {
            forward
        };
        return EditPlan::keys(repeat(per_unit, action.count));
    }

    if spans::is_supported(unit) {
        return compile_span_motion(action, model, verb);
    }

    EditPlan::refuse(format!("{verb} by {} is not supported", unit_name(unit)))
}

/// Sentence and paragraph motion, measured against the real field.
///
/// Every other Tier 1 command is a fixed chord. This one cannot be: nothing in
/// any toolkit knows where a sentence ends, so the distance has to be computed
/// and then walked one character at a time.
fn compile_span_motion(action: &EditAction, model: &dyn TextModel, verb: &str) -> EditPlan {
    let unit = action.unit.expect("checked by caller");
    let direction = action.direction.expect("checked by caller");

    let Some(snapshot) = model.read_field() else {
        return EditPlan::refuse(format!(
            "{verb} by {} needs to read the focused field, which this application does not expose",
            unit_name(unit)
        ));
    };

    let count = usize::try_from(action.count).unwrap_or(0);
    let distance = if direction == Direction::Previous {
        spans::distance_back(&snapshot.text, snapshot.caret, unit, count)
    } else {
        spans::distance_forward(&snapshot.text, snapshot.caret, unit, count)
    };

    let Some(distance) = distance else {
        let where_ = if direction == Direction::Previous {
            "before"
        } else {
            "after"
        };
        return EditPlan::refuse(format!("no {} {where_} the caret", unit_name(unit)));
    };
    // distance is never 0: boundaries are strictly before or after the caret,
    // so a caret sitting exactly on one moves to the *next* boundary out.
    if distance > MAX_SPAN_CHARS {
        return EditPlan::refuse(format!(
            "that {} is {distance} characters long; govox only spans up to {MAX_SPAN_CHARS} by keystroke",
            unit_name(unit)
        ));
    }

    let (backward, forward) = character_chords(action.op).expect("unit-motion op");
    let key = if direction == Direction::Previous {
        backward
    } else {
        forward
    };
    EditPlan::keys(repeat(&[key], distance as i64))
}

fn compile_move_to_edge(action: &EditAction) -> EditPlan {
    let (Some(unit), Some(direction)) = (action.unit, action.direction) else {
        return EditPlan::refuse("move needs both a unit and an edge");
    };
    let Some((beginning, end)) = edge_chords(unit) else {
        return EditPlan::refuse(format!(
            "moving to the edge of a {} needs to read the focused field, which is unavailable on this desktop",
            unit_name(unit)
        ));
    };
    let chords = if direction == Direction::Previous {
        beginning
    } else {
        end
    };
    EditPlan::keys(chords.iter().map(|c| (*c).to_owned()).collect())
}

/// Tier 2: act on a span named by its content.
///
/// Every one of these is the same three steps — find the phrase, walk the caret
/// to it, then do the ordinary thing. Only the last step differs, which is why
/// they share a compiler rather than having five.
///
/// Selection is built by walking rather than by AT-SPI's `add_selection`: that
/// call reports success on GTK4 and then produces no selection at all, and
/// writing through the accessibility bus is not something this codebase does
/// anyway — Chromium accepts reads and silently drops writes.
fn compile_phrase_edit(action: &EditAction, model: &dyn TextModel) -> EditPlan {
    let Some(phrase) = action.phrase.as_deref().filter(|p| !p.is_empty()) else {
        return EditPlan::refuse("that command needs something to find");
    };

    let Some(snapshot) = model.read_field() else {
        return EditPlan::refuse(format!(
            "cannot find \u{201c}{phrase}\u{201d} — this application does not expose its text"
        ));
    };

    let Some((start, end)) = spans::find_phrase(&snapshot.text, snapshot.caret, phrase) else {
        return EditPlan::refuse(format!("\u{201c}{phrase}\u{201d} is not in the field"));
    };

    let caret = snapshot.caret as i64;
    let length = (end - start) as i64;

    if action.op == EditOp::MoveBeforePhrase {
        return bounded(travel(start as i64 - caret));
    }
    if action.op == EditOp::MoveAfterPhrase {
        return bounded(travel(end as i64 - caret));
    }

    // The rest all start from the end of the match and select backwards over it.
    let mut chords = travel(end as i64 - caret);
    chords.extend(repeat(&["shift+left"], length));

    match action.op {
        EditOp::SelectPhrase => bounded(chords),
        EditOp::DeletePhrase => {
            chords.push("backspace".to_owned());
            bounded(chords)
        }
        EditOp::ReplacePhrase => {
            chords.push("backspace".to_owned());
            let plan = bounded(chords);
            if !plan.ok() {
                return plan;
            }
            // The TextAction goes through the injector like any dictation, so
            // it picks up nothing special and behaves identically everywhere.
            let mut actions = plan.actions;
            actions.push(InsertionAction::Text(
                action.replacement.clone().unwrap_or_default(),
            ));
            EditPlan {
                actions,
                unsupported: None,
            }
        }
        _ => EditPlan::refuse(format!("unhandled edit operation {:?}", action.op)),
    }
}
