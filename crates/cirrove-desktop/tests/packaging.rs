//! The packaged autostart entry and tray unit, read from the repository.
//!
//! Two mechanisms start a tray on Linux and neither covers every desktop. An XDG
//! autostart entry is understood by GNOME, KDE, XFCE, LXQt, Cinnamon and MATE,
//! is bridged into systemd by `systemd-xdg-autostart-generator` where that
//! exists, and works on distributions with no systemd at all. A systemd user
//! unit gives the right lifecycle but only where something actually reaches
//! `graphical-session.target`. So both ship, and the binary is what makes that
//! safe: it refuses to be the second copy.
//!
//! None of this can be decided when a package is installed. A package is
//! installed once, system-wide, before any session exists, and the same machine
//! can have several users on different desktops. `docs/distribution.md` says as
//! much -- package scripts may not assume a desktop, a shell, a home directory
//! or a session bus -- so the intelligence lives in the binary and these files
//! stay dumb and standard. What is assertable here is that they stayed that way.
#![allow(clippy::unwrap_used)]
use std::collections::HashMap;

fn repo(relative: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {relative}: {e}"))
}

/// `key=value` per section, later duplicates kept in order.
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

fn value(
    parsed: &HashMap<String, Vec<(String, String)>>,
    section: &str,
    key: &str,
) -> Option<String> {
    parsed
        .get(section)?
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

fn autostart() -> HashMap<String, Vec<(String, String)>> {
    sections(&repo(
        "packaging/desktop/io.github.Dandiccf.Cirrove.Tray.desktop",
    ))
}

fn unit() -> HashMap<String, Vec<(String, String)>> {
    sections(&repo("packaging/systemd/cirrove-tray.service"))
}

#[test]
fn the_autostart_entry_names_no_desktop_it_would_have_to_guess() {
    // OnlyShowIn/NotShowIn would decide at packaging time which desktops get a
    // tray, and that answer is wrong for the next desktop and for a user who
    // switches between logins. The binary tolerates a missing host instead.
    let parsed = autostart();
    assert!(
        value(&parsed, "Desktop Entry", "OnlyShowIn").is_none(),
        "OnlyShowIn excludes desktops this cannot know about"
    );
    assert!(
        value(&parsed, "Desktop Entry", "NotShowIn").is_none(),
        "NotShowIn excludes desktops this cannot know about"
    );
}

#[test]
fn the_autostart_entry_is_an_autostart_and_not_a_launcher_item() {
    let parsed = autostart();
    assert_eq!(
        value(&parsed, "Desktop Entry", "NoDisplay").as_deref(),
        Some("true"),
        "a tray in the application menu is one more thing to explain"
    );
    assert_eq!(
        value(&parsed, "Desktop Entry", "Type").as_deref(),
        Some("Application")
    );
}

#[test]
fn the_autostart_entry_runs_the_tray_off_the_path() {
    // An absolute path would bake in one prefix and break every distribution
    // that installs somewhere else. distribution.md requires standard locations.
    let parsed = autostart();
    let exec = value(&parsed, "Desktop Entry", "Exec").expect("no Exec");
    assert_eq!(exec, "cirrove-tray", "Exec must not carry a prefix: {exec}");
}

#[test]
fn the_unit_follows_the_graphical_session_rather_than_the_login() {
    // The daemon holds a mount and must survive without a session; the tray has
    // nothing to draw on without one. Same machine, opposite lifetimes, which is
    // the whole reason this is a second unit.
    let parsed = unit();
    assert_eq!(
        value(&parsed, "Install", "WantedBy").as_deref(),
        Some("graphical-session.target"),
        "a tray wired to default.target would start with no session to draw on"
    );
    assert_eq!(
        value(&parsed, "Unit", "PartOf").as_deref(),
        Some("graphical-session.target"),
        "without PartOf the tray outlives the session that gave it a panel"
    );
}

#[test]
fn the_unit_is_user_relative_rather_than_one_developers_home() {
    let parsed = unit();
    let exec = value(&parsed, "Service", "ExecStart").expect("no ExecStart");
    assert!(
        exec.starts_with("%h/"),
        "ExecStart must use the %h specifier, not a literal home: {exec}"
    );
    assert!(
        !exec.contains("/home/"),
        "a literal home directory ships one developer's machine: {exec}"
    );
}

#[test]
fn the_unit_does_not_restart_a_deliberate_exit() {
    // Exit 0 is what a second copy does when the first already owns the bus
    // name, which is the ordinary outcome when both mechanisms fire. Restarting
    // that is a loop.
    let parsed = unit();
    let restart = value(&parsed, "Service", "Restart").unwrap_or_default();
    assert_eq!(
        restart, "on-failure",
        "Restart=always would respawn the copy that correctly stood down"
    );
}

#[test]
fn both_mechanisms_start_the_same_command() {
    // If these drift, one mechanism silently starts something else.
    let exec = value(&autostart(), "Desktop Entry", "Exec").expect("no Exec");
    let unit_exec = value(&unit(), "Service", "ExecStart").expect("no ExecStart");
    assert!(
        unit_exec.ends_with(&exec),
        "autostart runs {exec:?} but the unit runs {unit_exec:?}"
    );
}
