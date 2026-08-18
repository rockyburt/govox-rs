//! Enumeration notices a keyboard that appears, and one that goes away.
//!
//! `#[ignore]`d: it creates a real device through `/dev/uinput`, which needs
//! membership of the `input` group. Run it deliberately:
//!
//! ```text
//! cargo test -p govox-input --test hotplug -- --ignored --nocapture
//! ```
//!
//! This is the half of keyboard hot-plug that can be tested without a daemon:
//! that [`find_keyboard_devices`] reflects the current state of the world
//! rather than the state at startup. The supervisor that acts on it lives in
//! `govox-daemon`, and its bookkeeping is unit-tested there.

use evdev::AttributeSet;
use evdev::KeyCode;
use evdev::uinput::VirtualDevice;
use govox_input::evdev_listener::find_keyboard_devices;

/// The activation key this pretends to carry. Left Control, because that is
/// the default and the one a keyboard without a right Control still has.
const KEY: &str = "KEY_LEFTCTRL";

#[test]
#[ignore = "creates a device through /dev/uinput; needs the 'input' group"]
fn a_keyboard_that_appears_is_enumerated_and_one_that_goes_is_not() {
    let before = find_keyboard_devices(&[KEY.to_owned()]);

    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::KEY_LEFTCTRL);
    let device = VirtualDevice::builder()
        .expect("/dev/uinput is writable — are you in the 'input' group?")
        .name("govox hotplug test keyboard")
        .with_keys(&keys)
        .expect("a virtual keyboard is describable")
        .build()
        .expect("the virtual keyboard is created");

    // udev creates the node asynchronously, so this is not instantaneous.
    let appeared = wait_until(|| find_keyboard_devices(&[KEY.to_owned()]).len() > before.len());
    let during = find_keyboard_devices(&[KEY.to_owned()]);
    assert!(
        appeared,
        "a new keyboard was not enumerated: {before:?} -> {during:?}"
    );

    drop(device);

    let went = wait_until(|| find_keyboard_devices(&[KEY.to_owned()]).len() == before.len());
    assert!(
        went,
        "an unplugged keyboard is still enumerated: {:?}",
        find_keyboard_devices(&[KEY.to_owned()])
    );
}

/// Poll `check` for up to two seconds.
fn wait_until(check: impl Fn() -> bool) -> bool {
    for _ in 0..40 {
        if check() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}
