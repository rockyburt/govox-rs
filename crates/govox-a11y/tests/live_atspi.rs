//! What only a real desktop can answer about AT-SPI.
//!
//! Coverage is a property of the focused *element*, not of the desktop, so
//! there is no assertion here that holds on every machine — which is exactly
//! why these are diagnostics rather than pass/fail tests. What they *can*
//! establish is the thing `govox-py` learned the hard way: whether the field
//! the user is typing into is reachable at all, and from which window.
//!
//! ```console
//! $ cargo test -p govox-a11y -- --ignored --nocapture
//! ```
//!
//! Click into the window you care about first, then run it — the reader
//! deliberately answers only for the toplevel carrying ACTIVE, so whatever has
//! focus when this runs is what it reports on. If that is the terminal you
//! launched it from, that is the honest answer and not a bug.

use std::time::Instant;

use govox_a11y::FieldReader;

/// Which window would answer a read, and what it says.
///
/// Reports the window by name because a probe that says "6888 characters"
/// without naming the window is impossible to act on: the natural reading is
/// "the app I clicked into", and the true answer may be the terminal.
#[tokio::test]
#[ignore = "needs an accessibility bus and a focused window"]
async fn reports_what_the_focused_field_exposes() {
    let reader = FieldReader::connect()
        .await
        .expect("the accessibility bus should answer");

    match reader.active_window().await {
        Some(window) => println!("active window: {window}"),
        None => println!("active window: none — no toplevel carries ACTIVE"),
    }

    let started = Instant::now();
    let snapshot = reader.read(None).await;
    let elapsed = started.elapsed();

    match snapshot {
        Some(field) => println!(
            "read {} characters, caret at {}, in {:.0} ms",
            field.text.chars().count(),
            field.caret,
            elapsed.as_secs_f64() * 1000.0
        ),
        // Not a failure. Every terminal and every Electron app answers this
        // way, and the design contract is that commands degrade to keystrokes.
        None => println!(
            "nothing readable, in {:.0} ms",
            elapsed.as_secs_f64() * 1000.0
        ),
    }
}

/// The walk stays inside its budget even when nothing is found.
///
/// This is the one property that must hold everywhere: a read is on the path
/// of a spoken command, so an unbounded search is a visibly stalled daemon.
/// The 400-node cap it replaced failed the *other* way — fast and wrong.
#[tokio::test]
#[ignore = "needs an accessibility bus"]
async fn a_read_finishes_within_its_budget() {
    let reader = FieldReader::connect()
        .await
        .expect("the accessibility bus should answer");

    let started = Instant::now();
    let _ = reader.read(None).await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "a read took {elapsed:?}, which a user would feel as a stalled command"
    );
}
