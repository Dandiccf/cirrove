//! What `cirrove --help` tells a person to run.
//!
//! A doc comment in the subcommand enum is the help text, and a missing one is
//! not a compile error: the command simply appears in the list with nothing
//! beside it, and the comment meant for it lands on whichever command comes
//! next. That happened. `diagnose` advertised itself as the way to abandon
//! changes the daemon gave up on -- which is what `discard-stuck` does -- and
//! `discard-stuck` had no description at all, so the one command that fixes a
//! stuck change was the one command the help would not explain. Someone
//! following the help would have written a diagnostics file and wondered why
//! nothing was fixed.
#![allow(clippy::unwrap_used)]
use std::process::Command;

fn help() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_cirrove"))
        .arg("--help")
        .output()
        .expect("the CLI binary runs");
    assert!(out.status.success(), "--help exits zero");
    String::from_utf8(out.stdout).expect("help is UTF-8")
}

/// The command list, as `name -> description`. Continuation lines of a wrapped
/// description are folded into the command they belong to.
fn commands(text: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("Commands:") {
            inside = true;
            continue;
        }
        if inside && (line.starts_with("Options:") || line.starts_with("Arguments:")) {
            break;
        }
        if !inside || line.trim().is_empty() {
            continue;
        }
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        // clap indents commands by two and wraps their text far to the right.
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let first = parts.next().unwrap_or_default();
        let rest = parts.next().unwrap_or_default().trim().to_string();
        let looks_like_a_command =
            indent <= 4 && !first.is_empty() && first.chars().all(|c| c.is_ascii_lowercase() || c == '-');
        if looks_like_a_command {
            found.push((first.to_string(), rest));
        } else if let Some(last) = found.last_mut() {
            last.1.push(' ');
            last.1.push_str(trimmed.trim());
        }
    }
    found
}

#[test]
fn every_command_the_help_lists_says_what_it_does() {
    let text = help();
    let listed = commands(&text);
    assert!(listed.len() > 10, "the help lists the commands: {listed:?}");
    let silent: Vec<&String> = listed
        .iter()
        .filter(|(_, about)| about.trim().is_empty())
        .map(|(name, _)| name)
        .collect();
    assert!(
        silent.is_empty(),
        "these commands appear in --help with no description: {silent:?}"
    );
}

#[test]
fn discard_stuck_is_the_one_that_abandons_changes_and_diagnose_is_not() {
    let text = help();
    let listed = commands(&text);
    let about = |name: &str| -> String {
        listed
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name} is in --help"))
            .1
            .to_lowercase()
    };

    let discard = about("discard-stuck");
    assert!(
        discard.contains("abandon") || discard.contains("discard") || discard.contains("give"),
        "discard-stuck must say that it abandons the changes the daemon gave up on, not {discard:?}"
    );

    let diagnose = about("diagnose");
    assert!(
        diagnose.contains("diagnost") || diagnose.contains("bundle"),
        "diagnose must say it writes a diagnostics bundle, not {diagnose:?}"
    );
    assert!(
        !diagnose.contains("abandon"),
        "diagnose must not claim to abandon anything; that is discard-stuck: {diagnose:?}"
    );
}
