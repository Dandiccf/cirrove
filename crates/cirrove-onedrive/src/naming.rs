//! The names OneDrive will not take, decided before anything is written.
//!
//! A name the cloud refuses used to be accepted by the mount, journalled,
//! uploaded, refused by Graph, and left as a change the daemon had given up
//! on -- an hour after the application that made it had moved on. The rules
//! are known; applying them at the mount turns that into `EINVAL` at the
//! moment of creation, which an application shows as a dialog and a person
//! can act on.
//!
//! The rules are Microsoft's for OneDrive and SharePoint: the characters
//! `" * : < > ? / \ |`, leading or trailing spaces, a trailing period, the
//! names `.lock`, `CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`,
//! `_vti_`, `desktop.ini`, a `~$` prefix, `_vti_` anywhere, and 255 characters
//! at most for a name. Where the rule is a limit the answer is `TooLong`; for
//! everything else it names the offending part, so a journal line or a test
//! failure says what was wrong rather than only that something was.
use cirrove_core::NameProblem;

const FORBIDDEN: &[char] = &['"', '*', ':', '<', '>', '?', '/', '\\', '|'];
const RESERVED: &[&str] = &[".lock", "desktop.ini", "_vti_"];
const RESERVED_DEVICES: &[&str] = &["CON", "PRN", "AUX", "NUL"];

/// Why OneDrive would refuse this name, or `None` if it would not.
pub fn name_problem(name: &str) -> Option<NameProblem> {
    if name.is_empty() {
        return Some(NameProblem::Invalid("empty name"));
    }
    if name.chars().count() > 255 {
        return Some(NameProblem::TooLong);
    }
    if let Some(c) = name
        .chars()
        .find(|c| FORBIDDEN.contains(c) || c.is_control())
    {
        return Some(NameProblem::Invalid(if c.is_control() {
            "a control character"
        } else {
            match c {
                '"' => "a double quote",
                '*' => "an asterisk",
                ':' => "a colon",
                '<' => "a less-than sign",
                '>' => "a greater-than sign",
                '?' => "a question mark",
                '/' => "a slash",
                '\\' => "a backslash",
                _ => "a vertical bar",
            }
        }));
    }
    if name.starts_with(' ') || name.ends_with(' ') {
        return Some(NameProblem::Invalid("a leading or trailing space"));
    }
    if name.ends_with('.') {
        return Some(NameProblem::Invalid("a trailing period"));
    }
    if name.starts_with("~$") {
        return Some(NameProblem::Invalid(
            "the ~$ prefix Office uses for lock files",
        ));
    }
    if name.contains("_vti_") {
        return Some(NameProblem::Invalid("_vti_, which SharePoint reserves"));
    }
    let lower = name.to_ascii_lowercase();
    if RESERVED.iter().any(|r| lower == *r) {
        return Some(NameProblem::Invalid("a name OneDrive reserves"));
    }
    // Device names are reserved with or without an extension: CON.txt is CON.
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if RESERVED_DEVICES.contains(&stem.as_str()) {
        return Some(NameProblem::Invalid("a Windows device name"));
    }
    if stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit()
    {
        return Some(NameProblem::Invalid("a Windows device name"));
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_pass_including_the_awkward_but_legal_ones() {
        for name in [
            "Report.docx",
            "Übersicht 2026.xlsx",
            "a.b.c",
            ".hidden",
            "con sole.txt",
            "COMMON",
            "COM10",
            "lpt",
            "notes (final) [v2] {draft}",
            "emoji 📎",
            "_vti",
            "x".repeat(255).as_str(),
        ] {
            assert_eq!(
                name_problem(name),
                None,
                "{name:?} is a legal OneDrive name"
            );
        }
    }

    #[test]
    fn every_rule_names_what_it_refuses() {
        let cases: &[(&str, &str)] = &[
            ("", "empty"),
            ("a:b", "colon"),
            ("a|b", "vertical bar"),
            ("what?", "question mark"),
            ("\"quoted\"", "double quote"),
            ("back\\slash", "backslash"),
            (" leading", "space"),
            ("trailing ", "space"),
            ("dots.", "period"),
            ("~$Report.docx", "~$"),
            ("dir_vti_bin", "_vti_"),
            (".lock", "reserves"),
            ("Desktop.INI", "reserves"),
            ("CON", "device"),
            ("con.txt", "device"),
            ("COM1", "device"),
            ("lpt9.log", "device"),
            ("tab\there", "control"),
        ];
        for (name, word) in cases {
            match name_problem(name) {
                Some(NameProblem::Invalid(reason)) => {
                    assert!(
                        reason.contains(word),
                        "{name:?}: {reason:?} should mention {word:?}"
                    );
                }
                other => panic!("{name:?}: expected a named refusal, got {other:?}"),
            }
        }
        assert_eq!(name_problem(&"x".repeat(256)), Some(NameProblem::TooLong));
    }
}
