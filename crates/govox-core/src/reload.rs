//! Re-reading config and dictionary while the daemon runs.
//!
//! The daemon holds the personal dictionary in more than one place — the
//! recogniser's bias prompt and the correction pipeline's replacements — and
//! every one of them was fixed at startup. Editing `dictionary.toml` therefore
//! meant `systemctl --user restart govox`, which reloads the Whisper model and
//! drops the session. Both are reachable now, and the daemon watches the files
//! so a save is enough; see `govox-daemon`'s `watch` module.
//!
//! Only some configuration can change under a running daemon. The rest — the
//! model, the audio device, the activation key, the input method — is wired
//! into objects at construction, so this module's other job is to say plainly
//! which edits did *not* take effect, rather than let a reload look like it
//! worked.

use crate::config::Config;

/// Sections a reload can genuinely apply.
///
/// `correction` is read per utterance by the pipeline and `logging` is
/// re-applied by reconfiguring the subscriber, so both take effect at once.
/// The personal dictionary is reported separately: it lives in its own file
/// and is the reason this feature exists.
/// `commands` joins them because [`crate::config::Config::commands`] is read
/// through the correction pipeline, which is rebuilt wholesale on publish —
/// and because editing a custom command is an edit-and-try loop, exactly like
/// calibrating a caret offset. Making each try cost a model reload would make
/// the feature unusable.
pub const RELOADABLE_SECTIONS: &[&str] = &["correction", "logging", "commands"];

/// Keys inside otherwise restart-only sections that a running daemon *can*
/// adopt.
///
/// `[feedback]` mostly describes surfaces built at startup — the overlay
/// process, its position, the chime — but `app_rules` are consulted per
/// session, so they can change under a running daemon. Calibrating a
/// per-application caret offset is an edit-and-try loop, and making each try
/// cost a model reload would make it unusable.
pub const LIVE_KEYS: &[(&str, &[&str])] = &[("feedback", &["app_rules"])];

/// Every top-level section, in declaration order.
///
/// Order is not cosmetic: it decides the order sections are named in the
/// summary a user reads in a notification.
pub const SECTIONS: &[&str] = &[
    "audio",
    "recognition",
    "streaming",
    "correction",
    "injection",
    "activation",
    "indicator",
    "vad",
    "editing",
    "ime",
    "feedback",
    "telemetry",
    "logging",
    "commands",
];

/// What a reload attempt did, in terms a notification can state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReloadOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub applied: Vec<String>,
    pub needs_restart: Vec<String>,
}

impl ReloadOutcome {
    #[must_use]
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Self::default()
        }
    }

    /// One line for a caption or a desktop notification.
    #[must_use]
    pub fn summary(&self) -> String {
        if !self.ok {
            let error = self.error.as_deref().unwrap_or("");
            return format!("Reload failed — keeping the previous settings. {error}");
        }
        let applied = if self.applied.is_empty() {
            "nothing changed".to_owned()
        } else {
            self.applied.join(", ")
        };
        if self.needs_restart.is_empty() {
            return format!("Reloaded {applied}.");
        }
        let sections = self.needs_restart.join(", ");
        format!("Reloaded {applied}. Restart required for: {sections}.")
    }

    /// Whether this reload left the running daemon exactly as it was.
    ///
    /// Only true when the files parsed *and* nothing changed *and* nothing
    /// needs a restart — a save that touched a restart-only key is not a no-op,
    /// it is the case where the user most needs to be told. Used to keep an
    /// automatic reload silent; a requested one always answers.
    #[must_use]
    pub fn is_no_op(&self) -> bool {
        self.ok && self.applied.is_empty() && self.needs_restart.is_empty()
    }
}

/// Top-level sections whose contents differ, in declaration order.
///
/// Compared through the serialized form rather than field by field. Hand-rolled
/// comparisons are exactly where a new config key gets forgotten, and the
/// failure mode is silent: the section stops being reported as changed and the
/// user is told a restart is unnecessary when it is.
#[must_use]
pub fn changed_sections(old: &Config, new: &Config) -> Vec<String> {
    let (Ok(old), Ok(new)) = (toml::Table::try_from(old), toml::Table::try_from(new)) else {
        // Serialization cannot fail for a valid Config; if it somehow does,
        // reporting everything as changed is the safe direction.
        return SECTIONS.iter().map(|s| (*s).to_owned()).collect();
    };

    SECTIONS
        .iter()
        .filter(|name| old.get(**name) != new.get(**name))
        .map(|name| (*name).to_owned())
        .collect()
}

/// Changed sections a running daemon cannot adopt.
///
/// Reported so a reload never silently no-ops: changing `[recognition] model`
/// and clicking Reload must not look like it switched models.
///
/// A section whose *only* change is a live key is not reported, or calibrating
/// an app rule would tell the user to restart when the rule had already taken
/// effect — a false alarm that teaches them to ignore the message.
#[must_use]
pub fn restart_required(old: &Config, new: &Config) -> Vec<String> {
    changed_sections(old, new)
        .into_iter()
        .filter(|name| !RELOADABLE_SECTIONS.contains(&name.as_str()))
        .filter(|name| !only_live_keys_changed(name, old, new))
        .collect()
}

