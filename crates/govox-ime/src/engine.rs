//! The engine object ibus-daemon drives, and the field state it learns from.
//!
//! An input method is told things no other part of govox can find out on
//! Wayland: where the caret is, what the document says around it, and what kind
//! of field has focus. All three arrive as unsolicited calls from the client,
//! on whatever task zbus dispatches them to, and are read later from the
//! dictation path — so they are cached in [`FieldState`] rather than fetched on
//! demand. `govox-py` reached the same conclusion for the same reason.

use std::collections::HashSet;
use std::sync::Mutex;

use govox_core::domain::CaretRect;
use zvariant::{OwnedObjectPath, Value};

use crate::variant::PreeditFocusMode;

/// The interface name of the engine object.
pub const ENGINE_INTERFACE: &str = "org.freedesktop.IBus.Engine";

/// `IBusInputPurpose`, resolved to a name.
///
/// Stored by name so that nothing outside this crate needs IBus's enums to make
/// sense of a field purpose — the correction pipeline takes a `&str`.
fn purpose_name(purpose: u32) -> String {
    match purpose {
        0 => "FREE_FORM",
        1 => "ALPHA",
        2 => "DIGITS",
        3 => "NUMBER",
        4 => "PHONE",
        5 => "URL",
        6 => "EMAIL",
        7 => "NAME",
        8 => "PASSWORD",
        9 => "PIN",
        10 => "TERMINAL",
        11 => "DATE",
        12 => "TIME",
        13 => "DATETIME",
        // A purpose this build does not know is still worth reporting: the
        // number is enough to look up, and dropping it would make a new IBus
        // release look like a client that reports nothing.
        other => return other.to_string(),
    }
    .to_owned()
}

/// `IBUS_KEY_Escape`, from `ibuskeysyms.h`. The X11 keysym, not a keycode.
const IBUS_KEY_ESCAPE: u32 = 0xff1b;

/// `IBUS_RELEASE_MASK`, bit 30 of the modifier state: this event is a release.
const IBUS_RELEASE_MASK: u32 = 1 << 30;

/// Everything the focused client has told us about its field.
///
/// Each entry describes **one** field and none is re-reported by a client that
/// has nothing to say, so [`FieldState::forget`] clears all of them together
/// when focus moves. Left standing they are read as facts about the *new*
/// field, and each fails differently: a stale caret sends the HUD to another
/// monitor (which is how this was found), stale surrounding text decides
/// wrongly whether an utterance continues a sentence, and a stale `TERMINAL`
/// lowercases the first word of ordinary prose.
#[derive(Debug, Default)]
pub struct FieldState {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// The object path of the engine currently receiving input.
    ///
    /// IBus creates one engine per input context through the factory, so
    /// "which one do I drive?" is answered by focus, not by construction order.
    active: Option<OwnedObjectPath>,
    caret: Option<CaretRect>,
    surrounding_before: Option<String>,
    purpose: Option<String>,
    caret_logged: bool,
    surrounding_logged: bool,
    content_types_seen: HashSet<(u32, u32)>,
    /// Whether a dictation session is running, as the daemon last said.
    ///
    /// The engine consults this and nothing else before consuming a key: with
    /// no session there is nothing to stop, so Escape must reach the
    /// application untouched.
    session_active: bool,
    /// Told when the engine consumed a stop key.
    stop_tx: Option<tokio::sync::mpsc::UnboundedSender<()>>,
}

