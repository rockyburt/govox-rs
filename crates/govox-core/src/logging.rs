//! Turning the `[logging]` section into a `tracing` filter.
//!
//! The reference configures Python's `logging` module, whose vocabulary this
//! section still speaks: `WARNING` rather than `WARN`, dotted logger names
//! like `govox.daemon`, and a root level distinct from govox's own. None of
//! that maps to `tracing` by itself, which is why the section sat parsed and
//! unread — a configured `DEBUG` produced no debug output, and nothing said so.
//!
//! The translation is here, as a pure function, because it is the part worth
//! testing: a filter string that is subtly wrong logs nothing and looks
//! exactly like a filter string that is right.

use std::fmt::Write as _;

use crate::config::LoggingConfig;

/// Python level names, as a `tracing` level.
///
/// `NOTSET` means "inherit" in Python and has no `tracing` equivalent, so it
/// yields `None` and the caller omits the directive — which is inheriting.
#[must_use]
pub fn tracing_level(level: &str) -> Option<&'static str> {
    match level.trim().to_uppercase().as_str() {
        "CRITICAL" | "FATAL" | "ERROR" => Some("error"),
        "WARN" | "WARNING" => Some("warn"),
        "INFO" => Some("info"),
        "DEBUG" => Some("debug"),
        _ => None,
    }
}

/// A dotted logger name as a `tracing` target.
///
/// `govox.daemon` is one crate, `govox_daemon`; anything beyond that is a
/// module path within it. So the first dot becomes an underscore and the rest
/// become `::` — `govox.daemon.pipeline` is `govox_daemon::pipeline`.
///
/// A name with no dots is passed through, so `govox_ime::session` can be
/// written directly by anyone who thinks in Rust paths rather than Python
/// logger names.
#[must_use]
pub fn tracing_target(logger: &str) -> String {
    let logger = logger.trim();
    let Some((crate_name, rest)) = logger.split_once('.') else {
        return logger.to_owned();
    };
    let rest = rest.replace('.', "::");
    format!("{crate_name}_{rest}")
}

/// The `EnvFilter` directive string for this configuration.
///
/// `root_level` is the bare global default; `level` applies to everything
/// govox logs, matched by prefix so it covers every `govox_*` crate; and each
/// entry in `loggers` narrows further.
#[must_use]
pub fn filter_directives(config: &LoggingConfig) -> String {
    let mut directives = String::new();
    if let Some(root) = tracing_level(&config.root_level) {
        directives.push_str(root);
    }
    if let Some(level) = tracing_level(&config.level) {
        if !directives.is_empty() {
            directives.push(',');
        }
        let _ = write!(directives, "govox={level}");
    }
    for (logger, level) in &config.loggers {
        let Some(level) = tracing_level(level) else {
            continue;
        };
        if !directives.is_empty() {
            directives.push(',');
        }
        let _ = write!(directives, "{}={level}", tracing_target(logger));
    }
    directives
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LoggingConfig {
        LoggingConfig::default()
    }

    #[test]
    fn python_level_names_become_tracing_levels() {
        // WARNING is the one that matters: it is the shipped default for the
        // root logger, and `tracing` does not accept it.
        assert_eq!(tracing_level("WARNING"), Some("warn"));
        assert_eq!(tracing_level("warning"), Some("warn"));
        assert_eq!(tracing_level("CRITICAL"), Some("error"));
        assert_eq!(tracing_level("FATAL"), Some("error"));
        assert_eq!(tracing_level("DEBUG"), Some("debug"));
        assert_eq!(tracing_level("INFO"), Some("info"));
    }

    #[test]
    fn notset_is_omitted_rather_than_guessed() {
        // "inherit" has no directive; leaving it out is what inheriting means.
        assert_eq!(tracing_level("NOTSET"), None);
        assert_eq!(tracing_level("nonsense"), None);
    }

    #[test]
    fn dotted_logger_names_become_crate_paths() {
        assert_eq!(tracing_target("govox.daemon"), "govox_daemon");
        assert_eq!(tracing_target("govox.ime"), "govox_ime");
        // Beyond the crate, dots are module separators.
        assert_eq!(
            tracing_target("govox.daemon.pipeline"),
            "govox_daemon::pipeline"
        );
    }

    #[test]
    fn rust_style_targets_are_left_alone() {
        assert_eq!(tracing_target("govox"), "govox");
        assert_eq!(tracing_target("govox_ime::session"), "govox_ime::session");
    }

    #[test]
    fn the_defaults_produce_the_filter_that_shipped() {
        // root WARNING + govox INFO, which is what the hardcoded
        // "govox=info,warn" meant before this section was read at all.
        assert_eq!(filter_directives(&config()), "warn,govox=info");
    }

    #[test]
    fn per_logger_overrides_are_appended() {
        let mut config = config();
        config
            .loggers
            .insert("govox.daemon".to_owned(), "DEBUG".to_owned());
        config
            .loggers
            .insert("govox.ime".to_owned(), "DEBUG".to_owned());
        assert_eq!(
            filter_directives(&config),
            "warn,govox=info,govox_daemon=debug,govox_ime=debug"
        );
    }

    #[test]
    fn an_unusable_logger_level_is_skipped_not_fatal() {
        // Validation rejects these at load, so reaching here means a level
        // that validates but has no equivalent. Dropping one directive beats
        // refusing to log at all.
        let mut config = config();
        config
            .loggers
            .insert("govox.ime".to_owned(), "NOTSET".to_owned());
        assert_eq!(filter_directives(&config), "warn,govox=info");
    }
}
