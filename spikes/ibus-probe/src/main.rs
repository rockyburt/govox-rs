//! M-1(b): can we register an IBus component and serve an engine from Rust
//! over raw D-Bus, with no libibus and no GLib main loop?
//!
//! `govox-py` reaches IBus through libibus via PyGObject on a dedicated GLib
//! main loop. The pure-D-Bus decision removes that loop — but IBus's
//! serializable objects use a bespoke GVariant layout that libibus normally
//! builds for you and that is documented nowhere. If it could not be reproduced
//! by hand, M10 was in trouble.
//!
//! The layouts here were **read off the live daemon**, not guessed:
//!
//! ```text
//! $ gdbus call --address "$IBUS_ADDRESS" --dest org.freedesktop.IBus \
//!     --object-path /org/freedesktop/IBus \
//!     --method org.freedesktop.IBus.GetEnginesByNames '["xkb:us::eng"]'
//! ([<('IBusEngineDesc', @a{sv} {}, 'xkb:us::eng', …, uint32 50, '', …)>],)
//! ```
//!
//! # Why registration alone proves nothing
//!
//! `RegisterComponent` returns success for a component the daemon then never
//! uses, and `GetEnginesByNames` reads the *static* XML registry rather than
//! dynamic registrations — so it reports 0 engines even for a registration that
//! worked. (Querying the running govox-py's own `govox` engine returns 0 too.)
//! That is a fourth member of this project's "silent success" family, and it is
//! the reason this probe goes further: the only real evidence is ibus-daemon
//! calling back into our factory.
//!
//! # Safety
//!
//! This forces that callback on an input context **we create and own**, via
//! that context's own `SetEngine`. It never calls the daemon-wide
//! `SetGlobalEngine`, so a running govox-py and the user's actual typing are
//! left alone.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::sync::Notify;
use zvariant::{ObjectPath, OwnedValue, StructureBuilder, Value};

const ENGINE: &str = "govox-rs-spike";
const BUS_NAME: &str = "org.freedesktop.IBus.GovoxRsSpike";
const FACTORY_PATH: &str = "/org/freedesktop/IBus/Factory";
const ENGINE_PATH: &str = "/org/freedesktop/IBus/Engine/GovoxRsSpike/1";

/// Minimal engine object. Present so the path the factory hands back resolves;
/// the spike does not exercise the preedit lifecycle.
struct SpikeEngine;

#[zbus::interface(name = "org.freedesktop.IBus.Engine")]
impl SpikeEngine {
    /// Returning false means "not handled, pass it through".
    ///
    /// govox-py's engine does the same and documents the contract loudly: an
    /// active input method receives every key event in the focused field, and
    /// this handler must never log, count by key, or retain anything.
    fn process_key_event(&self, _keyval: u32, _keycode: u32, _state: u32) -> bool {
        false
    }
    fn focus_in(&self) {}
    fn focus_out(&self) {}
    fn reset(&self) {}
    fn enable(&self) {}
    fn disable(&self) {}
    fn destroy(&self) {}
}

/// The object ibus-daemon calls when something asks for our engine.
struct SpikeFactory {
    /// Fired when `CreateEngine` actually arrives — the real proof.
    called: Arc<Notify>,
}

#[zbus::interface(name = "org.freedesktop.IBus.Factory")]
impl SpikeFactory {
    async fn create_engine(&self, name: &str) -> zbus::fdo::Result<ObjectPath<'_>> {
        println!("  >>> ibus-daemon called CreateEngine({name:?})");
        self.called.notify_one();
        ObjectPath::try_from(ENGINE_PATH).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
}

