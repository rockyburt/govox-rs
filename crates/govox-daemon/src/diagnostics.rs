//! `govox doctor` — can this machine dictate, and if not, what would fix it?
//!
//! The value of this command is entirely in the **remedies**. A check that says
//! "ydotool: unavailable" is a restatement of the symptom; one that names the
//! socket it looked for and the command that starts it is the difference
//! between a diagnostic and a shrug. So every WARN and FAIL carries the steps
//! that clear it, and a check with no actionable remedy is a check worth
//! deleting.
//!
//! The severities are deliberate and not interchangeable:
//!
//! * **FAIL** means govox cannot dictate at all. Only a total lack of injection
//!   or of hotkey capture qualifies.
//! * **WARN** means something is degraded but dictation works. ydotool being
//!   unreachable is a WARN, not a FAIL, because the clipboard fallback still
//!   gets text into the focused window — it just needs a paste.
//! * **SKIP** means the feature is switched off in the configuration. Reporting
//!   a disabled optional subsystem as a problem trains people to ignore the
//!   output, which costs more than the missing line ever would.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use govox_core::config::{Config, DownloadPolicy, RecognitionConfig};
use govox_core::domain::Capabilities;

/// How a single check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl Status {
    const fn mark(self) -> &'static str {
        match self {
            Self::Ok => "[ok]",
            Self::Warn => "[~~]",
            Self::Fail => "[--]",
            Self::Skip => "[··]",
        }
    }

    const fn machine(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }
}

/// One thing that was looked at.
#[derive(Debug, Clone)]
pub struct Check {
    /// Stable identifier, for the machine-readable output.
    pub name: String,
    /// What the user calls it.
    pub label: String,
    pub status: Status,
    pub summary: String,
    /// What to do about it. Empty for an OK or a SKIP.
    pub remedy: Vec<String>,
}

impl Check {
    fn new(name: &str, label: &str, status: Status, summary: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            label: label.to_owned(),
            status,
            summary: summary.into(),
            remedy: Vec::new(),
        }
    }

    fn with_remedy(mut self, steps: &[&str]) -> Self {
        self.remedy = steps.iter().map(|step| (*step).to_owned()).collect();
        self
    }
}

/// A group of related checks.
#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub checks: Vec<Check>,
}

/// Everything `doctor` found.
#[derive(Debug, Clone)]
pub struct Report {
    pub sections: Vec<Section>,
}

impl Report {
    fn all(&self) -> impl Iterator<Item = &Check> {
        self.sections.iter().flat_map(|section| &section.checks)
    }

    /// Whether anything means govox cannot run.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.all().any(|check| check.status == Status::Fail)
    }

    /// The report as the user reads it.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from("govox doctor\n\n");
        for section in &self.sections {
            let _ = writeln!(out, "  {}", section.title);
            for check in &section.checks {
                let _ = writeln!(
                    out,
                    "    {}  {}  —  {}",
                    check.status.mark(),
                    check.label,
                    check.summary
                );
            }
            out.push('\n');
        }

        let problems: Vec<&Check> = self
            .all()
            .filter(|check| matches!(check.status, Status::Warn | Status::Fail))
            .collect();
        if self.has_failures() {
            out.push_str("  govox cannot run  — see Troubleshooting below\n");
        } else if problems.is_empty() {
            out.push_str("  govox can run on this system\n");
        } else {
            out.push_str("  govox can run, with warnings  — see Troubleshooting below\n");
        }

        let actionable: Vec<&&Check> = problems
            .iter()
            .filter(|check| !check.remedy.is_empty())
            .collect();
        if !actionable.is_empty() {
            out.push_str("\n  Troubleshooting\n");
            for check in actionable {
                let _ = writeln!(out, "  - {}: {}", check.label, check.summary);
                for step in &check.remedy {
                    let _ = writeln!(out, "      {step}");
                }
            }
        }
        out
    }

    /// One `check.<name>=<status>` line each, for scripts.
    #[must_use]
    pub fn render_machine(&self) -> String {
        let mut out = String::new();
        for check in self.all() {
            let _ = writeln!(out, "check.{}={}", check.name, check.status.machine());
        }
        out
    }
}

