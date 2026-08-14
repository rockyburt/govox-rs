//! Watching keyboards for the activation shortcut.
//!
//! Keyboards are **observed, never grabbed**. Every watched key still reaches
//! the focused application, which is why the controller watches exactly one key
//! — the one its mode uses — and why this module opens only the devices that
//! can emit it. Grabbing would make the activation key stop working everywhere
//! else on the system.

use std::path::{Path, PathBuf};

use evdev::{Device, EventSummary, KeyCode};
use govox_core::activation::KeyEvent;

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("input device access denied: {path}; is the user in the 'input' group?")]
    AccessDenied { path: PathBuf },
    #[error("input device unavailable: {path}: {source}")]
    Unavailable {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Translate a canonical evdev name (`KEY_RIGHTCTRL`) to its code.
///
/// The names come from user config, so an unknown one is an ordinary `None`
/// and not a panic.
#[must_use]
pub fn key_code(name: &str) -> Option<KeyCode> {
    name.parse().ok()
}

/// Translate a code back to its canonical name.
///
/// `None` for a code outside evdev's table, matching `govox-py`, whose
/// `_resolve_key_name` returns `None` when `ecodes.KEY` has no entry.
///
/// The check is on the rendered string because `KeyCode`'s `Debug` is the only
/// public route to a name, and it renders unmapped codes as `unknown key: 314`
/// rather than failing. Letting that through would put a sentence where a key
/// name belongs, and it would compare unequal to every configured key — a
/// shortcut that silently never fires.
#[must_use]
pub fn key_name(code: KeyCode) -> Option<String> {
    let name = format!("{code:?}");
    name.starts_with("KEY_").then_some(name)
}

/// Paths of devices that can emit at least one of `keys`.
///
/// Filtering by the configured shortcut keys — rather than any device exposing
/// `EV_KEY` — keeps govox from opening mice, power/sleep buttons, jack-detect
/// switches and other non-keyboards that also report button events.
#[must_use]
pub fn find_keyboard_devices(keys: &[String]) -> Vec<PathBuf> {
    let wanted: Vec<KeyCode> = keys.iter().filter_map(|k| key_code(k)).collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    devices_matching(|device| {
        device
            .supported_keys()
            .is_some_and(|supported| wanted.iter().any(|code| supported.contains(*code)))
    })
}

/// Paths of devices that look like real keyboards.
///
/// Used by `govox keys` to capture from every keyboard so the user can press a
/// candidate activation key and read back its canonical `KEY_*` name. Requiring
/// a ubiquitous keyboard key (Esc) excludes mice, power buttons and jack-detect
/// switches, which also report `EV_KEY` events.
#[must_use]
pub fn find_key_devices() -> Vec<PathBuf> {
    devices_matching(|device| {
        device
            .supported_keys()
            .is_some_and(|keys| keys.contains(KeyCode::KEY_ESC))
    })
}

fn devices_matching<F>(predicate: F) -> Vec<PathBuf>
where
    F: Fn(&Device) -> bool,
{
    // A device we cannot open is skipped, not an error: enumeration runs on
    // every hotplug rescan, and one permission-denied node must not blind
    // govox to the keyboard next to it.
    evdev::enumerate()
        .filter(|(_, device)| predicate(device))
        .map(|(path, _)| path)
        .collect()
}

/// Open one device for reading.
///
/// # Errors
/// If the node is missing or the user lacks permission.
pub fn open_device(path: &Path) -> Result<Device, InputError> {
    Device::open(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::PermissionDenied {
            InputError::AccessDenied {
                path: path.to_owned(),
            }
        } else {
            InputError::Unavailable {
                path: path.to_owned(),
                source,
            }
        }
    })
}