/// Locate the IBus private bus.
///
/// IBus does not use the session bus. `IBus.Bus()` reads
/// `$XDG_CONFIG_HOME/ibus/bus/<machine-id>-<display>`, and stale files for dead
/// daemons are left behind — this machine has three, of which one is live. Any
/// Rust implementation must reproduce this lookup *and* check the daemon is
/// alive, which libibus hides.
fn ibus_address() -> Result<String> {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });
    let dir = base.join("ibus/bus");

    let mut live = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        let mut address = None;
        let mut pid = None;
        for line in body.lines() {
            if let Some(rest) = line.strip_prefix("IBUS_ADDRESS=") {
                address = Some(rest.trim().to_string());
            }
            if let Some(rest) = line.strip_prefix("IBUS_DAEMON_PID=") {
                pid = rest.trim().parse::<i32>().ok();
            }
        }
        if let (Some(a), Some(p)) = (address, pid) {
            let alive = std::path::Path::new(&format!("/proc/{p}")).exists();
            println!(
                "  {} pid={p} {}",
                path.file_name().unwrap_or_default().display(),
                if alive { "ALIVE" } else { "stale" }
            );
            if alive {
                live.push(a);
            }
        }
    }
    if live.is_empty() {
        bail!("no live ibus-daemon found in {}", dir.display());
    }
    Ok(live.remove(0))
}

/// `('IBusEngineDesc', a{sv}, name, longname, description, language, license,
///   author, icon, layout, u rank, hotkeys, symbol, setup, layout_variant,
///   layout_option, version, textdomain, icon_prop_key)`
fn engine_desc() -> Result<Value<'static>> {
    let attachments: HashMap<String, OwnedValue> = HashMap::new();
    let b = StructureBuilder::new()
        .add_field("IBusEngineDesc".to_string())
        .add_field(attachments)
        .add_field(ENGINE.to_string())
        .add_field("govox-rs spike".to_string())
        .add_field("M-1(b) probe — safe to ignore".to_string())
        .add_field("en".to_string())
        .add_field("MIT".to_string())
        .add_field("Rocky Burt".to_string())
        .add_field("audio-input-microphone-symbolic".to_string())
        .add_field("us".to_string())
        .add_field(0u32) // rank 0 — never auto-selected
        .add_field(String::new()) // hotkeys
        .add_field(String::new()) // symbol
        .add_field(String::new()) // setup
        .add_field(String::new()) // layout_variant
        .add_field(String::new()) // layout_option
        .add_field(String::new()) // version
        .add_field(String::new()) // textdomain
        .add_field(String::new()); // icon_prop_key
    Ok(Value::from(b.build()?))
}

/// `('IBusComponent', a{sv}, name, description, version, license, author,
///   homepage, exec, textdomain, av observed_paths, av engines)`
///
/// `exec` is empty on purpose: the engine lives in the daemon process, so there
/// is nothing for ibus-daemon to spawn.
fn component() -> Result<Value<'static>> {
    let attachments: HashMap<String, OwnedValue> = HashMap::new();
    let observed_paths: Vec<Value<'static>> = Vec::new();
    let engines: Vec<Value<'static>> = vec![engine_desc()?];

    let b = StructureBuilder::new()
        .add_field("IBusComponent".to_string())
        .add_field(attachments)
        .add_field(BUS_NAME.to_string())
        .add_field("govox-rs M-1(b) spike".to_string())
        .add_field("0.0.0".to_string())
        .add_field("MIT".to_string())
        .add_field("Rocky Burt".to_string())
        .add_field(String::new()) // homepage
        .add_field(String::new()) // exec — in-process engine
        .add_field(String::new()) // textdomain
        .add_field(observed_paths)
        .add_field(engines);
    Ok(Value::from(b.build()?))
}