impl FieldState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The engine object that currently has focus, if any.
    #[must_use]
    pub fn active(&self) -> Option<OwnedObjectPath> {
        self.lock().active.clone()
    }

    /// Where to report a consumed stop key, and the session state that decides
    /// whether one can be consumed at all.
    pub fn set_stop_channel(&self, tx: tokio::sync::mpsc::UnboundedSender<()>) {
        self.lock().stop_tx = Some(tx);
    }

    /// The daemon telling us a session started or ended.
    pub fn set_session_active(&self, active: bool) {
        self.lock().session_active = active;
    }

    /// Consume this key as a stop, or decline it.
    ///
    /// Returns whether the key was consumed, which is what
    /// [`Engine::process_key_event`] hands back to IBus. Consuming is the whole
    /// point: it is the only path on which govox can stop a key reaching the
    /// application, which is what makes a *single* Escape safe here when the
    /// evdev path needs two.
    fn take_stop(&self, is_escape: bool) -> bool {
        let inner = self.lock();
        if !is_escape || !inner.session_active {
            return false;
        }
        match &inner.stop_tx {
            // Only consume if the request can actually be delivered. Swallowing
            // the key and then failing to stop would leave dictation running
            // *and* eat the Escape — the worst of both.
            Some(tx) => tx.send(()).is_ok(),
            None => false,
        }
    }

    /// What kind of field has focus, by name.
    #[must_use]
    pub fn purpose(&self) -> Option<String> {
        self.lock().purpose.clone()
    }

    /// The document text before the caret, as last pushed by the client.
    #[must_use]
    pub fn surrounding_before(&self) -> Option<String> {
        self.lock().surrounding_before.clone()
    }

    /// The caret rectangle in screen coordinates, as last reported.
    #[must_use]
    pub fn caret(&self) -> Option<CaretRect> {
        self.lock().caret
    }

    fn set_active(&self, path: &OwnedObjectPath) {
        self.lock().active = Some(path.clone());
    }

    /// Drop everything the previous focused field told us.
    fn forget(&self) {
        let mut inner = self.lock();
        inner.caret = None;
        inner.surrounding_before = None;
        inner.purpose = None;
    }

    fn set_caret(&self, rect: CaretRect) {
        let first = {
            let mut inner = self.lock();
            inner.caret = Some(rect);
            let first = !inner.caret_logged;
            inner.caret_logged = true;
            first
        };
        if first {
            // Once per process at INFO: whether clients report a usable caret
            // decides if the HUD can follow it, and it varies by toolkit.
            // Logging the first makes that answerable from a log.
            tracing::info!(?rect, "IBus reported a caret location");
        } else {
            // The rest at DEBUG. The INFO line answers "does this desktop
            // report carets at all", not "does *this application* report one
            // that matches where the caret visibly is" — and clients disagree.
            // A misplaced HUD needs the rectangle the app actually sent.
            tracing::debug!(?rect, "IBus caret location");
        }
    }

    fn set_surrounding_before(&self, text: Option<String>) {
        let first = {
            let mut inner = self.lock();
            let first = !inner.surrounding_logged && text.is_some();
            inner.surrounding_before = text;
            if first {
                inner.surrounding_logged = true;
            }
            first
        };
        if first {
            tracing::info!("IBus client provides surrounding text; continuation is context-aware");
        }
    }

    fn set_content_type(&self, purpose: u32, hints: u32) {
        let name = purpose_name(purpose);
        let first = {
            let mut inner = self.lock();
            inner.purpose = Some(name.clone());
            inner.content_types_seen.insert((purpose, hints))
        };
        if first {
            // One line per distinct combination, so moving between a URL bar, a
            // terminal and a text area produces a short readable inventory
            // rather than a flood.
            tracing::info!(purpose = %name, hints, "IBus content type");
        }
    }

    /// A poisoned lock here means a panic while holding purely descriptive
    /// state. Recovering it keeps dictation working, which is strictly better
    /// than propagating the panic into the D-Bus dispatch task.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One engine, as ibus-daemon sees it.
///
/// Every method here runs on a zbus dispatch task and must return promptly:
/// while this engine is active it sits between the user's keyboard and their
/// application.
pub struct Engine {
    pub(crate) state: std::sync::Arc<FieldState>,
    pub(crate) path: OwnedObjectPath,
}

#[zbus::interface(name = "org.freedesktop.IBus.Engine")]
impl Engine {
    /// Pass every key straight through, except a single Escape that ends a
    /// running session.
    ///
    /// An active input method sees every keystroke in the focused field. That
    /// is inherent to being one, and it is a surface govox does not otherwise
    /// have — so this **never logs, counts by key, or retains anything**. Do
    /// not add telemetry here; whatever the question is, this is not the place
    /// to answer it. The one comparison below is against a single constant, it
    /// records nothing about the key, and every other key returns on the same
    /// line it always did.
    ///
    /// Why this is the exception: consuming is the only way to stop a key
    /// reaching the application, which is what lets a *single* Escape end
    /// dictation here — the evdev path cannot swallow, so it needs a double
    /// tap. The cost is that Escape behaves differently between an IBus-routed
    /// field and one that ignores IBus; that is documented rather than hidden,
    /// because the alternative is a stop key that sometimes leaks through.
    fn process_key_event(&self, keyval: u32, _keycode: u32, state: u32) -> bool {
        // Presses only. A release carries IBUS_RELEASE_MASK, and consuming it
        // as well would send a second stop for one keypress.
        if state & IBUS_RELEASE_MASK != 0 {
            return false;
        }
        self.state.take_stop(keyval == IBUS_KEY_ESCAPE)
    }

