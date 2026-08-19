//! The configuration watch, against a real filesystem.
//!
//! The unit tests in `watch.rs` cover which paths are watched and which events
//! matter; neither can tell whether inotify actually delivers. That is what
//! these do, and it is the half that breaks: an editor's atomic save, or a file
//! created after the watch was placed, are exactly the cases a plausible-looking
//! implementation misses.

use std::path::{Path, PathBuf};
use std::time::Duration;

use govox_daemon::daemon::ReloadTrigger;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Long enough to absorb the debounce and a loaded machine, short enough that a
/// genuine failure is a failure rather than a hang.
const PATIENCE: Duration = Duration::from_secs(5);

/// A directory of our own, in the style the rest of the tree uses: no
/// `tempfile` dev-dependency for something this small.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("govox-watch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

async fn next_reload(
    reloads: &mut mpsc::UnboundedReceiver<ReloadTrigger>,
) -> Option<ReloadTrigger> {
    tokio::time::timeout(PATIENCE, reloads.recv())
        .await
        .unwrap_or(None)
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write");
}

/// Save the file the way an editor does: write a neighbour, rename it over.
fn save_atomically(path: &Path, text: &str) {
    let temp = path.with_extension("tmp");
    write(&temp, text);
    std::fs::rename(&temp, path).expect("rename into place");
}

#[tokio::test]
async fn a_plain_write_triggers_a_reload() {
    let dir = scratch("write");
    let config = dir.join("config.toml");
    write(&config, "# empty\n");

    let (tx, mut reloads) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let _watcher = govox_daemon::watch::spawn(std::slice::from_ref(&config), tx, &cancel)
        .expect("a watch on a real dir");

    write(&config, "[correction]\nspoken_punctuation = false\n");

    assert_eq!(
        next_reload(&mut reloads).await,
        Some(ReloadTrigger::FileChanged)
    );
    cancel.cancel();
}

#[tokio::test]
async fn an_atomic_save_triggers_a_reload_every_time() {
    // The regression this whole module exists for: a watch on the *file* would
    // follow the replaced inode and see only the first save.
    let dir = scratch("atomic");
    let dictionary = dir.join("dictionary.toml");
    write(&dictionary, "# empty\n");

    let (tx, mut reloads) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let _watcher = govox_daemon::watch::spawn(std::slice::from_ref(&dictionary), tx, &cancel)
        .expect("a watch");

    for round in 0..3 {
        save_atomically(
            &dictionary,
            &format!("[dictionary]\nbias = [\"round{round}\"]\n"),
        );
        assert_eq!(
            next_reload(&mut reloads).await,
            Some(ReloadTrigger::FileChanged),
            "save {round} was not noticed"
        );
    }
    cancel.cancel();
}

#[tokio::test]
async fn a_file_created_after_the_watch_started_is_noticed() {
    // No `~/.config/govox/dictionary.toml` until the day you write one, and
    // that day is the one where the watch has to already be looking.
    let dir = scratch("created");
    let dictionary = dir.join("dictionary.toml");

    let (tx, mut reloads) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let _watcher = govox_daemon::watch::spawn(std::slice::from_ref(&dictionary), tx, &cancel)
        .expect("a watch");

    write(&dictionary, "[dictionary]\nbias = [\"govox\"]\n");

    assert_eq!(
        next_reload(&mut reloads).await,
        Some(ReloadTrigger::FileChanged)
    );
    cancel.cancel();
}

#[tokio::test]
async fn one_save_is_one_reload() {
    // A save is a burst of events. Without the debounce each would recompile
    // the correction pipeline, and a dictionary edit would cost several.
    let dir = scratch("debounce");
    let config = dir.join("config.toml");
    write(&config, "# empty\n");

    let (tx, mut reloads) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let _watcher =
        govox_daemon::watch::spawn(std::slice::from_ref(&config), tx, &cancel).expect("a watch");

    for line in 0..20 {
        write(&config, &format!("# line {line}\n"));
    }

    assert_eq!(
        next_reload(&mut reloads).await,
        Some(ReloadTrigger::FileChanged)
    );
    // Nothing further: the burst coalesced. Waited out rather than asserted
    // instantly, so a second reload arriving late still fails the test.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        reloads.try_recv().is_err(),
        "one save produced more than one reload"
    );
    cancel.cancel();
}

#[tokio::test]
async fn a_neighbouring_file_is_ignored() {
    let dir = scratch("neighbour");
    let config = dir.join("config.toml");
    write(&config, "# empty\n");

    let (tx, mut reloads) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let _watcher =
        govox_daemon::watch::spawn(std::slice::from_ref(&config), tx, &cancel).expect("a watch");

    write(&dir.join("notes.txt"), "unrelated\n");
    write(&dir.join(".config.toml.swp"), "vim\n");

    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        reloads.try_recv().is_err(),
        "an unrelated file in the same directory triggered a reload"
    );
    cancel.cancel();
}

#[tokio::test]
async fn cancelling_stops_the_watch() {
    let dir = scratch("cancel");
    let config = dir.join("config.toml");
    write(&config, "# empty\n");

    let (tx, mut reloads) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let _watcher =
        govox_daemon::watch::spawn(std::slice::from_ref(&config), tx, &cancel).expect("a watch");
    cancel.cancel();
    // The debounce task observes the cancellation on its next turn.
    tokio::time::sleep(Duration::from_millis(100)).await;

    write(&config, "# edited after shutdown\n");

    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        reloads.try_recv().is_err(),
        "a cancelled watch still asked for a reload"
    );
}