/// Translate one evdev event into a [`KeyEvent`], or `None` to ignore it.
///
/// Autorepeat (value 2) is dropped here. That is load-bearing for double-tap:
/// a held key would otherwise emit a stream of key-downs and masquerade as a
/// double-tap the user never performed.
#[must_use]
pub fn to_key_event(event: &evdev::InputEvent) -> Option<KeyEvent> {
    let EventSummary::Key(_, code, value) = event.destructure() else {
        return None;
    };
    let name = key_name(code)?;
    match value {
        1 => Some(KeyEvent::Down(name)),
        0 => Some(KeyEvent::Up(name)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_names_resolve_to_codes() {
        assert_eq!(key_code("KEY_RIGHTCTRL"), Some(KeyCode::KEY_RIGHTCTRL));
        assert_eq!(key_code("KEY_ESC"), Some(KeyCode::KEY_ESC));
        assert_eq!(key_code("KEY_F12"), Some(KeyCode::KEY_F12));
    }

    #[test]
    fn an_unknown_name_is_none_rather_than_a_panic() {
        // These come from user config; a typo must not take the daemon down.
        assert_eq!(key_code("KEY_NOPE"), None);
        assert_eq!(key_code(""), None);
        assert_eq!(key_code("rightctrl"), None, "names are case-sensitive");
    }

    #[test]
    fn no_configured_keys_means_no_devices_opened() {
        // Not "open everything": a config naming only unknown keys must leave
        // govox watching nothing rather than every input node on the system.
        assert!(find_keyboard_devices(&[]).is_empty());
        assert!(find_keyboard_devices(&["KEY_NOPE".to_owned()]).is_empty());
    }

    #[test]
    fn key_events_carry_the_canonical_name() {
        let down =
            evdev::InputEvent::new(evdev::EventType::KEY.0, KeyCode::KEY_RIGHTCTRL.code(), 1);
        assert_eq!(
            to_key_event(&down),
            Some(KeyEvent::Down("KEY_RIGHTCTRL".to_owned()))
        );

        let up = evdev::InputEvent::new(evdev::EventType::KEY.0, KeyCode::KEY_RIGHTCTRL.code(), 0);
        assert_eq!(
            to_key_event(&up),
            Some(KeyEvent::Up("KEY_RIGHTCTRL".to_owned()))
        );
    }

    #[test]
    fn autorepeat_is_dropped() {
        // Value 2 is a repeat. Letting it through would let a held key look
        // like a double-tap, starting a dictation session the user never asked
        // for — with a modifier still physically down.
        let repeat =
            evdev::InputEvent::new(evdev::EventType::KEY.0, KeyCode::KEY_RIGHTCTRL.code(), 2);
        assert_eq!(to_key_event(&repeat), None);
    }

    #[test]
    fn an_unmapped_code_has_no_name() {
        // evdev renders these as "unknown key: 764". Putting that where a key
        // name belongs would compare unequal to every configured key, so the
        // shortcut would silently never fire.
        let unmapped = KeyCode::new(764);
        assert!(format!("{unmapped:?}").starts_with("unknown key"));
        assert_eq!(key_name(unmapped), None);

        let event = evdev::InputEvent::new(evdev::EventType::KEY.0, 764, 1);
        assert_eq!(to_key_event(&event), None);
    }

    #[test]
    fn names_round_trip_through_codes() {
        for name in ["KEY_RIGHTCTRL", "KEY_F12", "KEY_ESC", "KEY_A"] {
            let code = key_code(name).expect("in the table");
            assert_eq!(key_name(code).as_deref(), Some(name));
        }
    }

    /// Requires membership of the `input` group; run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs /dev/input access"]
    fn real_keyboards_are_found_and_openable() {
        let keyboards = find_key_devices();
        assert!(
            !keyboards.is_empty(),
            "no keyboards found; is the user in the 'input' group?"
        );
        for path in &keyboards {
            open_device(path).expect("an enumerated keyboard opens");
        }

        // The default toggle key must resolve to at least one of them, or the
        // shipped config activates nothing on this machine.
        let toggles = find_keyboard_devices(&["KEY_RIGHTCTRL".to_owned()]);
        assert!(
            !toggles.is_empty(),
            "no device can emit the default toggle key"
        );
    }

    #[test]
    fn non_key_events_are_ignored() {
        // Mice and touchpads emit these constantly on shared devices.
        let motion = evdev::InputEvent::new(evdev::EventType::RELATIVE.0, 0, 5);
        assert_eq!(to_key_event(&motion), None);
    }
}