    /// The client telling us where the caret is, in screen coordinates.
    ///
    /// Coverage is not guaranteed: a client that never reports, or reports
    /// zeroes, leaves the HUD where it was. The daemon treats this as a hint
    /// and keeps the screen corner as its fallback.
    fn set_cursor_location(&self, x: i32, y: i32, w: i32, h: i32) {
        self.state.set_caret((x, y, w, h));
    }

    /// Focus arrived. Reset here as well as in `FocusOut`, because `FocusOut`
    /// is not guaranteed: IBus has several focus vfunc generations and a client
    /// may only ever drive one of them. `FocusIn` is the one observed to
    /// arrive, and it is also the safer place — it runs before the new client
    /// reports anything, so nothing it sends is thrown away.
    fn focus_in(&self) {
        self.state.forget();
        self.state.set_active(&self.path);
    }

    /// The modern focus call, carrying the input context that took focus.
    fn focus_in_id(&self, _object_path: &str, _client: &str) {
        self.focus_in();
    }

    fn focus_out(&self) {
        self.state.forget();
    }

    fn focus_out_id(&self, _object_path: &str) {
        self.state.forget();
    }

    fn enable(&self) {
        self.state.forget();
        self.state.set_active(&self.path);
    }

    fn disable(&self) {
        self.state.forget();
    }

    fn reset(&self) {}

    /// The client pushing the document text around the caret.
    ///
    /// Only the part **before** the caret is kept. That is all the sentence
    /// logic needs, and keeping less of the user's document than necessary is
    /// the right default for something that sees everything they type.
    ///
    /// `cursor_pos` is a character offset, as every IBus offset is.
    fn set_surrounding_text(&self, text: Value<'_>, cursor_pos: u32, _anchor_pos: u32) {
        let Some(full) = ibus_text_body(&text) else {
            tracing::debug!("unreadable surrounding text");
            return;
        };
        let caret = cursor_pos as usize;
        let before: String = full.chars().take(caret).collect();
        self.state.set_surrounding_before(Some(before));
    }

    /// What the daemon does with the keys we decline. govox needs none of them.
    fn set_capabilities(&self, _caps: u32) {}

    fn page_up(&self) {}
    fn page_down(&self) {}
    fn cursor_up(&self) {}
    fn cursor_down(&self) {}
    fn candidate_clicked(&self, _index: u32, _button: u32, _state: u32) {}
    fn property_activate(&self, _name: &str, _state: u32) {}
    fn property_show(&self, _name: &str) {}
    fn property_hide(&self, _name: &str) {}

    /// The client describing what kind of field has focus.
    ///
    /// On the wire this is a **write-only property**, not a method — libibus's
    /// `do_set_content_type` vfunc is a local convenience over a property set,
    /// which is not something the Python could have revealed.
    ///
    /// Read by the correction pipeline, which stands the prose rules down
    /// outside ordinary writing: a trailing full stop breaks a URL and a
    /// capital breaks a shell command.
    #[zbus(property, name = "ContentType")]
    fn set_content_type(&self, value: (u32, u32)) {
        let (purpose, hints) = value;
        self.state.set_content_type(purpose, hints);
    }

    /// Tell the daemon this engine understands the id-carrying focus calls.
    #[zbus(property, name = "FocusId")]
    fn focus_id(&self) -> (bool,) {
        (true,)
    }

    /// Ask clients to push surrounding text. They only do so when asked.
    #[zbus(property, name = "ActiveSurroundingText")]
    fn active_surrounding_text(&self) -> (bool,) {
        (true,)
    }
}

/// The text out of an `IBusText` variant, `(sa{sv}sv)`.
///
/// Returns `None` for anything that is not one, which is the honest answer to a
/// client sending something unexpected — the alternative is guessing at the
/// user's document contents.
fn ibus_text_body(value: &Value<'_>) -> Option<String> {
    let inner = match value {
        Value::Value(boxed) => boxed.as_ref(),
        other => other,
    };
    let Value::Structure(fields) = inner else {
        return None;
    };
    let fields = fields.fields();
    // 0: the "IBusText" tag, 1: attachments, 2: the string, 3: attributes.
    <&str>::try_from(fields.get(2)?).ok().map(ToOwned::to_owned)
}

