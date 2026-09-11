//! The packaged systemd unit, read from the repository.
//!
//! A live reboot on 2026-09-11 confirmed that an enabled `cirroved` comes back at
//! the next login and stops cleanly -- see
//! `docs/benchmarks/service-lifecycle-and-suspend.json`. That run cannot be repeated
//! by CI: it ends the session that would be watching it. What CI can do is hold the
//! unit to the wiring that run depended on, so that the clause fails here rather than
//! on a user's next login.
//!
//! This asserts the unit, not the behaviour. It cannot show that the service starts
//! at login; only a login can. It shows that the install wiring which makes that
//! happen is still present and still user-relative.
#![allow(clippy::unwrap_used)]
use std::collections::HashMap;

const UNIT: &str = "packaging/systemd/cirroved.service";

/// `key=value` per section, with later duplicates kept in order.
fn sections(text: &str) -> HashMap<String, Vec<(String, String)>> {
    let mut parsed: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_string();
            parsed.entry(section.clone()).or_default();
            continue;
        }
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        parsed
            .entry(section.clone())
            .or_default()
            .push((key.trim().to_string(), value.trim().to_string()));
    }
    parsed
}

fn unit() -> HashMap<String, Vec<(String, String)>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(UNIT);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    sections(&text)
}

fn value(
    unit: &HashMap<String, Vec<(String, String)>>,
    section: &str,
    key: &str,
) -> Option<String> {
    unit.get(section)?
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

/// The clause itself. `systemctl --user enable` reads `[Install]`; without
/// `WantedBy=default.target` it links nothing, the unit is still "enabled" to a
/// casual check, and nothing starts at the next login.
#[test]
fn enabling_the_unit_wires_it_into_the_login_session() {
    let unit = unit();
    assert_eq!(
        value(&unit, "Install", "WantedBy").as_deref(),
        Some("default.target"),
        "[Install] WantedBy=default.target is what makes an enabled unit start at login"
    );
}

/// The live run proves this unit works for the user who ran it. Specifiers are what
/// make that evidence transfer: `%h` and `%t` resolve per user at start. An absolute
/// path under one developer's home would pass a reboot on that machine and start
/// nothing anywhere else.
#[test]
fn the_service_paths_are_user_relative_rather_than_one_developers_home() {
    let unit = unit();
    let exec = value(&unit, "Service", "ExecStart").expect("ExecStart");
    for expected in [
        "%h/.local/bin/cirroved",
        "--state-dir %h/.local/state/cirrove",
        "--socket %t/cirrove/control.sock",
    ] {
        assert!(
            exec.contains(expected),
            "ExecStart lost {expected:?}; it reads {exec:?}"
        );
    }
    assert!(
        !exec.contains("/home/"),
        "ExecStart names an absolute home directory, which only starts for one user: {exec:?}"
    );

    // systemd creates and owns the socket's parent before the daemon starts.
    assert_eq!(
        value(&unit, "Service", "RuntimeDirectory").as_deref(),
        Some("cirrove"),
        "RuntimeDirectory=cirrove creates %t/cirrove for the control socket"
    );
    assert_eq!(
        value(&unit, "Service", "RuntimeDirectoryMode").as_deref(),
        Some("0700"),
        "the control socket's directory must not be readable by other users"
    );
}

/// The reboot stopped the daemon cleanly: it logged its own unmount and exited in
/// 38 ms. `TimeoutStopSec` is the budget it gets to do that before SIGKILL, which
/// would leave the mount behind for the next login to trip over.
#[test]
fn a_stop_leaves_time_to_unmount_before_it_is_killed() {
    let unit = unit();
    let timeout = value(&unit, "Service", "TimeoutStopSec").expect("TimeoutStopSec");
    let seconds: u64 = timeout
        .strip_suffix('s')
        .unwrap_or(&timeout)
        .parse()
        .unwrap_or_else(|_| panic!("TimeoutStopSec is not a plain number of seconds: {timeout:?}"));
    assert!(
        seconds >= 10,
        "TimeoutStopSec={seconds}s is too short to unmount and flush before SIGKILL"
    );
}

/// The unit carries a comment explaining this, and a comment does not fail a build.
/// `fusermount3` is setuid; it needs its privilege transition to mount for an
/// unprivileged user. `NoNewPrivileges=yes` looks like ordinary hardening and would
/// stop the mount on a typical desktop -- the failure appears at the user's next
/// login, not here, unless something asserts it.
#[test]
fn hardening_does_not_block_the_setuid_mount_helper() {
    let unit = unit();
    let setting = value(&unit, "Service", "NoNewPrivileges").unwrap_or_default();
    assert!(
        !matches!(
            setting.to_ascii_lowercase().as_str(),
            "yes" | "true" | "1" | "on"
        ),
        "NoNewPrivileges={setting:?} prevents fusermount3 from mounting for an unprivileged user"
    );
}