/// What the checks are allowed to look at.
///
/// Injected so the whole diagnostic is testable without a desktop — which
/// matters more here than anywhere else in the project, because a `doctor` that
/// can only be tested by breaking your own machine will not be tested.
pub struct Probes {
    pub session_type: String,
    pub desktop: String,
    /// Does this path exist and is it connectable?
    pub socket_exists: Box<dyn Fn(&Path) -> bool + Send + Sync>,
    /// Is this binary on `$PATH`?
    pub has_binary: Box<dyn Fn(&str) -> bool + Send + Sync>,
    /// Readable `/dev/input/event*` devices.
    pub input_devices: Box<dyn Fn() -> Vec<PathBuf> + Send + Sync>,
    /// Is a microphone available? `None` for yes, `Some(reason)` for no.
    pub microphone: Box<dyn Fn() -> Option<String> + Send + Sync>,
    /// Is this model already on disk, so the first run does not download?
    pub model_cached: Box<dyn Fn(&RecognitionConfig) -> bool + Send + Sync>,
    /// Is there a live ibus-daemon? `None` for yes.
    pub ibus: Box<dyn Fn() -> Option<String> + Send + Sync>,
}

impl Default for Probes {
    fn default() -> Self {
        Self {
            session_type: std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
            desktop: std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
            socket_exists: Box::new(|path| path.exists()),
            has_binary: Box::new(|name| which(name).is_some()),
            input_devices: Box::new(readable_input_devices),
            microphone: Box::new(|| {
                let devices = govox_audio::capture::list_devices();
                devices
                    .is_empty()
                    .then(|| "no capture device is available".to_owned())
            }),
            // "Cached" asked as a *resolution under an offline policy*: the
            // same question the daemon asks at startup, so unlike a guess at
            // the cache layout it cannot drift. Never touches the network.
            model_cached: Box::new(|config| {
                let mut offline = config.clone();
                offline.download_policy = DownloadPolicy::Offline;
                govox_asr::model::resolve(&offline).is_ok()
            }),
            ibus: Box::new(|| govox_ime::address::discover().err().map(|e| e.to_string())),
        }
    }
}

/// Run every check.
#[must_use]
pub fn run(config: &Config, probes: &Probes) -> Report {
    Report {
        sections: vec![
            session(probes),
            injection(config, probes),
            hotkeys(config, probes),
            audio(probes),
            recognition(config, probes),
            preedit(config, probes),
            field_reading(config),
        ],
    }
}

fn session(probes: &Probes) -> Section {
    let check = if probes.session_type == "wayland" {
        let summary = if probes.desktop.is_empty() {
            "wayland".to_owned()
        } else {
            format!("wayland ({})", probes.desktop)
        };
        Check::new("session", "session", Status::Ok, summary)
    } else if probes.session_type == "x11" {
        // Not a failure: ydotool writes to /dev/uinput, which the compositor is
        // not involved in, so injection works. Only the parts that assume
        // Wayland's rules are affected.
        Check::new(
            "session",
            "session",
            Status::Warn,
            "x11 — govox targets wayland",
        )
        .with_remedy(&["govox is developed against Wayland; X11 is untested, not unsupported."])
    } else {
        Check::new(
            "session",
            "session",
            Status::Warn,
            format!("unknown session type {:?}", probes.session_type),
        )
        .with_remedy(&[
            "XDG_SESSION_TYPE is unset. If you are running under systemd --user,",
            "the unit needs the graphical session's environment:",
            "  systemctl --user import-environment XDG_SESSION_TYPE XDG_CURRENT_DESKTOP",
        ])
    };
    Section {
        title: "Session".to_owned(),
        checks: vec![check],
    }
}

/// ydotool failing is a WARN, not a FAIL: the clipboard fallback keeps govox
/// usable — text lands on the clipboard and is pasted, rather than typed into
/// the focused window. Only a total lack of injection is a FAIL.
fn injection(config: &Config, probes: &Probes) -> Section {
    let mut checks = Vec::new();
    // Not from `[injection] ydotool_socket`: that key configures the *runner*,
    // while ydotoold listens where `$YDOTOOL_SOCKET` says or at
    // /run/user/<uid>/.ydotool_socket. Checking the config key would report a
    // missing socket on every machine, since the default is empty.
    let socket = ydotool_socket(&config.injection.ydotool_socket);

    let ydotool_ok = (probes.has_binary)("ydotool") && (probes.socket_exists)(&socket);
    if ydotool_ok {
        checks.push(Check::new(
            "ydotool",
            "ydotool",
            Status::Ok,
            format!("ydotoold reachable at {}", socket.display()),
        ));
    } else if (probes.has_binary)("ydotool") {
        checks.push(
            Check::new(
                "ydotool",
                "ydotool",
                Status::Warn,
                format!("ydotoold is not listening at {}", socket.display()),
            )
            .with_remedy(&[
                "systemctl --user enable --now ydotoold.service",
                "If the socket lives elsewhere, set [injection] ydotool_socket.",
            ]),
        );
    } else {
        checks.push(
            Check::new(
                "ydotool",
                "ydotool",
                Status::Warn,
                "ydotool is not installed",
            )
            .with_remedy(&[
                "sudo apt install ydotool",
                "then: systemctl --user enable --now ydotoold.service",
            ]),
        );
    }

    let clipboard_ok = (probes.has_binary)("wl-copy");
    if clipboard_ok {
        checks.push(Check::new(
            "clipboard",
            "clipboard",
            Status::Ok,
            "wl-copy available (fallback)",
        ));
    } else {
        checks.push(
            Check::new(
                "clipboard",
                "clipboard",
                Status::Warn,
                "wl-copy is not installed, so there is no injection fallback",
            )
            .with_remedy(&["sudo apt install wl-clipboard"]),
        );
    }

    if !ydotool_ok && !clipboard_ok {
        // Both gone is the one case that is genuinely fatal: recognised text
        // would have nowhere to go.
        checks.push(
            Check::new(
                "injection",
                "text injection",
                Status::Fail,
                "no way to get text into the focused window",
            )
            .with_remedy(&["Install either ydotool or wl-clipboard; see the two lines above."]),
        );
    }

    Section {
        title: "Text injection".to_owned(),
        checks,
    }
}

