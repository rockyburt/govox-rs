//! Noticing that a configuration file changed, without being asked.
//!
//! The daemon could already re-read `config.toml` and `dictionary.toml` — see
//! [`crate::daemon::Daemon::reload`] — but only when the tray's Reload item was
//! clicked. Adding a word to the personal dictionary is an edit-and-try loop,
//! and a step between "save" and "try" that lives in a menu is a step that gets
//! forgotten: the next utterance comes out unchanged and the dictionary looks
//! broken rather than merely unloaded.
//!
//! So the files are watched, and a save *is* the reload. Three details make
//! that work rather than merely appear to:
//!
//! 1. **Directories are watched, not files.** Editors save by writing a
//!    temporary file and renaming it into place. An inotify watch follows the
//!    inode, so a file-level watch survives exactly one save and then silently
//!    observes an unlinked inode forever. Watching the parent directory sees
//!    the rename, and sees the file being *created* for the first time — which
//!    matters, since neither file has to exist when the daemon starts.
//! 2. **Events are debounced.** One save can produce a create, several writes
//!    and a rename, and each one alone would recompile the correction pipeline.
//! 3. **A reload that changed nothing stays quiet.** A manual reload always
//!    reports, because someone asked and deserves an answer. An automatic one
//!    fires on every save of a watched file, including a save that only moved a
//!    comment — announcing those would train the user to ignore the
//!    notification that tells them a restart is needed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::daemon::ReloadTrigger;
use govox_core::config::{Config, Environment};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// How long to wait after the first event before reloading.
///
/// Long enough to coalesce one editor's save into one reload, short enough that
/// the reload lands before you have finished switching windows to try it.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Keeps the filesystem watch alive.
///
/// `notify` stops watching when the watcher is dropped, so the pipeline holds
/// this for as long as it runs. Dropping it early leaves a daemon that looks
/// watched and is not.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

/// Every file a reload re-reads, resolved to an absolute path.
///
/// The explicit `--config` path is included because it is the file this run
/// started from, and the XDG path because it is the one edited when `--config`
/// was not passed. Both are listed even when absent: a file that does not exist
/// yet is exactly the one whose creation should be noticed.
#[must_use]
pub fn watched_paths(config: &Config, explicit: Option<&Path>, env: &Environment) -> Vec<PathBuf> {
    let home = env.home();
    let mut paths: Vec<PathBuf> = Vec::new();

    if let Some(user) = env.user_config_path() {
        paths.push(user);
    }
    if let Some(explicit) = explicit {
        paths.push(explicit.to_path_buf());
    }
    let dictionary = config.correction.dictionary_path.trim();
    if !dictionary.is_empty() {
        paths.push(govox_core::domain::expand_user(
            Path::new(dictionary),
            home.as_deref(),
        ));
    }

    // Only a path with a parent can be watched, and only once each: two entries
    // naming the same file would reload twice per save.
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| path.parent().is_some())
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

/// Whether an event on a watched directory concerns one of `watched`.
///
/// Access events are ignored — reading `config.toml` is not a reason to reload
/// it — and so is anything naming another file in the same directory, which for
/// `~/.config/govox` includes every editor swap file written beside the one
/// being edited.
fn concerns(kind: EventKind, event_paths: &[PathBuf], watched: &HashSet<PathBuf>) -> bool {
    let interesting = matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    );
    interesting && event_paths.iter().any(|path| watched.contains(path))
}