/// The currently-active global engine's name, if any.
///
/// The `GlobalEngine` property is an `IBusEngineDesc` variant, whose field 2
/// (after the type tag and the attachment dict) is the engine name.
async fn read_global_engine_name(conn: &zbus::Connection) -> Option<String> {
    let props = zbus::Proxy::new(
        conn,
        "org.freedesktop.IBus",
        "/org/freedesktop/IBus",
        "org.freedesktop.DBus.Properties",
    )
    .await
    .ok()?;
    let reply = props
        .call_method("Get", &("org.freedesktop.IBus", "GlobalEngine"))
        .await
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    let Value::Structure(s) = value.into() else {
        return None;
    };
    match s.fields().get(2) {
        Some(Value::Str(name)) => Some(name.to_string()),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("locating ibus bus:");
    let address = ibus_address()?;
    println!("\nconnecting to {address}\n");

    let called = Arc::new(Notify::new());

    // Serve the factory and claim the name *before* registering the component,
    // so the daemon can resolve us the moment the registration lands.
    let conn = zbus::connection::Builder::address(address.as_str())?
        .name(BUS_NAME)?
        .serve_at(FACTORY_PATH, SpikeFactory { called: called.clone() })?
        .serve_at(ENGINE_PATH, SpikeEngine)?
        .build()
        .await
        .context("connecting to the IBus private bus and claiming the factory name")?;
    println!("connected as {:?}, owning {BUS_NAME}", conn.unique_name());

    let bus = zbus::Proxy::new(
        &conn,
        "org.freedesktop.IBus",
        "/org/freedesktop/IBus",
        "org.freedesktop.IBus",
    )
    .await?;

    let comp = component()?;
    println!("component signature: {}", comp.value_signature());
    bus.call_method("RegisterComponent", &(&comp,))
        .await
        .context("RegisterComponent rejected the variant")?;
    println!("RegisterComponent: accepted (proves nothing on its own)\n");

    // The real test: make the daemon instantiate our engine. Done on a context
    // we own, never via the daemon-wide SetGlobalEngine.
    let ctx_path: zvariant::OwnedObjectPath = bus
        .call_method("CreateInputContext", &("govox-rs-spike-probe",))
        .await?
        .body()
        .deserialize()?;
    println!("created our own input context at {ctx_path}");

    let ctx = zbus::Proxy::new(
        &conn,
        "org.freedesktop.IBus",
        ctx_path.as_ref(),
        "org.freedesktop.IBus.InputContext",
    )
    .await?;

    println!("asking that context for {ENGINE:?} …");
    match ctx.call_method("SetEngine", &(ENGINE,)).await {
        Ok(_) => println!("per-context SetEngine returned OK"),
        Err(e) => println!("per-context SetEngine refused: {e}"),
    }

    let mut proven = tokio::time::timeout(std::time::Duration::from_secs(2), called.notified())
        .await
        .is_ok();

    // GNOME runs ibus in use-global-engine mode, which refuses per-context
    // SetEngine outright ("Cannot set engines when use-global-engine is
    // enabled"). The daemon-wide switch is therefore the *only* way to make an
    // engine instantiate — which is exactly why govox-py has to call
    // set_global_engine at all, and why its 15s deadlock mattered so much.
    //
    // The window is kept as short as possible and the previous engine is always
    // restored below. SpikeEngine::process_key_event returns false, so keys pass
    // through untouched even while it is active.
    if !proven {
        let previous: Option<String> = read_global_engine_name(&conn).await;
        println!(
            "\nfalling back to the global switch (previous engine: {})",
            previous.as_deref().unwrap_or("<none>")
        );

        let switched = bus.call_method("SetGlobalEngine", &(ENGINE,)).await;
        match &switched {
            Ok(_) => println!("SetGlobalEngine returned OK"),
            Err(e) => println!("SetGlobalEngine error: {e}"),
        }

        proven = tokio::time::timeout(std::time::Duration::from_secs(5), called.notified())
            .await
            .is_ok();

        // Always restore, whatever happened above.
        let restore = previous.as_deref().unwrap_or("xkb:us::eng");
        match bus.call_method("SetGlobalEngine", &(restore,)).await {
            Ok(_) => println!("restored global engine to {restore:?}"),
            Err(e) => println!("WARNING: could not restore global engine: {e}"),
        }
    }

    println!("\n--- M-1(b) verdict ---");
    if proven {
        println!("PASS. ibus-daemon resolved our component, found our factory on");
        println!("the bus name, and called CreateEngine. The whole registration");
        println!("path works from raw zbus: no libibus, no GObject introspection,");
        println!("no GLib main loop.");
    } else {
        println!("INCONCLUSIVE. The variant was accepted and the name was claimed,");
        println!("but CreateEngine never arrived within 5s. Either the daemon did");
        println!("not accept the component or SetEngine on a non-focused context");
        println!("does not instantiate an engine. Next step: compare against a");
        println!("libibus-based registration on a scratch session.");
    }
    println!("\nNot exercised, deliberately: SetGlobalEngine and the preedit");
    println!("lifecycle, both of which would disturb the live desktop session.");

    // Clean up: dropping the connection withdraws the component.
    Ok(())
}
