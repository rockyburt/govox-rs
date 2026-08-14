//! The one place `govox-input` is allowed to spawn a process.
//!
//! Every injector talks to the outside world through [`Runner`] and nothing
//! else. That is what `govox-py` does with its injected `Runner` callable, and
//! the reason is the same: the interesting assertions in this module are about
//! *exact argv*, and a test that has to install `ydotool` to make them is a test
//! that does not run.

use std::process::{Command, Stdio};

/// What a command did. Only the parts any caller reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandResult {
    pub returncode: i32,
    pub stderr: String,
}

impl CommandResult {
    #[must_use]
    pub fn ok() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn failed(stderr: impl Into<String>) -> Self {
        Self {
            returncode: 1,
            stderr: stderr.into(),
        }
    }

    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.returncode == 0
    }
}

/// Runs a command, optionally writing `stdin` to it.
///
/// `Sync` because injectors are shared across tasks; see
/// [`Injector`](govox_core::domain::Injector).
pub trait Runner: Send + Sync {
    fn run(&self, command: &[String], stdin: Option<&str>) -> CommandResult;
}

/// So one runner can back both injectors behind the fallback wrapper, the way
/// `govox-py` passes the same callable to each.
impl<T: Runner + ?Sized> Runner for std::sync::Arc<T> {
    fn run(&self, command: &[String], stdin: Option<&str>) -> CommandResult {
        (**self).run(command, stdin)
    }
}

/// Actually spawns the process.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&self, command: &[String], stdin: Option<&str>) -> CommandResult {
        let Some((program, args)) = command.split_first() else {
            return CommandResult::failed("empty command");
        };

        let mut child = match Command::new(program)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            // A missing binary is an ordinary outcome here, not a panic: the
            // selector's whole job is to fall back when one is unavailable.
            Err(err) => return CommandResult::failed(format!("{program}: {err}")),
        };

        if let Some(text) = stdin
            && let Some(mut pipe) = child.stdin.take()
        {
            use std::io::Write as _;
            // A broken pipe means the child exited early; its status and
            // stderr below describe that better than this error would.
            let _ = pipe.write_all(text.as_bytes());
        }

        match child.wait_with_output() {
            Ok(output) => CommandResult {
                returncode: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Err(err) => CommandResult::failed(format!("{program}: {err}")),
        }
    }
}

/// A [`Runner`] that records calls instead of making them.
///
/// Lives in the library rather than in `tests/` so the daemon's own tests can
/// use it too, exactly as `govox-py`'s hand-written recording fakes are shared
/// across test modules.
#[derive(Debug, Default)]
pub struct RecordingRunner {
    calls: std::sync::Mutex<Vec<(Vec<String>, Option<String>)>>,
    /// Make the Nth call (1-based) fail. Used to drive the fallback path.
    fail_nth: Option<usize>,
}

impl RecordingRunner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A runner whose first call fails, as `govox-py`'s `fail_first` does.
    #[must_use]
    pub fn failing_first() -> Self {
        Self {
            fail_nth: Some(1),
            ..Self::default()
        }
    }

    /// Every call so far, as `(argv, stdin)`.
    #[must_use]
    pub fn calls(&self) -> Vec<(Vec<String>, Option<String>)> {
        self.calls
            .lock()
            .expect("recording runner poisoned")
            .clone()
    }
}

impl Runner for RecordingRunner {
    fn run(&self, command: &[String], stdin: Option<&str>) -> CommandResult {
        let mut calls = self.calls.lock().expect("recording runner poisoned");
        calls.push((command.to_vec(), stdin.map(ToOwned::to_owned)));
        if self.fail_nth == Some(calls.len()) {
            return CommandResult::failed("rejected");
        }
        CommandResult::ok()
    }
}