/// Watch `paths` and send on `reloads` when one of them changes.
///
/// Returns `None` when no watch could be established. That is a degraded
/// daemon, not a broken one — the tray's Reload still works — so it is logged
/// and startup continues, as every other optional layer here does.
#[must_use]
pub fn spawn(
    paths: &[PathBuf],
    reloads: mpsc::UnboundedSender<ReloadTrigger>,
    cancel: &CancellationToken,
) -> Option<ConfigWatcher> {
    if paths.is_empty() {
        return None;
    }
    let watched: HashSet<PathBuf> = paths.iter().cloned().collect();

    let (hits, mut pending) = mpsc::unbounded_channel::<()>();
    let mut watcher = match notify::recommended_watcher(move |event| {
        let Ok(notify::Event { kind, paths, .. }) = event else {
            return;
        };
        if concerns(kind, &paths, &watched) {
            // The receiving task outlives the watcher, so a failure here means
            // the daemon is already stopping. Nothing to report.
            let _ = hits.send(());
        }
    }) {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::warn!(%error, "cannot watch the configuration files; use the tray to reload");
            return None;
        }
    };

    // Parents, deduplicated: `config.toml` and `dictionary.toml` normally share
    // `~/.config/govox`, and watching it twice delivers every event twice.
    let mut watching = 0usize;
    let mut parents: HashSet<&Path> = HashSet::new();
    for parent in paths.iter().filter_map(|path| path.parent()) {
        if !parents.insert(parent) {
            continue;
        }
        match watcher.watch(parent, RecursiveMode::NonRecursive) {
            Ok(()) => watching += 1,
            // Expected when the directory does not exist — there is no
            // `~/.config/govox` on a machine running entirely on defaults.
            // Debug, not warn: nothing here is the user's to fix.
            Err(error) => tracing::debug!(dir = %parent.display(), %error, "not watching"),
        }
    }
    if watching == 0 {
        return None;
    }
    tracing::info!(files = paths.len(), "watching the configuration for edits");

    let cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                hit = pending.recv() => {
                    if hit.is_none() {
                        break;
                    }
                }
            }
            // Let the rest of the save land, then treat everything it produced
            // as the one change it was.
            tokio::time::sleep(DEBOUNCE).await;
            while pending.try_recv().is_ok() {}
            if reloads.send(ReloadTrigger::FileChanged).is_err() {
                break;
            }
        }
    });

    Some(ConfigWatcher { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind};

    fn config_with_dictionary(path: &str) -> Config {
        let mut config = Config::load_from(None, &Environment::default()).expect("defaults");
        config.correction.dictionary_path = path.to_owned();
        config
    }

    fn env() -> Environment {
        Environment::from_pairs([("HOME", "/home/example")])
    }

    #[test]
    fn watches_the_user_config_and_the_dictionary() {
        let config = config_with_dictionary("~/.config/govox/dictionary.toml");
        assert_eq!(
            watched_paths(&config, None, &env()),
            vec![
                PathBuf::from("/home/example/.config/govox/config.toml"),
                PathBuf::from("/home/example/.config/govox/dictionary.toml"),
            ]
        );
    }

    #[test]
    fn watches_an_explicit_config_as_well_as_the_default_one() {
        let explicit = PathBuf::from("/etc/govox/config.toml");
        assert_eq!(
            watched_paths(&config_with_dictionary(""), Some(&explicit), &env()),
            vec![
                PathBuf::from("/home/example/.config/govox/config.toml"),
                explicit,
            ]
        );
    }

    #[test]
    fn lists_a_file_named_twice_only_once() {
        let explicit = PathBuf::from("/home/example/.config/govox/config.toml");
        assert_eq!(
            watched_paths(&config_with_dictionary(""), Some(&explicit), &env()),
            vec![explicit]
        );
    }

    #[test]
    fn an_empty_dictionary_path_is_not_watched() {
        assert_eq!(
            watched_paths(&config_with_dictionary("   "), None, &env()),
            vec![PathBuf::from("/home/example/.config/govox/config.toml")]
        );
    }

    fn watched() -> HashSet<PathBuf> {
        [PathBuf::from("/c/govox/config.toml")]
            .into_iter()
            .collect()
    }

    #[test]
    fn a_write_to_a_watched_file_concerns_us() {
        assert!(concerns(
            EventKind::Modify(ModifyKind::Any),
            &[PathBuf::from("/c/govox/config.toml")],
            &watched(),
        ));
    }

    #[test]
    fn a_rename_into_place_concerns_us() {
        // What an editor's atomic save looks like from the directory, and the
        // reason the watch is on the directory rather than the file.
        assert!(concerns(
            EventKind::Create(CreateKind::File),
            &[PathBuf::from("/c/govox/config.toml")],
            &watched(),
        ));
    }

    #[test]
    fn a_neighbouring_swap_file_does_not() {
        assert!(!concerns(
            EventKind::Modify(ModifyKind::Any),
            &[PathBuf::from("/c/govox/.config.toml.swp")],
            &watched(),
        ));
    }

    #[test]
    fn merely_reading_the_file_does_not() {
        assert!(!concerns(
            EventKind::Access(AccessKind::Read),
            &[PathBuf::from("/c/govox/config.toml")],
            &watched(),
        ));
    }
}
