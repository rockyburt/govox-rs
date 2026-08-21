//! The panel indicator, over StatusNotifierItem.
//!
//! `ksni` speaks the SNI D-Bus protocol directly, which deletes GTK3,
//! AyatanaAppIndicator3, a GLib main loop and one of `govox-py`'s three
//! `sys.path` bridging hacks. Icons stay freedesktop symbolic names, so govox
//! still ships no image assets.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use govox_core::feedback::{
    PULSE_FRAMES, PULSE_INTERVAL_MS, mode_presentation, state_presentation,
};
use ksni::TrayMethods as _;
use tokio::sync::mpsc;

/// What the tray asks the daemon to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Reload,
    Quit,
}

/// What the About submenu shows.
///
/// `rows` is deliberately opaque label/value pairs rather than named fields:
/// the tray's job is to render, and which facts are worth showing is the
/// daemon's to decide. Adding one is then a line in `pipeline.rs` and no change
/// here at all.
///
/// The facts are *live* rather than static on purpose. A version string is the
/// least useful thing this could show for a daemon whose failure modes are all
/// "which backend actually got picked?" — the wrong GPU, a CPU build that was
/// meant to be a GPU one, an IBus engine that never registered. Every value is
/// already computed; before this it was only reachable by reading the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AboutFacts {
    pub version: String,
    pub licence: String,
    pub rows: Vec<(String, String)>,
}

/// The SNI item itself. `ksni` calls into this from its own task.
struct GovoxTray {
    state: Arc<TrayState>,
    commands: mpsc::UnboundedSender<TrayCommand>,
}

/// Shared so the daemon can update presentation without going through `ksni`'s
/// handle type in every call site.
#[derive(Default)]
struct TrayState {
    /// Index into a two-element table, so this is the whole presentation.
    state: std::sync::Mutex<String>,
    /// Which pulse frame is showing; `usize::MAX` means "not pulsing".
    pulse_frame: AtomicUsize,
    /// The sustained mode, if govox is in one.
    ///
    /// Separate from `state` because they answer different questions and one
    /// must not overwrite the other: `state` is what is happening this second,
    /// this is what will keep happening until told otherwise.
    mode: std::sync::Mutex<Option<String>>,
    /// Filled in after construction: several of these facts are not known yet
    /// when the tray registers. The injector is chosen and the accessibility
    /// bus is dialled well after the icon has to appear, and delaying the icon
    /// until they are ready would trade a visible tray for a tidier call.
    about: std::sync::Mutex<AboutFacts>,
}

const NOT_PULSING: usize = usize::MAX;

impl GovoxTray {
    fn mode(&self) -> Option<String> {
        self.state.mode.lock().expect("tray mode poisoned").clone()
    }

    /// The one line that says what govox will do with what you say next.
    ///
    /// A mode wins, because it is the answer that lasts: "Listening" while in
    /// command mode is true and useless.
    fn status_line(&self) -> String {
        if let Some(mode) = self.mode() {
            return mode_presentation(&mode).0.to_owned();
        }
        let state = self.state.state.lock().expect("tray state poisoned");
        state_presentation(&state).0.to_owned()
    }
}

impl ksni::Tray for GovoxTray {
    fn id(&self) -> String {
        "govox".into()
    }

    fn title(&self) -> String {
        "govox".into()
    }

