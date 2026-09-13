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
    //
    // What this buys and what it costs, learned from a login that produced no
    // tray: `systemd-xdg-autostart-generator` resolves `Exec=` against its own
    // PATH, which at session start does not include ~/.local/bin. A bare name is
    // therefore right for a packaged install in /usr/bin -- limine-restore-notify
    // sits in the same directory with a bare Exec and generates its unit fine --
    // and silently wrong for a user-local one: the generator logs "executable
    // specified in Exec= does not exist" and writes no unit at all. No error
    // reaches the user; the tray simply never appears.
    //
    // So this assertion is about the packaged file and stays. The gap it leaves
    // belongs to installation, and docs/desktop.md now says a user-local install
    // must rewrite Exec= to an absolute path.
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

/// The application id the window registers, and everything that must agree
/// with it.
const APP_ID: &str = "io.github.Dandiccf.Cirrove";

fn metainfo() -> String {
    repo("packaging/metainfo/io.github.Dandiccf.Cirrove.metainfo.xml")
}

/// One value inside an XML element, by naive scan. Enough for a metainfo file
/// this project writes itself; a real parser would be a dependency for a test.
fn element(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}");
    let start = xml.find(&open)?;
    let after = xml[start..].find('>')? + start + 1;
    let end = xml[after..].find(&format!("</{name}>"))? + after;
    Some(xml[after..end].trim().to_string())
}

#[test]
fn the_desktop_entry_icon_and_metainfo_share_one_application_identity() {
    // Milestone 5: one identity across the desktop entry, the installed icon,
    // the AppStream metainfo, the D-Bus name and the Wayland app_id. This holds
    // the static half -- the files agree with the id the code registers. The
    // runtime half, that a shell associates the window with its launcher, needs
    // a session and is the session-matrix row.
    let parsed = autostart_main();
    assert_eq!(
        value(&parsed, "Desktop Entry", "Icon").as_deref(),
        Some(APP_ID),
        "the launcher icon must be the application, not a generic folder"
    );
    let xml = metainfo();
    assert_eq!(element(&xml, "id").as_deref(), Some(APP_ID));
    assert_eq!(
        element(&xml, "launchable").as_deref(),
        Some("io.github.Dandiccf.Cirrove.desktop"),
        "AppStream must point at the desktop entry it describes"
    );
    // The source of truth for the id is the window's registration and the
    // tray's bus name, read from the code rather than restated here.
    let main_rs = repo("crates/cirrove-desktop/src/main.rs");
    assert!(
        main_rs.contains(&format!("application_id(\"{APP_ID}\")")),
        "main.rs registers a different application id"
    );
    let tray_rs = repo("crates/cirrove-desktop/src/tray/mod.rs");
    assert!(
        tray_rs.contains(&format!("\"{APP_ID}.Tray\"")),
        "the tray's bus name must be under the application id"
    );
}

fn autostart_main() -> HashMap<String, Vec<(String, String)>> {
    sections(&repo(
        "packaging/desktop/io.github.Dandiccf.Cirrove.desktop",
    ))
}

#[test]
fn every_icon_the_tray_names_ships_as_a_file() {
    // The first tray registered, declared its properties, reported
    // NeedsAttention, and drew nothing, because the icon it named did not exist.
    // Naming an icon is a promise that a file is installed under that name; this
    // reads the names out of the tray and checks the promise is kept in
    // packaging, so a renamed icon fails here rather than on someone's panel.
    let tray_rs = repo("crates/cirrove-desktop/src/tray/mod.rs");
    // Only what the tray itself names. Its own tests write fixture files under
    // names a previous build used, to prove those are cleared away, and those
    // are the opposite of a promise that something is installed.
    let tray_rs = match tray_rs.find("#[cfg(test)]") {
        Some(end) => tray_rs[..end].to_string(),
        None => tray_rs,
    };
    let mut named = Vec::new();
    for line in tray_rs.lines() {
        if let Some(start) = line.find(&format!("\"{APP_ID}-")) {
            let rest = &line[start + 1..];
            if let Some(end) = rest.find('"') {
                named.push(rest[..end].to_string());
            }
        }
    }
    assert!(
        !named.is_empty(),
        "the tray names no branded icons; it is still on generic freedesktop names"
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in named {
        // A colored icon ships under scalable/apps, a symbolic one under
        // symbolic/apps; the tray's are colored, but accept either so the
        // check is about the promise being kept, not where.
        let scalable = format!("packaging/icons/scalable/apps/{name}.svg");
        let symbolic = format!("packaging/icons/symbolic/apps/{name}.svg");
        assert!(
            root.join(&scalable).exists() || root.join(&symbolic).exists(),
            "the tray names {name} and nothing ships it at {scalable} or {symbolic}"
        );
    }
}