fn hotkeys(config: &Config, probes: &Probes) -> Section {
    let mut checks = Vec::new();

    // Every configured key is checked, not just the first: with two Controls
    // bound, a typo in one of them would otherwise pass doctor and then fail
    // only for whichever hand the user happened to use.
    let keys = &config.activation.toggle_key;
    let unknown: Vec<&String> = keys
        .names()
        .iter()
        .filter(|key| {
            govox_core::keycodes::KeyCode::named(key).is_none() && !key.starts_with("KEY_")
        })
        .collect();
    if keys.is_empty() {
        checks.push(
            Check::new(
                "key_toggle",
                "activation key",
                Status::Fail,
                "no activation key is configured".to_owned(),
            )
            .with_remedy(&["Set `[activation] toggle_key` to a key name, or a list of them."]),
        );
    } else if unknown.is_empty() {
        checks.push(Check::new(
            "key_toggle",
            "activation key",
            Status::Ok,
            keys.describe(),
        ));
    } else {
        let names: Vec<String> = unknown.iter().map(|k| format!("{k:?}")).collect();
        checks.push(
            Check::new(
                "key_toggle",
                "activation key",
                Status::Fail,
                format!("{} is not a key name evdev knows", names.join(", ")),
            )
            .with_remedy(&["Run `govox keys` and press the key you want; it prints the name."]),
        );
    }

    let devices = (probes.input_devices)();
    if devices.is_empty() {
        checks.push(
            Check::new(
                "devices",
                "input devices",
                Status::Fail,
                "no readable devices in /dev/input",
            )
            .with_remedy(&[
                "govox watches the keyboard directly, which needs group membership:",
                "  sudo usermod -aG input $USER",
                "then log out and back in — group changes do not apply to a running session.",
            ]),
        );
    } else {
        checks.push(Check::new(
            "devices",
            "input devices",
            Status::Ok,
            format!("{} readable device(s) in /dev/input", devices.len()),
        ));
    }

    Section {
        title: "Hotkey capture".to_owned(),
        checks,
    }
}

fn audio(probes: &Probes) -> Section {
    let check = match (probes.microphone)() {
        None => Check::new(
            "microphone",
            "microphone",
            Status::Ok,
            "a capture device opened",
        ),
        Some(reason) => {
            Check::new("microphone", "microphone", Status::Fail, reason).with_remedy(&[
                "Run `govox devices` to see what the system offers,",
                "then name one in [audio] device.",
            ])
        }
    };
    Section {
        title: "Audio".to_owned(),
        checks: vec![check],
    }
}

fn recognition(config: &Config, probes: &Probes) -> Section {
    let mut checks = Vec::new();
    let model = &config.recognition.model;

    match govox_asr::model::gguf_filename(model) {
        Some(_) => checks.push(Check::new(
            "model_name",
            "model",
            Status::Ok,
            format!("{model} is a known checkpoint"),
        )),
        None => checks.push(
            Check::new(
                "model_name",
                "model",
                Status::Fail,
                format!("{model:?} has no GGUF build"),
            )
            .with_remedy(&[
                "whisper.cpp needs a GGUF checkpoint. The distil-* family has none;",
                "large-v3-turbo is the closest equivalent.",
            ]),
        ),
    }

    if (probes.model_cached)(&config.recognition) {
        checks.push(Check::new(
            "model_cached",
            "model file",
            Status::Ok,
            "present in the cache",
        ));
    } else if config.recognition.download_policy == DownloadPolicy::Offline {
        checks.push(
            Check::new(
                "model_cached",
                "model file",
                Status::Fail,
                "not cached, and [recognition] download_policy is \"offline\"",
            )
            .with_remedy(&[
                "Either set download_policy to \"cache_first\" for one run,",
                "or copy the ggml-*.bin into the cache by hand.",
            ]),
        );
    } else {
        // Not a warning about correctness — just about the first run being slow.
        checks.push(Check::new(
            "model_cached",
            "model file",
            Status::Warn,
            "not cached; the first run will download it",
        ));
    }

    Section {
        title: "Recognition".to_owned(),
        checks,
    }
}