    fn icon_name(&self) -> String {
        // A mode outranks both the pulse and the state. While one is on, what
        // the panel must answer is "what will happen when I speak", not "is
        // audio arriving" — and in command mode the answer is "not typing".
        if let Some(mode) = self.mode() {
            return mode_presentation(&mode).1.to_owned();
        }
        let frame = self.state.pulse_frame.load(Ordering::Relaxed);
        if frame != NOT_PULSING {
            return PULSE_FRAMES[frame % PULSE_FRAMES.len()].to_owned();
        }
        let state = self.state.state.lock().expect("tray state poisoned");
        state_presentation(&state).1.to_owned()
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let description = self.status_line();
        ksni::ToolTip {
            title: "govox".into(),
            description,
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem, SubMenu};
        let label = self.status_line();

        vec![
            // Not activatable: it is a status line, not an action.
            StandardItem {
                label,
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Reload configuration".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(TrayCommand::Reload);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            MenuItem::SubMenu(SubMenu {
                label: "About".into(),
                // The submenu itself stays enabled while everything inside it
                // is not: a disabled parent does not open in most panels, so
                // disabling it would hide the contents rather than grey them.
                submenu: about_items(&self.state.about.lock().expect("about poisoned")),
                ..Default::default()
            }),
        ]
    }
}

/// The About submenu's contents, as inert rows.
///
/// Nothing here is activatable. It is a readout, and a menu entry that looks
/// clickable but does nothing is worse than one that plainly does not.
fn about_items(facts: &AboutFacts) -> Vec<ksni::MenuItem<GovoxTray>> {
    use ksni::menu::{MenuItem, StandardItem};

    let inert = |label: String| {
        MenuItem::Standard(StandardItem {
            label,
            enabled: false,
            ..Default::default()
        })
    };

    // Before `set_about` has run — a window of a second or two at startup —
    // say so rather than showing a convincing but empty table.
    if facts.version.is_empty() && facts.rows.is_empty() {
        return vec![inert("Starting up…".to_owned())];
    }

    let mut items = vec![inert(format!("govox {}", facts.version))];
    if !facts.licence.is_empty() {
        items.push(inert(format!("{} licence", facts.licence)));
    }
    if !facts.rows.is_empty() {
        items.push(MenuItem::Separator);
        items.extend(
            facts
                .rows
                .iter()
                .map(|(label, value)| inert(format!("{label}: {value}"))),
        );
    }
    items
}

/// The bus name a StatusNotifierItem has to register with to be shown.
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// The registered item, once a watcher has accepted it.
///
/// Late-bound because the watcher is not guaranteed to exist when govox
/// starts: GNOME provides it from an extension, so a daemon bound to the
/// graphical session routinely wins the race, and a shell restart takes the
/// name away and brings it back. `None` means "not registered yet", which is
/// a state the tray recovers from on its own — see [`Tray::start`].
type LateHandle = Arc<tokio::sync::RwLock<Option<ksni::Handle<GovoxTray>>>>;

