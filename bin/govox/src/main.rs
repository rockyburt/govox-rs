//! The `govox` CLI.
//!
//! Subcommands mirror `govox-py`'s argparse surface so muscle memory and the
//! systemd unit carry over unchanged.

mod reference;

use clap::{Parser, Subcommand};
use govox_core::config::Config;

/// Exit codes, which the systemd unit depends on.
///
/// `RestartPreventExitStatus=2` means a configuration or dictionary error must
/// fail visibly instead of restart-looping, so [`EXIT_CONFIG`] is reserved for
/// exactly that and must not be reused for ordinary runtime failures.
const EXIT_RUNTIME: i32 = 1;
const EXIT_CONFIG: i32 = 2;

#[derive(Parser)]
#[command(
    name = "govox",
    version,
    about = "Wayland-first speech-to-text dictation daemon"
)]
struct Cli {
    /// Additional config file, applied last over the layered defaults.
    #[arg(long, value_name = "PATH", global = true)]
    config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the dictation daemon.
    Run,
    /// Report whether this desktop can support dictation, and what is missing.
    Doctor {
        /// Emit `key=value` lines instead of prose.
        #[arg(long)]
        machine: bool,
    },
    /// List input microphones.
    Devices,
    /// Print key names as they are pressed, for configuring activation.
    Keys,
    /// List every phrase govox understands, and which are switched on.
    Commands,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(Error::Config(message)) => {
            eprintln!("govox: {message}");
            std::process::ExitCode::from(EXIT_CONFIG as u8)
        }
        Err(Error::Runtime(message)) => {
            eprintln!("govox: {message}");
            std::process::ExitCode::from(EXIT_RUNTIME as u8)
        }
    }
}

enum Error {
    /// Configuration or dictionary failure. Maps to [`EXIT_CONFIG`], which the
    /// systemd unit's `RestartPreventExitStatus=2` depends on: these must fail
    /// visibly rather than restart-loop.
    Config(String),
    Runtime(String),
}

fn run(cli: &Cli) -> Result<(), Error> {
    // Loaded before dispatch, for every subcommand, exactly as the reference
    // does — so an invalid config is reported the same way whichever
    // subcommand surfaced it, and always with exit code 2.
    let config = Config::load(cli.config.as_deref()).map_err(|e| Error::Config(e.to_string()))?;

    // After the config, because the config is what says how to log — and
    // before anything else, so the first line the daemon emits is already
    // filtered the way the user asked. A config failure is reported by `main`
    // on stderr and needs no subscriber.
    init_logging(&config.logging);

    tracing::debug!(
        model = %config.recognition.model,
        device = %config.recognition.device,
        activation = %config.activation.mode,
        injection = %config.injection.method,
        streaming = config.streaming.enabled,
        ime = config.ime.enabled,
        "configuration loaded"
    );

    match &cli.command {
        Command::Run => dictate(config),
        Command::Doctor { machine } => doctor(&config, *machine),
        Command::Devices => devices(),
        Command::Keys => keys(),
        Command::Commands => {
            print!("{}", reference::render(&config));
            Ok(())
        }
    }
}

/// Install the tracing subscriber described by `[logging]`.
///
/// `RUST_LOG` still wins when it is set. It is the escape hatch for debugging
/// a daemon you cannot easily reconfigure — including one whose config is the
/// thing you are debugging — and a config file that silently overrode the
/// environment would take that away.
///
/// `[logging] format` is **not** honoured: it is a `%(asctime)s`-style Python
/// format string with no `tracing` equivalent. Recorded in docs/parity.md.
fn init_logging(config: &govox_core::config::LoggingConfig) {
    use govox_core::config::LogStyle;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(govox_core::logging::filter_directives(config))
    });
    let builder = tracing_subscriber::fmt().with_env_filter(filter);

    match config.style {
        // Structured output for a log shipper, so the whole event survives
        // rather than the message alone.
        LogStyle::Json => builder.json().init(),
        LogStyle::Plain => builder.with_ansi(false).init(),
        LogStyle::Color => builder.with_ansi(true).init(),
        // `auto` is the crate's own default: colour when the output is a
        // terminal, plain when it is a journal or a file.
        LogStyle::Auto => builder.init(),
    }
}

/// Report whether this machine can dictate, and what would fix it if not.
///
/// Exits non-zero on a FAIL so a setup script can branch on it. Warnings do
/// not: they describe a degraded but working system, and failing on them would
/// make the exit code useless for the thing it is actually for.
fn doctor(config: &Config, machine: bool) -> Result<(), Error> {
    let probes = govox_daemon::diagnostics::Probes::default();
    let report = govox_daemon::diagnostics::run(config, &probes);
    print!(
        "{}",
        if machine {
            report.render_machine()
        } else {
            report.render()
        }
    );
    if report.has_failures() {
        return Err(Error::Runtime(
            "doctor found a problem that stops govox running".into(),
        ));
    }
    Ok(())
}

