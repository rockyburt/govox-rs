//! The part of M10 that only a real desktop can answer.
//!
//! Everything in the unit tests is about the *shape* of what govox sends. None
//! of it can tell you whether ibus-daemon accepts it, because the fourth
//! "silent success" trap is precisely that acceptance is unobservable:
//! `RegisterComponent` returns OK regardless, and `GetEnginesByNames` reads a
//! static XML registry that returns zero engines for a registration that
//! worked — confirmed in the M-1(b) spike against `govox-py`'s own *active*
//! engine.
//!
//! **The only evidence is ibus-daemon calling back into the factory.** That is
//! what this test waits for.
//!
//! ```console
//! $ cargo test -p govox-ime -- --ignored --nocapture
//! ```
//!
//! Ignored by default, like every other test here that needs hardware, a model
//! or a desktop session — the same line `govox-py` draws with
//! `@pytest.mark.integration`.
//!
//! Two things to know before running it:
//!
//! * It uses the engine name `govox-rs`, not `govox`, so it cannot disturb a
//!   running `govox-py`. If `govox-py` is running it will hold the bus name and
//!   this test reports that instead — which is itself the coexistence guard
//!   working.
//! * `SetGlobalEngine` is the only door on GNOME (per-context activation is
//!   refused outright), so this **does** change the desktop's active input
//!   method for a moment. The previous engine is restored before it returns.

use std::time::Duration;

use govox_core::config::ImeConfig;
use govox_core::domain::PreeditSink;
use govox_ime::IbusSession;

/// Serialises the two tests.
///
/// They both claim the same bus name, and cargo runs a binary's tests
/// concurrently — so without this the second finds the name held by the *first*
/// and takes its "something else owns it" escape, which makes the refusal it
/// exists to prove go permanently untested. Exactly the kind of green that
/// means nothing.
/// Async-aware, because it is held across the `await`s that do the work.
static BUS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn config() -> ImeConfig {
    ImeConfig {
        enabled: true,
        // Never "govox": that is the running reference implementation's name.
        engine_name: "govox-rs".to_owned(),
        baseline_engine: "xkb:us::eng".to_owned(),
    }
}

/// Registration is accepted and the daemon instantiates our engine.
///
/// The assertion at the end is the whole test. Registration succeeding proves
/// nothing — that is the fourth trap — so what is checked is that a *client*
/// reported something to the engine object ibus-daemon built through our
/// factory. Only a real binding produces that.
///
/// A failure here means the desktop resolved the component and then nothing
/// bound to it, which is the interesting case; it is not a flaky test.
#[tokio::test]
#[ignore = "needs a live ibus-daemon and changes the active input method"]
async fn ibus_creates_an_engine_through_our_factory() {
    let _ = tracing_subscriber::fmt::try_init();
    let _guard = BUS.lock().await;

    let session = IbusSession::start(&config())
        .await
        .expect("registration should be accepted");

    session.activate();

    // The switch measured 2.8 ms in the spike; a second is generous. Polling
    // rather than sleeping the whole second so a working desktop is quick.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while session.field_purpose().is_none() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Show something, then take it back without committing. Nothing may end up
    // in any document: that is the whole promise of provisional text.
    session.preedit("this text must never be committed");
    tokio::time::sleep(Duration::from_millis(200)).await;
    session.clear();

    let purpose = session.field_purpose();
    let caret = session.cursor_location();
    session.deactivate();
    // Dropping withdraws the component, but give the queued restore a moment to
    // reach the daemon first — otherwise the desktop is left on govox-rs.
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(session);

    assert!(
        purpose.is_some() || caret.is_some(),
        "the engine was created but no client ever reported to it; \
         registration returning OK is not evidence that anything bound"
    );
    // Observed on the reference machine: ptyxis reports TERMINAL and a caret
    // rectangle, which is what makes the content type usable for standing the
    // prose rules down outside ordinary writing.
    eprintln!("client reported purpose={purpose:?} caret={caret:?}");
}

/// A second session cannot claim the name a first one holds.
///
/// This is the guard against both daemons running at once during the parity
/// period: `govox-py` claims `org.freedesktop.IBus.Govox` too, so two processes
/// registering the same engine is impossible rather than merely discouraged.
#[tokio::test]
#[ignore = "needs a live ibus-daemon"]
async fn two_govox_processes_cannot_both_register() {
    let _guard = BUS.lock().await;

    let first = IbusSession::start(&config()).await;
    let Ok(first) = first else {
        // Something outside this process already owns the name — govox-py, in
        // practice. That is the situation this test describes rather than a
        // failure of it, so say so loudly: a silent skip here would read as a
        // pass for the guard it never got to exercise.
        eprintln!("SKIPPED: the bus name is already held (is govox-py running?)");
        return;
    };

    let second = IbusSession::start(&config()).await;
    assert!(
        matches!(second, Err(govox_ime::ImeError::NameTaken(_))),
        "the second registration must be refused, not silently shadow the first"
    );
    drop(first);
}
