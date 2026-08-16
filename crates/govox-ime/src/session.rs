//! Registering govox as an input method, and driving its preedit.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use govox_core::config::ImeConfig;
use govox_core::domain::{CaretRect, PreeditSink};
use tokio::sync::mpsc;
use zbus::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zvariant::OwnedObjectPath;

use crate::ImeError;
use crate::engine::{ENGINE_INTERFACE, Engine, FieldState, preedit_args};
use crate::variant;

/// A private bus name, so a running govox never collides with another engine.
pub const BUS_NAME: &str = "org.freedesktop.IBus.Govox";

const IBUS_SERVICE: &str = "org.freedesktop.IBus";
const IBUS_PATH: &str = "/org/freedesktop/IBus";
const FACTORY_PATH: &str = "/org/freedesktop/IBus/Factory";

/// How long any single call into ibus-daemon may take.
///
/// The async engine switch measured 2.8 ms to activate and 1.2 ms to deactivate
/// in the M-1(b) spike, so this bound only matters when ibus-daemon is wedged.
/// It is on **every** call, not just the switch: `govox-py` avoids only the one
/// deadlock it knew about, and a timeout that has never fired costs nothing.
const CALL_TIMEOUT: Duration = Duration::from_secs(8);

/// What the session task is asked to do.
///
/// Every variant is fire-and-forget. Errors are logged and dropped, because an
/// IBus that goes away mid-session means dictation loses its preedit, not that
/// the daemon fails.
#[derive(Debug)]
enum Command {
    Activate,
    Deactivate,
    Preedit(String),
    Commit(String),
    Clear,
}

impl Command {
    /// The command's name, for logs. Deliberately excludes the payload: the
    /// text is the user's dictation and does not belong in the journal.
    fn name(&self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Deactivate => "deactivate",
            Self::Preedit(_) => "preedit",
            Self::Commit(_) => "commit",
            Self::Clear => "clear",
        }
    }
}

/// govox registered as an input method.
///
/// Dropping this withdraws the component: the registration is tied to the
/// connection, and closing it is all the teardown IBus needs. `govox-py` has to
/// deactivate explicitly, wait out a `GLib.timeout_add(500, quit)` grace period
/// and join a thread for two seconds — none of which has an equivalent here.
pub struct IbusSession {
    commands: mpsc::UnboundedSender<Command>,
    state: Arc<FieldState>,
    /// Held so the connection outlives the session; see the type doc.
    _connection: Connection,
}

impl IbusSession {
    /// Register the component, export the factory, and start serving.
    ///
    /// The ordering is load-bearing and is the second trap `govox-py`
    /// documents: **claim the name and export the factory before registering**.
    /// gnome-shell resolves an input source by name the moment the registration
    /// lands, and a name whose factory is not yet exported fails to activate.
    pub async fn start(config: &ImeConfig) -> Result<Self, ImeError> {
        let address = crate::address::discover()?;
        tracing::debug!(address = %address.address, source = ?address.source, "found ibus-daemon");

        let connection = zbus::connection::Builder::address(address.address.as_str())?
            .build()
            .await?;

        let state = Arc::new(FieldState::new());
        connection
            .object_server()
            .at(
                FACTORY_PATH,
                Factory {
                    state: Arc::clone(&state),
                    engine_name: config.engine_name.clone(),
                    next: AtomicU64::new(1),
                },
            )
            .await?;

        claim_name(&connection).await?;

        register_component(&connection, &config.engine_name).await?;

        let (commands, receiver) = mpsc::unbounded_channel();
        tokio::spawn(run(
            receiver,
            connection.clone(),
            Arc::clone(&state),
            config.clone(),
        ));

        tracing::info!(engine = %config.engine_name, "IBus preedit engine registered");
        Ok(Self {
            commands,
            state,
            _connection: connection,
        })
    }
}

/// Take the bus name, and refuse to continue without it.
///
/// A **fifth** silent-success trap, and one `govox-py` cannot hit because
/// libibus claims the name for it. `Connection::request_name` passes no flags,
/// so a name that is already owned puts this connection in D-Bus's *queue* and
/// returns `Ok(())` — the reply says `InQueue`, and zbus discards it. The
/// daemon would log "registered", own nothing, serve a factory ibus-daemon
/// cannot reach, and show no error anywhere.
///
/// So the flags are explicit and the reply is checked. `DoNotQueue` means a
/// taken name comes back as `Exists` rather than a queue position, and
/// `PrimaryOwner` is the only reply that means what the code below assumes.
/// `AllowReplacement` is deliberately *not* set: an input method that can be
/// displaced mid-session by anything that asks is worse than one that refuses
/// to start.
async fn claim_name(connection: &Connection) -> Result<(), ImeError> {
    let reply = connection
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .await
        .map_err(|error| ImeError::NameTaken(format!("{BUS_NAME}: {error}")))?;
    if reply == RequestNameReply::PrimaryOwner {
        return Ok(());
    }
    // A name already held is very nearly always the other govox: the Python
    // reference uses this exact name. Saying so beats "name taken".
    Err(ImeError::NameTaken(format!("{BUS_NAME}: {reply:?}")))
}