/// Start the dictation pipeline and run until interrupted.
fn dictate(config: Config) -> Result<(), Error> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| Error::Runtime(format!("could not start the async runtime: {e}")))?;

    runtime.block_on(async move {
        let cancel = tokio_util::sync::CancellationToken::new();

        // Detached so it runs alongside the daemon rather than before it; the
        // runtime drops it once `run` returns.
        tokio::spawn(cancel_on_signal(cancel.clone()));

        govox_daemon::run(config, cancel)
            .await
            .map_err(|e| Error::Runtime(e.to_string()))
    })
}

/// Wait for a shutdown signal, then cancel `token`.
///
/// Ctrl-C and SIGTERM both mean "stop cleanly". Cancelling rather than exiting
/// lets the recognition thread release the GPU context and the capture stream
/// close, instead of both being torn down mid-call.
async fn cancel_on_signal(token: tokio_util::sync::CancellationToken) {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => tracing::info!("interrupted; stopping"),
                _ = term.recv() => tracing::info!("terminated; stopping"),
            }
        }
        // Losing SIGTERM costs us the clean systemd stop, but Ctrl-C is the
        // signal a person is most likely to send, so keep handling that rather
        // than dropping back to the default disposition for both.
        Err(error) => {
            tracing::warn!(%error, "cannot listen for SIGTERM; Ctrl-C only");
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("interrupted; stopping");
        }
    }
    token.cancel();
}

/// List microphones, in `govox-py`'s exact line format.
fn devices() -> Result<(), Error> {
    let devices = govox_audio::list_devices();
    if devices.is_empty() {
        return Err(Error::Runtime(
            "devices: no input devices found. Is PipeWire or PulseAudio running?".into(),
        ));
    }
    for device in devices {
        // The id is printed because it is what `[audio] device` takes: labels
        // are duplicated across devices, so a label alone cannot tell you which
        // of five identically-named entries to configure.
        // `{:g}`-style formatting: 44100 rather than 44100.0.
        println!(
            "{}: {} [{}] ({} ch, {} Hz){}",
            device.index,
            device.name,
            device.id,
            device.channels,
            device.default_sample_rate,
            if device.is_default { " (default)" } else { "" }
        );
    }
    Ok(())
}

/// Print the canonical evdev name of every key pressed, until interrupted.
///
/// Helps the user pick a leak-safe activation key on their own keyboard
/// without guessing evdev constants. Fails non-zero when no readable keyboard
/// is found, so the failure is scriptable.
fn keys() -> Result<(), Error> {
    use govox_input::evdev_listener::{find_key_devices, open_device, to_key_event};

    let paths = find_key_devices();
    if paths.is_empty() {
        return Err(Error::Runtime(
            "keys: no readable keyboard devices found. Add yourself to the 'input' \
             group or run with access to /dev/input/event*."
                .into(),
        ));
    }

    let mut devices = Vec::new();
    for path in &paths {
        match open_device(path) {
            Ok(device) => devices.push(device),
            // Skip rather than abort: enumeration found the node, but another
            // process may hold it. One unreadable keyboard must not stop the
            // user reading the key they just pressed on a different one.
            Err(error) => tracing::warn!(%error, "skipping input device"),
        }
    }
    if devices.is_empty() {
        return Err(Error::Runtime(
            "keys: every keyboard found was unreadable; check /dev/input permissions".into(),
        ));
    }

    println!("Press a key to see its evdev name (e.g. KEY_SCROLLLOCK). Press Ctrl-C to stop.");

    // A thread per device, all printing to the same stdout. Blocking reads are
    // fine here: this subcommand does nothing else, and it keeps `keys` free of
    // a tokio runtime it would otherwise need for one interactive loop.
    std::thread::scope(|scope| {
        for mut device in devices {
            scope.spawn(move || {
                loop {
                    let Ok(events) = device.fetch_events() else {
                        return; // unplugged
                    };
                    for event in events {
                        // Key-down only, so each press prints once rather than
                        // twice.
                        if let Some(govox_core::activation::KeyEvent::Down(name)) =
                            to_key_event(&event)
                        {
                            println!("  {name}");
                        }
                    }
                }
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// Subcommands govox-rs has that the reference did not, each with a row in
    /// `docs/parity.md`. Declared rather than folded into the list below, so
    /// the guard keeps catching an *accidental* addition — which is what it is
    /// for. Muscle memory and the systemd unit depend on the shared four.
    ///
    /// - `commands` — prints the phrase listing, generated from the grammar
    ///   tables. The reference documents its grammar only in source.
    const ADDED_SUBCOMMANDS: &[&str] = &["commands"];

    #[test]
    fn subcommands_match_the_python_surface() {
        let names: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|s| s.get_name().to_owned())
            .filter(|name| !ADDED_SUBCOMMANDS.contains(&name.as_str()))
            .collect();
        assert_eq!(names, ["run", "doctor", "devices", "keys"]);
    }

    /// Guard the allowlist itself: an entry that no longer names a real
    /// subcommand is dead weight that would mask the next genuine addition.
    #[test]
    fn every_declared_addition_is_really_a_subcommand() {
        let names: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|s| s.get_name().to_owned())
            .collect();
        for added in ADDED_SUBCOMMANDS {
            assert!(
                names.iter().any(|name| name == added),
                "{added} is declared as an addition but is not a subcommand"
            );
        }
    }
}
