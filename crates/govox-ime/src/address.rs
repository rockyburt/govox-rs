//! Finding the running `ibus-daemon`.
//!
//! IBus does not use the session bus. It runs a private one whose address is
//! written to `$XDG_CONFIG_HOME/ibus/bus/<machine-id>-<display>`, and `IBus.Bus()`
//! hides all of this — which is why `govox-py` has no equivalent of this module.
//!
//! The part that matters, established in the M-1(b) spike: **stale files for
//! dead daemons are left behind**. This machine had three and exactly one was
//! live. Taking the first file found connects to a dead socket, so every
//! candidate is checked for liveness through the `IBUS_DAEMON_PID` it records.

use std::path::{Path, PathBuf};

use crate::ImeError;

/// Where `ibus-daemon` is listening, and how that was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusAddress {
    /// The D-Bus address to connect to.
    pub address: String,
    /// The file it came from, or `None` when `$IBUS_ADDRESS` supplied it.
    pub source: Option<PathBuf>,
}

/// Resolve the address of the live daemon.
///
/// `$IBUS_ADDRESS` wins when it is set: it is how a session deliberately points
/// clients at a specific daemon, and second-guessing it would break that.
pub fn discover() -> Result<BusAddress, ImeError> {
    if let Ok(address) = std::env::var("IBUS_ADDRESS")
        && !address.is_empty()
    {
        return Ok(BusAddress {
            address,
            source: None,
        });
    }

    let dir = bus_dir()?;
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|error| ImeError::NoDaemon(format!("cannot read {}: {error}", dir.display())))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    // Sorted so that a machine with several live daemons picks the same one on
    // every start. Readdir order is not stable and an input method that lands
    // somewhere different each boot is impossible to reason about.
    candidates.sort();

    let mut stale = 0_usize;
    for path in &candidates {
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(entry) = parse(&body) else { continue };
        if !is_running(entry.pid) {
            stale += 1;
            tracing::debug!(path = %path.display(), pid = entry.pid, "skipping a stale ibus bus file");
            continue;
        }
        return Ok(BusAddress {
            address: entry.address,
            source: Some(path.clone()),
        });
    }

    Err(ImeError::NoDaemon(format!(
        "no live ibus-daemon in {} ({} file(s), {stale} stale)",
        dir.display(),
        candidates.len()
    )))
}

/// One parsed bus file.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    address: String,
    pid: i32,
}

/// Read `IBUS_ADDRESS` and `IBUS_DAEMON_PID` out of a bus file.
///
/// The format is `KEY=value` lines with `#` comments. Both keys are required:
/// an address without a pid cannot be checked for liveness, and connecting to
/// an unverified socket is exactly the failure this module exists to prevent.
fn parse(body: &str) -> Option<Entry> {
    let mut address = None;
    let mut pid = None;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "IBUS_ADDRESS" => address = Some(value.trim().to_owned()),
            "IBUS_DAEMON_PID" => pid = value.trim().parse::<i32>().ok(),
            _ => {}
        }
    }
    Some(Entry {
        address: address?,
        pid: pid?,
    })
}

/// Is that pid still alive?
///
/// `/proc` rather than `kill(pid, 0)`: no signal is sent, nothing needs the
/// permission to send one, and the answer is the same.
fn is_running(pid: i32) -> bool {
    pid > 0 && Path::new(&format!("/proc/{pid}")).exists()
}

/// `$XDG_CONFIG_HOME/ibus/bus`, with the usual `~/.config` fallback.
fn bus_dir() -> Result<PathBuf, ImeError> {
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let home = std::env::var("HOME").map_err(|_| {
                ImeError::NoDaemon("neither XDG_CONFIG_HOME nor HOME is set".into())
            })?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(base.join("ibus").join("bus"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_address_and_pid_out_of_a_bus_file() {
        let body = "# This file is created by ibus-daemon, do not modify it.\n\
                    IBUS_ADDRESS=unix:path=/run/user/1000/ibus/dbus-AbCdEf,guid=deadbeef\n\
                    IBUS_DAEMON_PID=9928\n";
        assert_eq!(
            parse(body),
            Some(Entry {
                address: "unix:path=/run/user/1000/ibus/dbus-AbCdEf,guid=deadbeef".into(),
                pid: 9928,
            })
        );
    }

    #[test]
    fn a_file_without_a_pid_is_unusable() {
        // Not "usable but unverified": the whole point of reading the file is
        // to find out whether the socket behind it is alive.
        assert_eq!(parse("IBUS_ADDRESS=unix:path=/tmp/x\n"), None);
    }

    #[test]
    fn a_file_without_an_address_is_unusable() {
        assert_eq!(parse("IBUS_DAEMON_PID=1\n"), None);
    }

    #[test]
    fn the_addresss_guid_suffix_is_kept_verbatim() {
        // The guid is part of the address, not decoration — dropping it makes
        // the connection fail authentication.
        let entry = parse("IBUS_ADDRESS=unix:abstract=/tmp/x,guid=ff\nIBUS_DAEMON_PID=1\n");
        assert_eq!(entry.unwrap().address, "unix:abstract=/tmp/x,guid=ff");
    }

    #[test]
    fn pid_one_is_not_assumed_dead_but_a_nonsense_pid_is() {
        // pid 1 always exists; 0 and negatives are not pids at all, and a
        // truncated file that parsed as one must not be treated as live.
        assert!(is_running(1));
        assert!(!is_running(0));
        assert!(!is_running(-9928));
    }
}