/// A running tray icon. Dropping it removes the item from the panel.
pub struct Tray {
    handle: LateHandle,
    state: Arc<TrayState>,
    pulse: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Tray {
    /// Register the item and return the tray, plus the command stream.
    ///
    /// Registration is **not** required to succeed here. If no
    /// StatusNotifierWatcher is running yet, a background task waits for the
    /// name to appear and registers then, so a govox that started before the
    /// GNOME extension — or outlived a shell restart — still gets its icon
    /// instead of going without one until the user notices and restarts it.
    ///
    /// # Errors
    /// If the session bus itself cannot be reached, which is not a missing
    /// tray but a broken session.
    pub async fn start() -> Result<(Self, mpsc::UnboundedReceiver<TrayCommand>), String> {
        let (commands, receiver) = mpsc::unbounded_channel();
        let state = Arc::new(TrayState {
            state: std::sync::Mutex::new("idle".to_owned()),
            mode: std::sync::Mutex::new(None),
            pulse_frame: AtomicUsize::new(NOT_PULSING),
            about: std::sync::Mutex::new(AboutFacts::default()),
        });

        let handle: LateHandle = Arc::new(tokio::sync::RwLock::new(None));
        let tray = GovoxTray {
            state: Arc::clone(&state),
            commands: commands.clone(),
        };
        match tray.spawn().await {
            Ok(live) => *handle.write().await = Some(live),
            Err(error) => {
                tracing::info!(
                    %error,
                    "no tray yet; waiting for {WATCHER} and registering when it appears"
                );
                tokio::spawn(register_when_watched(
                    Arc::clone(&handle),
                    Arc::clone(&state),
                    commands,
                ));
            }
        }

        Ok((
            Self {
                handle,
                state,
                pulse: std::sync::Mutex::new(None),
            },
            receiver,
        ))
    }

    pub fn set_state(&self, state: &str) {
        *self.state.state.lock().expect("tray state poisoned") = state.to_owned();
        self.refresh();
    }

    /// Enter or leave a sustained mode, or `None` for plain dictation.
    pub fn set_mode(&self, mode: Option<&str>) {
        *self.state.mode.lock().expect("tray mode poisoned") = mode.map(str::to_owned);
        self.refresh();
    }

    /// Publish the facts the About submenu reads.
    ///
    /// Separate from construction because the tray icon has to appear before
    /// the injector is chosen or the accessibility bus has answered. Calling it
    /// again replaces the lot, so a reload that changes them stays truthful.
    pub fn set_about(&self, facts: AboutFacts) {
        *self.state.about.lock().expect("about poisoned") = facts;
        self.refresh();
    }

    /// Begin blinking the panel icon. Idempotent.
    ///
    /// A static record dot is easy to miss in a busy panel; the blink is what
    /// makes "govox is listening right now" readable at a glance.
    pub fn start_pulse(&self) {
        let mut pulse = self.pulse.lock().expect("pulse poisoned");
        if pulse.is_some() {
            return;
        }
        self.state.pulse_frame.store(0, Ordering::Relaxed);

        let state = Arc::clone(&self.state);
        let handle = self.handle.clone();
        *pulse = Some(tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_millis(PULSE_INTERVAL_MS));
            loop {
                ticker.tick().await;
                state.pulse_frame.fetch_add(1, Ordering::Relaxed);
                let live = handle.read().await.clone();
                // Not registered yet is not the same as gone: keep the frame
                // counter moving so the icon is already mid-pulse if the
                // watcher turns up during this session.
                let Some(live) = live else { continue };
                // `update` returns None once the item has gone; stop pulsing
                // rather than spinning against a tray that no longer exists.
                if live.update(|_| {}).await.is_none() {
                    return;
                }
            }
        }));
    }

    /// Cancel the pulse and restore the steady icon.
    pub fn stop_pulse(&self) {
        if let Some(task) = self.pulse.lock().expect("pulse poisoned").take() {
            task.abort();
        }
        self.state.pulse_frame.store(NOT_PULSING, Ordering::Relaxed);
        self.refresh();
    }

    fn refresh(&self) {
        let handle = self.handle.clone();
        // Fire-and-forget: a panel redraw must never block the daemon, and a
        // tray that has gone away is not an error worth reporting per update.
        tokio::spawn(async move {
            let live = handle.read().await.clone();
            if let Some(live) = live {
                let _ = live.update(|_| {}).await;
            }
        });
    }

    pub fn shutdown(&self) {
        self.stop_pulse();
        let handle = self.handle.clone();
        tokio::spawn(async move {
            let live = handle.read().await.clone();
            if let Some(live) = live {
                live.shutdown().await;
            }
        });
    }
}