/// Announce the component to ibus-daemon.
///
/// **`RegisterComponent` returning OK proves nothing** — the fourth "silent
/// success" trap, found in the M-1(b) spike. `GetEnginesByNames` cannot confirm
/// it either: that method reads the *static* XML registry under
/// `/usr/share/ibus/component/`, and returns zero engines for a registration
/// that succeeded — confirmed against the running `govox-py`'s own live,
/// *active* engine. The only real evidence is ibus-daemon calling back into the
/// factory, which is why [`Factory::create_engine`] logs at INFO.
async fn register_component(connection: &Connection, engine_name: &str) -> Result<(), ImeError> {
    let component = variant::component(BUS_NAME, engine_name);
    call(connection, "RegisterComponent", &(component,)).await?;
    Ok(())
}

/// One method call on `org.freedesktop.IBus`, under the global timeout.
async fn call<B>(connection: &Connection, method: &str, body: &B) -> Result<(), ImeError>
where
    B: serde::Serialize + zvariant::DynamicType,
{
    let call = connection.call_method(
        Some(IBUS_SERVICE),
        IBUS_PATH,
        Some(IBUS_SERVICE),
        method,
        body,
    );
    match tokio::time::timeout(CALL_TIMEOUT, call).await {
        Ok(result) => {
            result?;
            Ok(())
        }
        Err(_) => Err(ImeError::Timeout(method.to_owned())),
    }
}

/// The session task: owns the connection and serialises every IBus call.
async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    connection: Connection,
    state: Arc<FieldState>,
    config: ImeConfig,
) {
    // Preedit is a surface the user watches. When it silently does nothing, a
    // DEBUG line nobody enabled looks the same as the feature not being wired
    // — so say it once per session, then drop to DEBUG so it cannot flood.
    let mut warned = false;
    while let Some(command) = commands.recv().await {
        let name = command.name();
        let outcome = match command {
            Command::Activate => switch(&connection, &config.engine_name).await,
            Command::Deactivate => switch(&connection, &config.baseline_engine).await,
            Command::Preedit(text) => show(&connection, &state, &text).await,
            Command::Commit(text) => commit(&connection, &state, &text).await,
            Command::Clear => clear(&connection, &state).await,
        };
        if let Err(error) = outcome {
            if warned {
                tracing::debug!(%error, command = name, "IBus call failed");
            } else {
                warned = true;
                tracing::warn!(
                    %error,
                    command = name,
                    "IBus call failed; provisional text will not appear (further failures at DEBUG)"
                );
            }
        }
    }
    // The channel closed, so the session was dropped. Leave the keyboard as it
    // was found rather than stranding the user on the dictation engine.
    if let Err(error) = switch(&connection, &config.baseline_engine).await {
        tracing::debug!(%error, "could not restore the baseline engine");
    }
}

/// Make `engine_name` the global engine.
///
/// `SetGlobalEngine` is the **only** door on GNOME: activating the engine on an
/// input context we create ourselves — the route that touches nothing the user
/// is typing into — is refused outright with "Cannot set engines when
/// use-global-engine is enabled". That is not a choice `govox-py` made.
///
/// It is also why the synchronous variant's deadlock mattered so much: with no
/// alternative path, a 15-second hang was the whole feature failing. Here the
/// call is async by construction, so the reentrancy that caused it cannot
/// happen — ibus-daemon's callback into our factory is served by another zbus
/// task while this one awaits.
async fn switch(connection: &Connection, engine_name: &str) -> Result<(), ImeError> {
    call(connection, "SetGlobalEngine", &(engine_name,)).await
}

/// Replace the whole preedit. Whole-string replace is why nothing diffs.
async fn show(connection: &Connection, state: &FieldState, text: &str) -> Result<(), ImeError> {
    emit(
        connection,
        state,
        "UpdatePreeditText",
        &preedit_args(text, true),
    )
    .await
}

/// Discard the preedit without committing it.
async fn clear(connection: &Connection, state: &FieldState) -> Result<(), ImeError> {
    emit(
        connection,
        state,
        "UpdatePreeditText",
        &preedit_args("", false),
    )
    .await?;
    // Belt and braces: an empty invisible preedit should already be nothing,
    // but HidePreeditText is what tells the client to drop any it is still
    // rendering.
    emit(connection, state, "HidePreeditText", &()).await
}