/// The arguments of an `UpdatePreeditText` signal.
///
/// The signal is `(v, u, b, u)`: text, caret, visible, **mode**. libibus offers
/// `update_preedit_text()` and `update_preedit_text_with_mode()` as two calls,
/// but there is only one signal and the mode is never optional on the wire —
/// which is the strongest possible form of the guarantee the type
/// [`PreeditFocusMode`] exists to give.
#[must_use]
pub fn preedit_args(text: &str, visible: bool) -> (Value<'static>, u32, bool, u32) {
    let caret = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
    (
        crate::variant::text(text),
        caret,
        visible,
        PreeditFocusMode::CLEAR.as_u32(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purposes_are_reported_by_name_and_unknown_ones_by_number() {
        assert_eq!(purpose_name(0), "FREE_FORM");
        assert_eq!(purpose_name(5), "URL");
        assert_eq!(purpose_name(10), "TERMINAL");
        // A purpose added by a future IBus is still worth reporting.
        assert_eq!(purpose_name(99), "99");
    }

    #[test]
    fn focus_clears_every_cache_together() {
        let state = FieldState::new();
        state.set_caret((1, 2, 3, 4));
        state.set_surrounding_before(Some("hello".into()));
        state.set_content_type(10, 0);
        assert_eq!(state.purpose().as_deref(), Some("TERMINAL"));

        state.forget();

        // All three, not just the caret: a purpose left standing lowercases the
        // first word of prose in the *next* field.
        assert_eq!(state.caret(), None);
        assert_eq!(state.surrounding_before(), None);
        assert_eq!(state.purpose(), None);
    }

    #[test]
    fn only_the_text_before_the_caret_is_kept_and_it_is_counted_in_characters() {
        let value = crate::variant::text("café au lait");
        let body = ibus_text_body(&value).expect("an IBusText carries its string");
        assert_eq!(body, "café au lait");
        // Character 6 of "café au lait" is "café a"; byte 6 would split the é.
        let before: String = body.chars().take(6).collect();
        assert_eq!(before, "café a");
    }

    #[test]
    fn a_variant_that_is_not_an_ibus_text_is_declined_rather_than_guessed_at() {
        assert_eq!(ibus_text_body(&Value::new(42_u32)), None);
        assert_eq!(ibus_text_body(&Value::new("bare string")), None);
    }

    #[test]
    fn preedit_always_carries_the_clearing_mode() {
        // There is one signal and the mode is a required argument, so this is
        // the only shape a preedit update can take.
        let (_, caret, visible, mode) = preedit_args("café", true);
        assert_eq!(caret, 4, "the caret sits after the last character");
        assert!(visible);
        assert_eq!(mode, PreeditFocusMode::CLEAR.as_u32());
    }

    #[test]
    fn clearing_sends_an_empty_invisible_preedit() {
        let (_, caret, visible, mode) = preedit_args("", false);
        assert_eq!(caret, 0);
        assert!(!visible);
        assert_eq!(mode, PreeditFocusMode::CLEAR.as_u32());
    }

    #[test]
    fn escape_is_consumed_only_while_a_session_runs() {
        let state = FieldState::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state.set_stop_channel(tx);

        // Idle: the application must get its Escape.
        assert!(!state.take_stop(true));
        assert!(rx.try_recv().is_err());

        state.set_session_active(true);
        assert!(state.take_stop(true), "consumed while listening");
        assert!(rx.try_recv().is_ok(), "and the stop was delivered");

        state.set_session_active(false);
        assert!(!state.take_stop(true));
    }

    #[test]
    fn every_other_key_passes_through() {
        let state = FieldState::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state.set_stop_channel(tx);
        state.set_session_active(true);
        assert!(!state.take_stop(false));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn nothing_is_consumed_without_somewhere_to_report_it() {
        // Swallowing the key and then failing to stop would leave dictation
        // running and eat the Escape — the worst of both.
        let state = FieldState::new();
        state.set_session_active(true);
        assert!(!state.take_stop(true));
    }

    #[test]
    fn a_dropped_receiver_stops_consuming() {
        let state = FieldState::new();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        state.set_stop_channel(tx);
        state.set_session_active(true);
        drop(rx);
        assert!(!state.take_stop(true));
    }
}