fn preedit(config: &Config, probes: &Probes) -> Section {
    if !config.ime.enabled {
        return Section {
            title: "Preedit dictation".to_owned(),
            checks: vec![Check::new(
                "ime",
                "input method",
                Status::Skip,
                "[ime] enabled is false",
            )],
        };
    }
    let check = match (probes.ibus)() {
        None => Check::new(
            "ime",
            "input method",
            Status::Ok,
            format!("ibus-daemon reachable; engine {:?}", config.ime.engine_name),
        ),
        Some(reason) => Check::new("ime", "input method", Status::Warn, reason).with_remedy(&[
            "Preedit needs a running ibus-daemon. Without one, dictation still",
            "works — interim text stays in the HUD caption instead of the field.",
            "  ibus-daemon -drx",
        ]),
    };
    Section {
        title: "Preedit dictation".to_owned(),
        checks: vec![check],
    }
}

fn field_reading(config: &Config) -> Section {
    let check = if config.editing.read_focused_field {
        // Deliberately not probed. Whether a *field* is readable is a property
        // of the focused element, not the desktop, so a connection test would
        // report "OK" and say nothing about the app. The live test answers it.
        Check::new(
            "field_reading",
            "focused field",
            Status::Ok,
            "enabled; coverage depends on the focused application",
        )
    } else {
        Check::new(
            "field_reading",
            "focused field",
            Status::Skip,
            "[editing] read_focused_field is false",
        )
    };
    Section {
        title: "Field reading".to_owned(),
        checks: vec![check],
    }
}

/// Where ydotoold listens.
///
/// An explicit `[injection] ydotool_socket` wins, then `$YDOTOOL_SOCKET`, then
/// ydotool's own default. Same order as the reference.
fn ydotool_socket(configured: &str) -> PathBuf {
    if !configured.trim().is_empty() {
        return PathBuf::from(shellexpand(configured.trim()));
    }
    if let Ok(from_env) = std::env::var("YDOTOOL_SOCKET")
        && !from_env.is_empty()
    {
        return PathBuf::from(from_env);
    }
    PathBuf::from(format!("/run/user/{}/.ydotool_socket", uid()))
}

/// This process's real user id.
fn uid() -> u32 {
    // Reading it rather than calling libc keeps this crate free of a C
    // dependency for one number.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                .and_then(|line| line.split_whitespace().next()?.parse().ok())
        })
        .unwrap_or(1000)
}

/// Expand a leading `~`, which is all the config paths use.
fn shellexpand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => path.to_owned(),
        },
        None => path.to_owned(),
    }
}

/// Is `name` on `$PATH`?
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// `/dev/input/event*` this process can actually open.
///
/// Readable, not merely present: the whole point of the check is the `input`
/// group, and a listing would report success for a user who cannot open one.
fn readable_input_devices() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("event"))
                && std::fs::File::open(path).is_ok()
        })
        .collect()
}

/// The environment as the injector selector wants it.
#[must_use]
pub fn capabilities(probes: &Probes) -> Capabilities {
    let ydotool = (probes.has_binary)("ydotool");
    let clipboard = (probes.has_binary)("wl-copy");
    let mut strategies = Vec::new();
    if ydotool {
        strategies.push("ydotool".to_owned());
    }
    if clipboard {
        strategies.push("clipboard".to_owned());
    }
    let mut reasons = Vec::new();
    if !ydotool {
        reasons.push("ydotool is not installed".to_owned());
    }
    if !clipboard {
        reasons.push("wl-copy is not installed".to_owned());
    }
    Capabilities {
        session_type: probes.session_type.clone(),
        desktop: probes.desktop.clone(),
        supported: !strategies.is_empty(),
        primary_injection: strategies.first().cloned(),
        injection_strategies: strategies,
        hotkey_strategies: vec!["evdev".to_owned()],
        reasons,
        ime_available: (probes.ibus)().is_none(),
    }
}
