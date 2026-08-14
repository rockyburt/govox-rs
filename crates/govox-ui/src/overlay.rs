//! Driving the on-screen overlay helper.
//!
//! The helper is a **separate process** speaking a newline-delimited text
//! protocol. `govox-py` split it out because GDK is single-backend per process;
//! that reason is gone, but a better one replaces it — the renderer is the
//! least-tested, most crash-prone code in the project, and out-of-process means
//! an overlay crash cannot take dictation down.
//!
//! The wire protocol is kept **byte-identical** to the reference, which makes
//! it the most useful debugging seam in the port: the Rust daemon can drive the
//! *Python* overlay and vice versa. Point [`OverlayClient`] at the Python
//! helper with `GOVOX_OVERLAY_CMD`:
//!
//! ```text
//! GOVOX_OVERLAY_CMD="python3 -m govox.feedback.overlay_app"
//! ```
//!
//! Commands out: `show` `pulse` `hide` `level` `caption` `anchor`
//! `expect-anchor` `caret-marker` `compact` `quit`. In: `stop`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// The protocol is newline-delimited, so a caption can never carry one.
///
/// Ellipsized on the **left** rather than truncated on the right, so the most
/// recently spoken words — the part still changing — stay visible as the
/// caption grows.
const CAPTION_MAX_CHARS: usize = 80;

/// One line of the overlay protocol.
///
/// Rendering is a pure function of the command, which is what lets the wire
/// format be tested without spawning anything.
/// Somewhere overlay commands can be sent.
///
/// The seam exists so the daemon's feedback fan-out can be tested without
/// spawning a helper process. That is not a hypothetical convenience: the
/// fan-out shipped once sending `Caption` and `Level` but never `Show`, which
/// spawns a helper, feeds it a whole session's text, and never maps its
/// window — a bug no test could see while this was a concrete type.
pub trait OverlaySink: Send + Sync {
    fn send(&self, command: &OverlayCommand);
    /// Call `on_stop` each time the user clicks the card.
    ///
    /// Takes the helper's stdout, so it can only be called once, and only
    /// after the helper exists — which is what `prewarm` guarantees.
    fn watch_stops(&self, on_stop: Box<dyn FnMut() + Send>) {
        let _ = on_stop;
    }

    /// Start the helper now, without showing anything.
    ///
    /// The helper is otherwise spawned by the first command, which puts a
    /// process launch, an X11 connection and a fontconfig lookup between the
    /// user pressing their key and the card appearing — once, on the first
    /// session after a restart, which is exactly when someone is watching for
    /// it. Paying that at startup costs an idle process and makes the first
    /// session look like every other one.
    fn prewarm(&self) {}
    /// Stop the helper for good.
    fn shutdown(&self);
}

impl OverlaySink for OverlayClient {
    fn send(&self, command: &OverlayCommand) {
        Self::send(self, command);
    }

    fn prewarm(&self) {
        Self::prewarm(self);
    }

    fn watch_stops(&self, on_stop: Box<dyn FnMut() + Send>) {
        Self::watch_stops(self, on_stop);
    }

