//! `govox doctor`, driven entirely by fake probes.
//!
//! The point of injecting the probes is that the interesting cases are the
//! *broken* ones, and a diagnostic that can only be tested by breaking your own
//! machine will not be tested. Every scenario below is a machine someone has
//! actually had.

use std::path::PathBuf;

use govox_core::config::{Config, Environment};
use govox_daemon::diagnostics::{Probes, Status, run};

fn defaults() -> Config {
    Config::load_from(None, &Environment::default()).expect("defaults are valid")
}

/// A machine where everything works.
fn healthy() -> Probes {
    Probes {
        session_type: "wayland".to_owned(),
        desktop: "GNOME".to_owned(),
        socket_exists: Box::new(|_| true),
        has_binary: Box::new(|_| true),
        input_devices: Box::new(|| vec![PathBuf::from("/dev/input/event0")]),
        microphone: Box::new(|| None),
        model_cached: Box::new(|_| true),
        ibus: Box::new(|| None),
    }
}

/// Find one check by its stable name.
fn status(report: &govox_daemon::diagnostics::Report, name: &str) -> Status {
    report
        .sections
        .iter()
        .flat_map(|section| &section.checks)
        .find(|check| check.name == name)
        .unwrap_or_else(|| panic!("no check named {name:?}"))
        .status
}

#[test]
fn a_healthy_machine_passes_cleanly() {
    let report = run(&defaults(), &healthy());
    assert!(!report.has_failures());
    assert!(
        report.render().contains("govox can run on this system"),
        "{}",
        report.render()
    );
}

/// The severity distinction that matters most.
///
/// ydotoold not listening is a WARN, not a FAIL: the clipboard fallback still
/// gets recognised text into the focused window, it just needs a paste. Calling
/// this fatal would send people chasing a working system.
#[test]
fn a_missing_ydotool_daemon_is_a_warning_not_a_failure() {
    let probes = Probes {
        socket_exists: Box::new(|_| false),
        ..healthy()
    };
    let report = run(&defaults(), &probes);

    assert_eq!(status(&report, "ydotool"), Status::Warn);
    assert!(!report.has_failures(), "the clipboard fallback still works");
}

/// Losing *both* injection paths is the one genuinely fatal case: recognised
/// text would have nowhere to go.
#[test]
fn losing_every_injection_path_is_fatal() {
    let probes = Probes {
        has_binary: Box::new(|_| false),
        ..healthy()
    };
    let report = run(&defaults(), &probes);

    assert_eq!(status(&report, "injection"), Status::Fail);
    assert!(report.has_failures());
}

/// The check that sends people to the `input` group.
#[test]
fn no_readable_input_devices_is_fatal_and_says_how_to_fix_it() {
    let probes = Probes {
        input_devices: Box::new(Vec::new),
        ..healthy()
    };
    let report = run(&defaults(), &probes);

    assert_eq!(status(&report, "devices"), Status::Fail);
    let rendered = report.render();
    // The remedy is the whole value of the command. "no readable devices" on
    // its own is a restatement of the symptom.
    assert!(rendered.contains("usermod -aG input"), "{rendered}");
    assert!(
        rendered.contains("log out and back in"),
        "the group change not applying to the running session is the part \
         people get stuck on: {rendered}"
    );
}

/// A disabled optional subsystem is SKIP, never WARN.
///
/// Reporting `[ime] enabled = false` as a problem trains people to ignore the
/// output, which costs more than the missing line ever would.
#[test]
fn switched_off_features_are_skipped_rather_than_flagged() {
    let mut config = defaults();
    // Switched off explicitly. `ime.enabled` defaults to true, and
    // `read_focused_field` to false, but what is under test is the reporting of
    // a feature the user turned OFF — so neither should be read from a default
    // that may move again.
    config.ime.enabled = false;
    config.editing.read_focused_field = false;
    config.streaming.enabled = false;

    // Probes that would fail *if* the features were on.
    let probes = Probes {
        ibus: Box::new(|| Some("no ibus-daemon".to_owned())),
        ..healthy()
    };
    let report = run(&config, &probes);

    assert_eq!(status(&report, "ime"), Status::Skip);
    assert_eq!(status(&report, "field_reading"), Status::Skip);
    assert!(!report.has_failures());
    assert!(report.render().contains("govox can run on this system"));
}

/// With preedit on and no daemon, it degrades rather than fails.
#[test]
fn preedit_without_a_daemon_degrades_to_the_caption() {
    let mut config = defaults();
    config.ime.enabled = true;
    let probes = Probes {
        ibus: Box::new(|| Some("ibus-daemon is not running".to_owned())),
        ..healthy()
    };
    let report = run(&config, &probes);

    assert_eq!(status(&report, "ime"), Status::Warn);
    assert!(!report.has_failures());
    assert!(report.render().contains("HUD caption"));
}

#[test]
fn a_model_with_no_gguf_build_is_named_along_with_its_replacement() {
    let mut config = defaults();
    config.recognition.model = "distil-large-v3".to_owned();
    let report = run(&config, &healthy());

    assert_eq!(status(&report, "model_name"), Status::Fail);
    // Naming the alternative is what makes this actionable: the family simply
    // has no GGUF build, so "not found" would read as a typo.
    assert!(
        report.render().contains("large-v3-turbo"),
        "{}",
        report.render()
    );
}

/// An uncached model under an offline policy cannot resolve itself.
#[test]
fn offline_and_uncached_is_fatal_but_cache_first_is_only_slow() {
    let mut config = defaults();
    let probes = Probes {
        model_cached: Box::new(|_| false),
        ..healthy()
    };

    config.recognition.download_policy = govox_core::config::DownloadPolicy::Offline;
    assert_eq!(status(&run(&config, &probes), "model_cached"), Status::Fail);

    config.recognition.download_policy = govox_core::config::DownloadPolicy::CacheFirst;
    assert_eq!(
        status(&run(&config, &probes), "model_cached"),
        Status::Warn,
        "a first-run download is slow, not broken"
    );
}

/// The machine-readable form is what a setup script branches on.
#[test]
fn machine_output_is_one_stable_key_per_check() {
    let report = run(&defaults(), &healthy());
    let machine = report.render_machine();
    let lines: Vec<&str> = machine.lines().collect();

    assert!(lines.iter().all(|line| line.starts_with("check.")));
    assert!(lines.contains(&"check.session=ok"));
    // No prose: remedies are human-mode only, and a script parsing them would
    // break the moment the wording improved.
    assert!(!machine.contains("systemctl"));
}

/// The daemon's injector selection and the diagnostic must agree.
///
/// They read the same probes for exactly this reason: a `doctor` that reports
/// ydotool as available while the daemon silently picked the clipboard is worse
/// than no diagnostic at all.
#[test]
fn capabilities_come_from_the_same_probes_the_report_does() {
    let probes = Probes {
        has_binary: Box::new(|name| name == "wl-copy"),
        ..healthy()
    };
    let caps = govox_daemon::diagnostics::capabilities(&probes);

    assert_eq!(caps.primary_injection.as_deref(), Some("clipboard"));
    assert!(!caps.supports_injection("ydotool"));
    assert!(caps.supported);

    let report = run(&defaults(), &probes);
    assert_eq!(status(&report, "ydotool"), Status::Warn);
    assert_eq!(status(&report, "clipboard"), Status::Ok);
}
