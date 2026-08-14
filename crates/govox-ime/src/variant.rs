//! IBus's serializable objects, built by hand.
//!
//! libibus normally builds these for you and their GVariant layout is
//! **documented nowhere**. Reproducing it was the single biggest unknown in
//! M10. Every layout below was read off the running system rather than guessed
//! — the two registration types with `gdbus call` during the M-1(b) spike, and
//! the three preedit types by asking libibus itself to serialize one:
//!
//! ```console
//! $ python3 -c 'import gi; gi.require_version("IBus","1.0")
//! from gi.repository import IBus
//! print(IBus.Text.new_from_string("hi").serialize_object().print_(True))'
//! ('IBusText', @a{sv} {}, 'hi', <('IBusAttrList', @a{sv} {}, @av [])>)
//! ```
//!
//! | Type | Signature |
//! |---|---|
//! | `IBusText` | `(sa{sv}sv)` — tag, attachments, text, attribute list |
//! | `IBusAttrList` | `(sa{sv}av)` |
//! | `IBusAttribute` | `(sa{sv}uuuu)` — type, value, start, end |
//! | `IBusEngineDesc` | `(sa{sv}ssssssssussssssss)` |
//! | `IBusComponent` | `(sa{sv}ssssssssavav)` |
//!
//! Reach for the same technique whenever another IBus type is needed. It beats
//! reading libibus's C source, and unlike the C source it cannot be out of date
//! with respect to the daemon actually running.

use std::collections::HashMap;

use zvariant::{Str, StructureBuilder, Value};

/// `IBusAttrType.UNDERLINE`.
const ATTR_TYPE_UNDERLINE: u32 = 1;
/// `IBusAttrUnderline.SINGLE`.
const ATTR_UNDERLINE_SINGLE: u32 = 1;

/// How a client must treat a preedit it is still showing when focus moves away.
///
/// **There is deliberately no `COMMIT`.** IBus's default is COMMIT, which makes
/// the *application* commit whatever preedit is pending when it loses focus —
/// including when govox deactivates its engine at the end of a session. That
/// turned a half-heard "delete that" into the literal words "delete that" in
/// the document, which is precisely what provisional text is supposed to make
/// impossible.
///
/// So the mode is not a parameter with a safe value; it is a type with one
/// inhabitant. `govox-py` guards this with a comment and a negative test. Here
/// no code path can express the mistake, because the value COMMIT would need
/// cannot be constructed: the field is private and [`PreeditFocusMode::CLEAR`]
/// is the only constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreeditFocusMode(u32);

impl PreeditFocusMode {
    /// Discard a pending preedit on focus loss. The only mode govox uses.
    pub const CLEAR: Self = Self(0);

    /// The wire value, for the `UpdatePreeditText` signal's `mode` argument.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// The empty `a{sv}` attachments dictionary every serializable object carries.
fn attachments<'a>() -> HashMap<Str<'a>, Value<'a>> {
    HashMap::new()
}

/// An `IBusText`, underlined over its whole length when it is not empty.
///
/// The underline is what makes preedit *look* provisional, and it is the only
/// attribute govox sets. Empty text still carries an (empty) attribute list
/// rather than no list at all — that is what libibus emits, and matching it
/// keeps the wire bytes identical to what every client is already tested
/// against.
///
/// The attribute's start and end are **character** offsets, like every other
/// offset in this project. `govox-py` passes `len(text)`, a code-point count;
/// `str::len()` here would underline past the end of a string with any
/// non-ASCII in it.
#[must_use]
pub fn text(body: &str) -> Value<'static> {
    let attributes: Vec<Value<'static>> = if body.is_empty() {
        Vec::new()
    } else {
        let end = u32::try_from(body.chars().count()).unwrap_or(u32::MAX);
        vec![attribute(
            ATTR_TYPE_UNDERLINE,
            ATTR_UNDERLINE_SINGLE,
            0,
            end,
        )]
    };
    // `attributes` goes in bare, not through `Value::new`: a `Value` placed in
    // a structure becomes an explicit `v`, and this field is an `av`. The
    // *outer* field below is a `v` and does want the wrapping.
    let list = Value::new(("IBusAttrList".to_owned(), attachments(), attributes));
    Value::new(("IBusText".to_owned(), attachments(), body.to_owned(), list))
}

/// One `IBusAttribute`, as the variant an `IBusAttrList` holds.
fn attribute(kind: u32, value: u32, start: u32, end: u32) -> Value<'static> {
    Value::new((
        "IBusAttribute".to_owned(),
        attachments(),
        kind,
        value,
        start,
        end,
    ))
}