    fn shutdown(&self) {
        Self::shutdown(self);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OverlayCommand {
    Show,
    Pulse,
    Hide,
    Level(f32),
    Caption(String),
    /// `None` releases the card back to its configured corner.
    Anchor(Option<(i32, i32, i32, i32)>),
    ExpectAnchor,
    CaretMarker(bool),
    Compact(bool),
    Quit,
}

impl OverlayCommand {
    /// The exact line sent to the helper, without its trailing newline.
    #[must_use]
    pub fn encode(&self) -> String {
        match self {
            Self::Show => "show".to_owned(),
            Self::Pulse => "pulse".to_owned(),
            Self::Hide => "hide".to_owned(),
            Self::Level(value) => format!("level {:.3}", value.clamp(0.0, 1.0)),
            Self::Caption(text) => {
                let text = encode_caption(text);
                if text.is_empty() {
                    "caption".to_owned()
                } else {
                    format!("caption {text}")
                }
            }
            Self::Anchor(None) => "anchor".to_owned(),
            Self::Anchor(Some((x, y, w, h))) => format!("anchor {x} {y} {w} {h}"),
            Self::ExpectAnchor => "expect-anchor".to_owned(),
            Self::CaretMarker(on) => format!("caret-marker {}", u8::from(*on)),
            Self::Compact(on) => format!("compact {}", u8::from(*on)),
            Self::Quit => "quit".to_owned(),
        }
    }
}

/// Collapse whitespace and cap the length, ellipsizing on the left.
///
/// The whitespace collapse is not cosmetic: an embedded newline would
/// desynchronize the stream, and the recogniser can produce one now that
/// spoken "new line" is a mid-utterance command.
///
/// Counted in **characters**, not bytes — `govox-py` uses `len()` on a `str`,
/// and a byte cap would cut a multi-byte character in half and emit invalid
/// UTF-8 to the helper.
#[must_use]
pub fn encode_caption(text: &str) -> String {
    let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= CAPTION_MAX_CHARS {
        return normalized;
    }
    let tail: String = normalized
        .chars()
        .skip(normalized.chars().count() - (CAPTION_MAX_CHARS - 1))
        .collect();
    format!("…{tail}")
}

/// Drives the overlay helper, degrading to a permanent no-op when unavailable.
///
/// The process spawns lazily on the first command, so a session that never
/// shows the overlay never pays for it.
pub struct OverlayClient {
    position: String,
    click_to_stop: bool,
    process: Mutex<Option<Child>>,
    /// Once disabled, stays disabled. A helper that failed to start will fail
    /// again, and retrying per command would spawn a process per utterance.
    disabled: AtomicBool,
}

impl OverlayClient {
    #[must_use]
    pub fn new(position: impl Into<String>, click_to_stop: bool) -> Self {
        Self {
            position: position.into(),
            click_to_stop,
            process: Mutex::new(None),
            disabled: AtomicBool::new(false),
        }
    }

    /// Whether an overlay surface is possible at all.
    ///
    /// XWayland exposes an X11 `DISPLAY`; its absence means no surface.
    #[must_use]
    pub fn available() -> bool {
        std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
    }

    /// Start the helper without sending it anything.
    ///
    /// Idempotent, and silent about failure for the same reason `send` is:
    /// a helper that will not start disables the overlay and never stops
    /// dictation.
    pub fn prewarm(&self) {
        if self.disabled.load(Ordering::Relaxed) {
            return;
        }
        let mut slot = self.process.lock().expect("overlay poisoned");
        if slot.is_none() {
            *slot = self.spawn();
        }
    }

    /// Send one command, spawning the helper if needed.
    ///
    /// Inherent as well as trait method so callers holding a concrete client
    /// need no import.
    pub fn send(&self, command: &OverlayCommand) {
        if self.disabled.load(Ordering::Relaxed) {
            return;
        }
        let mut slot = self.process.lock().expect("overlay poisoned");
        if slot.is_none() {
            match self.spawn() {
                Some(child) => *slot = Some(child),
                None => return,
            }
        }

        let Some(child) = slot.as_mut() else { return };
        let Some(stdin) = child.stdin.as_mut() else {
            return;
        };
        let line = format!("{}\n", command.encode());
        if stdin.write_all(line.as_bytes()).is_err() || stdin.flush().is_err() {
            // The helper died — it could not open a window, most likely. An
            // expected failure mode, so degrade silently rather than raise.
            tracing::warn!("overlay helper unavailable; on-screen indicator disabled");
            self.disabled.store(true, Ordering::Relaxed);
            if let Some(mut child) = slot.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn spawn(&self) -> Option<Child> {
        if !Self::available() {
            tracing::info!("no X11 DISPLAY; on-screen indicator disabled");
            self.disabled.store(true, Ordering::Relaxed);
            return None;
        }

        let (program, mut args) = helper_command();
        args.push("--position".to_owned());
        args.push(self.position.clone());
        if self.click_to_stop {
            args.push("--click-to-stop".to_owned());
        }

        match Command::new(&program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => Some(child),
            Err(error) => {
                tracing::warn!(%error, helper = %program, "overlay helper failed to start");
                self.disabled.store(true, Ordering::Relaxed);
                None
            }
        }
    }

    /// Watch the helper's stdout for `stop`, calling `on_stop` for each.
    ///
    /// Takes the pipe, so it can only be called once. Click-to-stop is the only
    /// thing the helper ever writes.
    pub fn watch_stops<F>(&self, mut on_stop: F)
    where
        F: FnMut() + Send + 'static,
    {
        let Some(stdout) = self
            .process
            .lock()
            .expect("overlay poisoned")
            .as_mut()
            .and_then(|child| child.stdout.take())
        else {
            return;
        };
        std::thread::Builder::new()
            .name("govox-overlay-reader".to_owned())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if line.trim() == "stop" {
                        on_stop();
                    }
                }
            })
            .ok();
    }

    /// Ask the helper to quit, then reap it.
    pub fn shutdown(&self) {
        let child = self.process.lock().expect("overlay poisoned").take();
        self.disabled.store(true, Ordering::Relaxed);
        let Some(mut child) = child else { return };

        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(b"quit\n");
            let _ = stdin.flush();
        }
        // Dropping stdin closes the pipe, which is the helper's other exit
        // signal and unblocks the reader thread.
        drop(child.stdin.take());

        // A helper that ignores `quit` must not hold up the daemon's exit.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                _ => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for OverlayClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The helper to spawn: `$GOVOX_OVERLAY_CMD`, or our own binary.
///
/// The override exists so the Rust daemon can drive the *Python* overlay over
/// the identical protocol, which is how the two implementations are compared
/// side by side.
fn helper_command() -> (String, Vec<String>) {
    resolve_helper(std::env::var_os("GOVOX_OVERLAY_CMD").as_deref())
}

/// The same decision, with the override passed in rather than read.
///
/// Split out because the environment is process-global and cargo runs a
/// binary's tests concurrently: two tests setting `GOVOX_OVERLAY_CMD` around
/// their assertions raced, and one would intermittently read the other's
/// teardown. Taking the value as an argument makes the tests independent of
/// when any other test happens to run.
fn resolve_helper(override_cmd: Option<&std::ffi::OsStr>) -> (String, Vec<String>) {
    if let Some(raw) = override_cmd {
        let raw = raw.to_string_lossy();
        let mut parts = raw.split_whitespace().map(ToOwned::to_owned);
        if let Some(program) = parts.next() {
            return (program, parts.collect());
        }
    }
    // Beside this binary, not on $PATH: a half-installed copy elsewhere would
    // be a confusing thing to pick up.
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("govox-overlay")))
        .filter(|p| p.is_file());
    match sibling {
        Some(path) => (path.to_string_lossy().into_owned(), Vec::new()),
        None => ("govox-overlay".to_owned(), Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_commands_encode_as_bare_words() {
        assert_eq!(OverlayCommand::Show.encode(), "show");
        assert_eq!(OverlayCommand::Pulse.encode(), "pulse");
        assert_eq!(OverlayCommand::Hide.encode(), "hide");
        assert_eq!(OverlayCommand::ExpectAnchor.encode(), "expect-anchor");
        assert_eq!(OverlayCommand::Quit.encode(), "quit");
    }

    #[test]
    fn level_is_three_decimal_places_and_clamped() {
        // The helper parses a fixed format; the clamp is what stops a level
        // computed from a hot microphone rendering as "level 1.734".
        assert_eq!(OverlayCommand::Level(0.5).encode(), "level 0.500");
        assert_eq!(OverlayCommand::Level(0.0).encode(), "level 0.000");
        assert_eq!(OverlayCommand::Level(1.0).encode(), "level 1.000");
        assert_eq!(OverlayCommand::Level(2.5).encode(), "level 1.000");
        assert_eq!(OverlayCommand::Level(-1.0).encode(), "level 0.000");
    }

    #[test]
    fn booleans_encode_as_one_and_zero() {
        assert_eq!(OverlayCommand::CaretMarker(true).encode(), "caret-marker 1");
        assert_eq!(
            OverlayCommand::CaretMarker(false).encode(),
            "caret-marker 0"
        );
        assert_eq!(OverlayCommand::Compact(true).encode(), "compact 1");
        assert_eq!(OverlayCommand::Compact(false).encode(), "compact 0");
    }

    #[test]
    fn an_anchor_carries_four_integers_and_none_releases_it() {
        assert_eq!(
            OverlayCommand::Anchor(Some((10, 20, 30, 40))).encode(),
            "anchor 10 20 30 40"
        );
        // Not "anchor None": the bare word is what returns the card to its
        // corner rather than leaving it stranded.
        assert_eq!(OverlayCommand::Anchor(None).encode(), "anchor");
    }

    #[test]
    fn an_empty_caption_is_the_bare_word() {
        assert_eq!(OverlayCommand::Caption(String::new()).encode(), "caption");
        assert_eq!(
            OverlayCommand::Caption("   ".to_owned()).encode(),
            "caption"
        );
    }

    #[test]
    fn a_caption_can_never_carry_a_newline() {
        // The protocol is newline-delimited, and the recogniser can produce a
        // newline now that spoken "new line" works mid-utterance. One here
        // would desynchronize every command after it.
        let encoded = OverlayCommand::Caption("first\nsecond\r\nthird".to_owned()).encode();
        assert_eq!(encoded, "caption first second third");
        assert!(!encoded.contains('\n'));
        assert!(!encoded.contains('\r'));
    }

    #[test]
    fn whitespace_runs_collapse() {
        assert_eq!(encode_caption("  a   b \t c  "), "a b c");
    }

    #[test]
    fn a_long_caption_is_ellipsized_on_the_left() {
        // The newest words are the ones still changing, so they must survive.
        let text = "abcdefghij".repeat(12); // 120 chars
        let encoded = encode_caption(&text);
        assert_eq!(encoded.chars().count(), CAPTION_MAX_CHARS);
        assert!(encoded.starts_with('…'));
        assert!(encoded.ends_with("abcdefghij"), "the tail must survive");
    }

    #[test]
    fn a_caption_at_exactly_the_limit_is_untouched() {
        let text = "x".repeat(CAPTION_MAX_CHARS);
        assert_eq!(encode_caption(&text), text);
        assert!(!encode_caption(&text).starts_with('…'));
    }

    #[test]
    fn the_cap_counts_characters_not_bytes() {
        // A byte cap would slice a multi-byte character in half and emit
        // invalid UTF-8 down the pipe.
        let text = "é".repeat(120);
        let encoded = encode_caption(&text);
        assert_eq!(encoded.chars().count(), CAPTION_MAX_CHARS);
        assert!(encoded.len() > CAPTION_MAX_CHARS, "these are multi-byte");
    }

    #[test]
    fn an_emoji_caption_is_not_cut_mid_character() {
        let text = "🤷".repeat(120);
        let encoded = encode_caption(&text);
        assert_eq!(encoded.chars().count(), CAPTION_MAX_CHARS);
        assert!(encoded.chars().skip(1).all(|c| c == '🤷'));
    }

    #[test]
    fn an_unspawnable_helper_disables_the_client_rather_than_retrying() {
        // Retrying per command would spawn a process per utterance.
        unsafe { std::env::set_var("GOVOX_OVERLAY_CMD", "/nonexistent/govox-overlay") };
        unsafe { std::env::set_var("DISPLAY", ":0") };

        let client = OverlayClient::new("top-right", false);
        client.send(&OverlayCommand::Show);
        assert!(client.disabled.load(Ordering::Relaxed));

        // And every later command is a no-op rather than another spawn.
        client.send(&OverlayCommand::Hide);
        assert!(client.process.lock().unwrap().is_none());

        unsafe { std::env::remove_var("GOVOX_OVERLAY_CMD") };
    }

    #[test]
    fn the_helper_command_can_be_overridden_for_side_by_side_comparison() {
        // This is what lets the Rust daemon drive the Python overlay, and it
        // is the port's most useful debugging seam — either implementation can
        // be swapped for the other without touching the daemon.
        let (program, args) = resolve_helper(Some(std::ffi::OsStr::new(
            "python3 -m govox.feedback.overlay_app",
        )));
        assert_eq!(program, "python3");
        assert_eq!(args, ["-m", "govox.feedback.overlay_app"]);
    }

    #[test]
    fn an_empty_override_falls_back_rather_than_spawning_nothing() {
        let (program, args) = resolve_helper(Some(std::ffi::OsStr::new("   ")));
        assert!(program.ends_with("govox-overlay"), "{program}");
        assert!(args.is_empty());
    }
}