/// Clear, then commit.
///
/// Clearing first is not tidiness: committing with a preedit still showing
/// leaves the provisional copy behind in some toolkits, i.e. the text twice.
async fn commit(connection: &Connection, state: &FieldState, text: &str) -> Result<(), ImeError> {
    clear(connection, state).await?;
    emit(connection, state, "CommitText", &(variant::text(text),)).await
}

/// Emit a signal from the engine object that currently has focus.
async fn emit<B>(
    connection: &Connection,
    state: &FieldState,
    signal: &str,
    body: &B,
) -> Result<(), ImeError>
where
    B: serde::Serialize + zvariant::DynamicType,
{
    let Some(path) = state.active() else {
        // Ordinary when no text field has focus yet — the application never
        // asked IBus for an engine. Ordinary once; if it is still true while
        // govox is drawing provisional text it is the whole reason nothing
        // appears under the caret, so say it rather than leave it at DEBUG.
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if signal == "UpdatePreeditText" && !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            tracing::warn!(
                signal,
                "no IBus engine has focus, so provisional text has nowhere to \
                 go; the focused application is probably not an IBus client"
            );
        } else {
            tracing::debug!(signal, "no live IBus engine; dropping the update");
        }
        return Ok(());
    };
    let emit = connection.emit_signal(None::<&str>, &path, ENGINE_INTERFACE, signal, body);
    match tokio::time::timeout(CALL_TIMEOUT, emit).await {
        Ok(result) => {
            result?;
            Ok(())
        }
        Err(_) => Err(ImeError::Timeout(signal.to_owned())),
    }
}

impl PreeditSink for IbusSession {
    fn activate(&self) {
        self.send(Command::Activate);
    }

    fn deactivate(&self) {
        self.send(Command::Deactivate);
    }

    fn preedit(&self, text: &str) {
        if text.is_empty() {
            self.clear();
            return;
        }
        self.send(Command::Preedit(text.to_owned()));
    }

    fn commit(&self, text: &str) {
        if text.is_empty() {
            self.clear();
            return;
        }
        self.send(Command::Commit(text.to_owned()));
    }

    fn clear(&self) {
        self.send(Command::Clear);
    }

    fn field_purpose(&self) -> Option<String> {
        self.state.purpose()
    }

    fn surrounding_text(&self) -> Option<String> {
        self.state.surrounding_before()
    }

    fn cursor_location(&self) -> Option<CaretRect> {
        self.state.caret()
    }
}

impl IbusSession {
    fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            tracing::debug!("the IBus session task has stopped; dropping the update");
        }
    }
}

/// The object ibus-daemon calls to get an engine.
struct Factory {
    state: Arc<FieldState>,
    engine_name: String,
    next: AtomicU64,
}

#[zbus::interface(name = "org.freedesktop.IBus.Factory")]
impl Factory {
    /// ibus-daemon asking for an engine instance.
    ///
    /// This callback is the only proof that registration worked, so it logs at
    /// INFO — see [`register_component`].
    async fn create_engine(
        &self,
        name: &str,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        if name != self.engine_name {
            return Err(zbus::fdo::Error::Failed(format!("unknown engine {name:?}")));
        }
        let serial = self.next.fetch_add(1, Ordering::Relaxed);
        let path = OwnedObjectPath::try_from(engine_path(serial))
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;

        server
            .at(
                &path,
                Engine {
                    state: Arc::clone(&self.state),
                    path: path.clone(),
                },
            )
            .await?;
        tracing::info!(engine = name, path = %path.as_str(), "ibus-daemon created a govox engine");
        Ok(path)
    }
}

/// The object path the factory hands out for the `serial`-th input context.
fn engine_path(serial: u64) -> String {
    format!("/org/freedesktop/IBus/Engine/govox/{serial}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use zvariant::{ObjectPath, Value};

    #[test]
    fn engine_paths_are_unique_per_input_context() {
        // IBus creates one engine per input context. Two contexts sharing a
        // path would make focus tracking meaningless: the daemon would drive
        // one object for two fields.
        assert_ne!(engine_path(1), engine_path(2));
        let path = engine_path(1);
        assert!(ObjectPath::try_from(path.as_str()).is_ok(), "{path}");
    }

    #[test]
    fn the_timeout_covers_the_measured_switch_with_room_to_spare() {
        // 2.8 ms measured; this bound is for a wedged daemon, not a slow one.
        assert!(CALL_TIMEOUT >= Duration::from_secs(1));
    }

    #[test]
    fn the_component_registers_under_the_private_bus_name() {
        // The name is what ibus-daemon looks up to find our factory, so it has
        // to be the one we actually claimed.
        let Value::Structure(component) = variant::component(BUS_NAME, "govox") else {
            panic!("IBusComponent is a structure");
        };
        assert_eq!(<&str>::try_from(&component.fields()[2]).unwrap(), BUS_NAME);
    }
}