/// An `IBusEngineDesc` describing govox's engine.
///
/// `rank` is 0 on purpose: a higher rank makes the desktop offer the engine as
/// a default input source, and an engine that types nothing until you speak is
/// a bad thing to land on by accident.
///
/// Nineteen fields, so this is assembled a field at a time: Rust's tuple
/// conversions stop at sixteen and IBus's layout does not care.
#[must_use]
pub fn engine_desc(name: &str) -> Value<'static> {
    let mut builder = StructureBuilder::new()
        .add_field("IBusEngineDesc".to_owned())
        .add_field(attachments())
        .add_field(name.to_owned())
        .add_field("govox dictation".to_owned())
        .add_field("Speech dictation as provisional text".to_owned())
        .add_field("en".to_owned())
        .add_field("MIT".to_owned())
        .add_field("govox".to_owned())
        .add_field("audio-input-microphone".to_owned())
        .add_field("us".to_owned())
        .add_field(0_u32); // rank
    // hotkeys, symbol, setup, layout_variant, layout_option, version,
    // textdomain, icon_prop_key — all unused, all still required.
    for _ in 0..8 {
        builder.push_field(String::new());
    }
    Value::Structure(builder.build().expect("a fully populated structure"))
}

/// An `IBusComponent` wrapping one engine description.
///
/// `exec` is empty on purpose: the engine lives inside the govox process, so
/// there is nothing for ibus-daemon to spawn. `packaging/ibus/govox.xml` in
/// `govox-py` says the same. A non-empty `exec` would have the daemon start a
/// second copy of something on demand.
#[must_use]
pub fn component(bus_name: &str, engine_name: &str) -> Value<'static> {
    let s = |value: &str| value.to_owned();
    let observed_paths: Vec<Value<'static>> = Vec::new();
    Value::new((
        s("IBusComponent"),
        attachments(),
        s(bus_name),
        s("govox dictation"),
        s("0.1"),
        s("MIT"),
        s("govox"),
        s(""), // homepage
        s(""), // exec — the engine is in-process
        s(""), // textdomain
        observed_paths,
        vec![engine_desc(engine_name)],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signature of a built value, as it will appear on the wire.
    fn signature(value: &Value<'_>) -> String {
        value.value_signature().to_string()
    }

    #[test]
    fn text_matches_the_layout_libibus_emits() {
        assert_eq!(signature(&text("hi")), "(sa{sv}sv)");
    }

    #[test]
    fn empty_text_still_carries_an_attribute_list() {
        // libibus emits ('IBusText', {}, '', <('IBusAttrList', {}, @av [])>)
        // for an empty string — the list is empty, not absent. Clearing the
        // preedit sends exactly this, so it is on the hot path, not an edge.
        assert_eq!(signature(&text("")), "(sa{sv}sv)");
    }

    #[test]
    fn the_underline_spans_characters_not_bytes() {
        // "café" is 5 bytes and 4 characters. A byte length here would ask the
        // client to underline one position past the end of the string.
        let Value::Structure(outer) = text("café") else {
            panic!("IBusText is a structure");
        };
        let Value::Value(list) = &outer.fields()[3] else {
            panic!("the fourth field is the attribute list variant");
        };
        let Value::Structure(list) = list.as_ref() else {
            panic!("IBusAttrList is a structure");
        };
        let Value::Array(attributes) = &list.fields()[2] else {
            panic!("the third field is the attribute array");
        };
        let Value::Value(first) = &attributes[0] else {
            panic!("attributes are variants");
        };
        let Value::Structure(first) = first.as_ref() else {
            panic!("IBusAttribute is a structure");
        };
        assert_eq!(u32::try_from(&first.fields()[5]).unwrap(), 4);
    }

    #[test]
    fn engine_desc_and_component_match_the_layouts_read_off_the_daemon() {
        assert_eq!(
            signature(&engine_desc("govox")),
            "(sa{sv}ssssssssussssssss)"
        );
        assert_eq!(
            signature(&component("org.freedesktop.IBus.Govox", "govox")),
            "(sa{sv}ssssssssavav)"
        );
    }

    #[test]
    fn the_engine_is_never_a_default_input_source() {
        let Value::Structure(desc) = engine_desc("govox") else {
            panic!("IBusEngineDesc is a structure");
        };
        assert_eq!(u32::try_from(&desc.fields()[10]).unwrap(), 0, "rank");
    }

    #[test]
    fn the_component_asks_the_daemon_to_spawn_nothing() {
        // A non-empty exec would have ibus-daemon start a second govox on
        // demand. The engine is served by the daemon that registered it.
        let Value::Structure(component) = component("org.freedesktop.IBus.Govox", "govox") else {
            panic!("IBusComponent is a structure");
        };
        assert_eq!(<&str>::try_from(&component.fields()[8]).unwrap(), "");
    }

    /// The negative test for the third "silent success" trap.
    ///
    /// It cannot fail at runtime, because the mistake it guards against does
    /// not compile: `PreeditFocusMode(1)` is a private-field construction and
    /// there is no COMMIT constant to reach for. What this asserts is that the
    /// *only* value the type can hold is still the one that discards.
    #[test]
    fn the_only_preedit_focus_mode_is_clear() {
        assert_eq!(PreeditFocusMode::CLEAR.as_u32(), 0);
        const { assert!(PreeditFocusMode::CLEAR.as_u32() == 0) };
    }
}