fn only_live_keys_changed(name: &str, old: &Config, new: &Config) -> bool {
    let Some((_, live)) = LIVE_KEYS.iter().find(|(section, _)| *section == name) else {
        return false;
    };
    let (Ok(old), Ok(new)) = (toml::Table::try_from(old), toml::Table::try_from(new)) else {
        return false;
    };

    // Blank the live keys in both and compare what is left. Equal means every
    // difference was in a key the running daemon already adopted.
    let blank = |table: &toml::Table| -> Option<toml::Value> {
        let mut section = table.get(name)?.clone();
        if let Some(map) = section.as_table_mut() {
            for key in *live {
                map.remove(*key);
            }
        }
        Some(section)
    };
    blank(&old) == blank(&new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Environment;

    fn defaults() -> Config {
        Config::load_from(None, &Environment::default()).expect("defaults are valid")
    }

    fn app_rule(pattern: &str) -> crate::config::OverlayAppRule {
        crate::config::OverlayAppRule {
            match_: pattern.to_owned(),
            caret_offset_x: 4,
            caret_offset_y: 0,
            follow_caret: None,
        }
    }

    #[test]
    fn a_reload_that_applied_nothing_and_needs_nothing_is_a_no_op() {
        assert!(
            ReloadOutcome {
                ok: true,
                ..ReloadOutcome::default()
            }
            .is_no_op()
        );
    }

    #[test]
    fn a_reload_that_applied_something_is_not_a_no_op() {
        let outcome = ReloadOutcome {
            ok: true,
            applied: vec!["dictionary".to_owned()],
            ..ReloadOutcome::default()
        };
        assert!(!outcome.is_no_op());
    }

    #[test]
    fn a_restart_only_edit_is_not_a_no_op() {
        // Nothing was applied, but this is the case the user most needs told:
        // they changed the model and the running daemon still has the old one.
        let outcome = ReloadOutcome {
            ok: true,
            needs_restart: vec!["recognition".to_owned()],
            ..ReloadOutcome::default()
        };
        assert!(!outcome.is_no_op());
    }

    #[test]
    fn a_failed_reload_is_never_a_no_op() {
        assert!(!ReloadOutcome::failed("bad toml").is_no_op());
    }

    #[test]
    fn an_unchanged_config_reports_nothing() {
        let config = defaults();
        assert!(changed_sections(&config, &config).is_empty());
        assert!(restart_required(&config, &config).is_empty());
    }

    #[test]
    fn the_section_list_matches_the_config_struct() {
        // If a section is added to Config and not here, it silently stops
        // being diffed: the user is told no restart is needed when it is.
        let table = toml::Table::try_from(defaults()).expect("config serializes");
        let mut actual: Vec<String> = table.keys().cloned().collect();
        let mut listed: Vec<String> = SECTIONS.iter().map(|s| (*s).to_owned()).collect();
        actual.sort();
        listed.sort();
        assert_eq!(actual, listed, "SECTIONS has drifted from Config");
    }

    #[test]
    fn a_reloadable_change_needs_no_restart() {
        let old = defaults();
        let mut new = defaults();
        new.correction.enabled = !old.correction.enabled;

        assert_eq!(changed_sections(&old, &new), ["correction"]);
        assert!(restart_required(&old, &new).is_empty());
    }

    #[test]
    fn a_restart_only_change_is_reported() {
        // The case that must never look like it worked.
        let old = defaults();
        let mut new = defaults();
        new.recognition.model = "large-v3-turbo".to_owned();

        assert_eq!(changed_sections(&old, &new), ["recognition"]);
        assert_eq!(restart_required(&old, &new), ["recognition"]);
    }

    #[test]
    fn a_feedback_app_rule_change_alone_needs_no_restart() {
        // Calibrating a caret offset is an edit-and-try loop; a false "restart
        // required" here teaches the user to ignore the message.
        let old = defaults();
        let mut new = defaults();
        new.feedback.app_rules = vec![app_rule("firefox")];

        assert_eq!(changed_sections(&old, &new), ["feedback"]);
        assert!(
            restart_required(&old, &new).is_empty(),
            "app_rules are consulted per session, so they are already live"
        );
    }

    #[test]
    fn another_feedback_change_still_needs_a_restart() {
        // The live-key exemption must not swallow its whole section.
        let old = defaults();
        let mut new = defaults();
        new.feedback.app_rules = vec![app_rule("firefox")];
        new.feedback.overlay = !old.feedback.overlay;

        assert_eq!(restart_required(&old, &new), ["feedback"]);
    }

    #[test]
    fn sections_are_reported_in_declaration_order() {
        let old = defaults();
        let mut new = defaults();
        new.logging.style = crate::config::LogStyle::Json;
        new.audio.frame_ms = 20;
        new.recognition.beam_size = 3;

        // Declaration order, not the order they were edited in.
        assert_eq!(
            changed_sections(&old, &new),
            ["audio", "recognition", "logging"]
        );
    }

    #[test]
    fn a_failed_reload_keeps_the_previous_settings_and_says_so() {
        let outcome = ReloadOutcome::failed("bad TOML at line 4");
        assert!(!outcome.ok);
        let summary = outcome.summary();
        assert!(summary.contains("keeping the previous settings"));
        assert!(summary.contains("bad TOML at line 4"));
    }

    #[test]
    fn a_reload_that_changed_nothing_says_so_rather_than_nothing() {
        let outcome = ReloadOutcome {
            ok: true,
            ..ReloadOutcome::default()
        };
        assert_eq!(outcome.summary(), "Reloaded nothing changed.");
    }

    #[test]
    fn a_summary_names_what_applied_and_what_did_not() {
        let outcome = ReloadOutcome {
            ok: true,
            error: None,
            applied: vec!["dictionary".to_owned(), "correction".to_owned()],
            needs_restart: vec!["recognition".to_owned()],
        };
        assert_eq!(
            outcome.summary(),
            "Reloaded dictionary, correction. Restart required for: recognition."
        );
    }
}