/// Wait for a StatusNotifierWatcher to appear, then register the item.
///
/// Registering into a watcher that is not there yet is the difference between
/// "govox has no tray icon today" and "govox has a tray icon a moment later".
/// The loop ends on the first success: ksni owns re-registration from then on.
async fn register_when_watched(
    handle: LateHandle,
    state: Arc<TrayState>,
    commands: mpsc::UnboundedSender<TrayCommand>,
) {
    let connection = match zbus::Connection::session().await {
        Ok(connection) => connection,
        Err(error) => {
            tracing::debug!(%error, "no session bus; giving up on the tray");
            return;
        }
    };
    let dbus = match zbus::fdo::DBusProxy::new(&connection).await {
        Ok(dbus) => dbus,
        Err(error) => {
            tracing::debug!(%error, "cannot watch bus names; giving up on the tray");
            return;
        }
    };
    let mut changes = match dbus.receive_name_owner_changed().await {
        Ok(changes) => changes,
        Err(error) => {
            tracing::debug!(%error, "cannot watch bus names; giving up on the tray");
            return;
        }
    };

    use futures_util::StreamExt as _;
    while let Some(signal) = changes.next().await {
        let Ok(args) = signal.args() else { continue };
        if args.name() != WATCHER || args.new_owner().is_none() {
            continue;
        }
        let tray = GovoxTray {
            state: Arc::clone(&state),
            commands: commands.clone(),
        };
        match tray.spawn().await {
            Ok(live) => {
                *handle.write().await = Some(live);
                tracing::info!("tray icon registered; {WATCHER} came back");
                return;
            }
            // The name is owned but the watcher is not serving yet. Stay on
            // the stream: the next appearance is another chance.
            Err(error) => tracing::debug!(%error, "tray registration still refused"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pulse_index_wraps_across_the_frame_table() {
        // fetch_add climbs without bound; the modulo is what keeps it valid.
        for raw in [0_usize, 1, 2, 3, 1000, 1001] {
            let frame = PULSE_FRAMES[raw % PULSE_FRAMES.len()];
            assert!(frame.ends_with("-symbolic"));
        }
    }

    #[test]
    fn the_not_pulsing_sentinel_cannot_collide_with_a_real_frame() {
        // NOT_PULSING is compared before the modulo, so it must never be a
        // value the counter can reach in a session.
        assert!(NOT_PULSING > PULSE_FRAMES.len());
        assert_eq!(NOT_PULSING, usize::MAX);
    }

    // --- About submenu ------------------------------------------------------

    /// The rendered labels, so the assertions read as what a user would see.
    fn labels(facts: &AboutFacts) -> Vec<String> {
        about_items(facts)
            .iter()
            .map(|item| match item {
                ksni::MenuItem::Standard(item) => item.label.clone(),
                ksni::MenuItem::Separator => "—".to_owned(),
                _ => "?".to_owned(),
            })
            .collect()
    }

    fn sample() -> AboutFacts {
        AboutFacts {
            version: "0.1.0".to_owned(),
            licence: "MIT".to_owned(),
            rows: vec![
                ("Model".to_owned(), "large-v3-turbo".to_owned()),
                ("Backend".to_owned(), "vulkan · GPU 1".to_owned()),
            ],
        }
    }

    #[test]
    fn the_submenu_renders_version_licence_and_rows() {
        assert_eq!(
            labels(&sample()),
            [
                "govox 0.1.0",
                "MIT licence",
                "—",
                "Model: large-v3-turbo",
                "Backend: vulkan · GPU 1",
            ]
        );
    }

    /// `set_about` runs a second or two after the icon appears. Until it does,
    /// an empty table would look like a daemon that knows nothing about itself.
    #[test]
    fn before_the_facts_arrive_it_says_so() {
        assert_eq!(labels(&AboutFacts::default()), ["Starting up…"]);
    }

    /// Every row is a readout. A menu entry that looks clickable and does
    /// nothing is worse than one that is plainly inert.
    #[test]
    fn nothing_in_the_submenu_is_activatable() {
        for item in about_items(&sample()) {
            if let ksni::MenuItem::Standard(item) = item {
                assert!(!item.enabled, "{:?} is activatable", item.label);
            }
        }
    }

    /// A missing licence should drop its line rather than render "` licence`".
    #[test]
    fn an_absent_licence_leaves_no_empty_row() {
        let facts = AboutFacts {
            version: "0.1.0".to_owned(),
            licence: String::new(),
            rows: Vec::new(),
        };
        assert_eq!(labels(&facts), ["govox 0.1.0"]);
    }
}
