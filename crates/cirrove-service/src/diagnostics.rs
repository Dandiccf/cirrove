//! A diagnostics bundle a person can paste somewhere without first reading
//! every line of it for their own name.
//!
//! What the daemon knows -- `status` -- and what it did -- its journal -- are
//! what a support question needs, and both are full of the things a user must
//! not share by accident: the account they signed in with, the tenant, the
//! folders they chose, the names of their files. This replaces those and keeps
//! the rest: states, counts, timings, and the labels the user chose for their
//! connections.
//!
//! Replacement, not deletion, and stable: the same value becomes the same token
//! everywhere in the bundle, so "the same tenant on both accounts" or "the
//! same item in three journal lines" still reads, without the value itself.
//! The token carries the first eight hex characters of a SHA-256 of the value,
//! which correlates and does not reveal.
//!
//! Free text -- error messages, journal lines -- is scanned for the shapes
//! that carry identity: mail addresses, absolute paths, URLs, quoted strings,
//! provider item ids, GUIDs. Anything that matches goes. This is a filter on
//! shapes, not an understanding of meaning, and the bundle says so at the
//! top: read it before sharing it.
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Keys whose whole value is an identity and is replaced with a token.
const IDENTITY_KEYS: &[&str] = &[
    "account",
    "tenant",
    "drive",
    "mount_path",
    "drive_id",
    "root_id",
    "account_id",
    "item",
    "collection",
];

/// `<key:hash8>` for a value: the same value, the same token.
pub fn token(kind: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("<{kind}:{}>", &hex::encode(digest)[..8])
}

/// Replace identities in a status document. Structure, counts, states and
/// user-chosen labels stay; free-text values are scanned like journal text.
pub fn redact_status(value: &Value) -> Value {
    redact_value(value, None)
}

fn redact_value(value: &Value, key: Option<&str>) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact_value(v, Some(k))))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|v| redact_value(v, key)).collect()),
        Value::String(text) => match key {
            Some(k) if IDENTITY_KEYS.contains(&k) => Value::String(token(k, text)),
            // A label is the user's own word for the connection and is what
            // every command names it by; it stays.
            Some("label") => Value::String(text.clone()),
            _ => Value::String(redact_text(text)),
        },
        other => other.clone(),
    }
}

/// Replace, in free text, everything shaped like an identity.
pub fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        // Quoted strings first: a path inside quotes is one replacement, not
        // a quoted "<path>".
        if let Some(stripped) = rest.strip_prefix('"')
            && let Some(end) = stripped.find('"')
        {
            out.push_str("\"<redacted>\"");
            rest = &stripped[end + 1..];
            continue;
        }
        if let Some(stripped) = rest.strip_prefix('`')
            && let Some(end) = stripped.find('`')
        {
            out.push_str("`<redacted>`");
            rest = &stripped[end + 1..];
            continue;
        }
        let word_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        if word_end == 0 {
            let c = rest.chars().next().unwrap_or(' ');
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let word = &rest[..word_end];
        out.push_str(&redact_word(word));
        rest = &rest[word_end..];
    }
    out
}

/// One whitespace-delimited word. Trailing punctuation is kept outside the
/// replacement so a sentence still reads.
fn redact_word(word: &str) -> String {
    let trimmed = word.trim_end_matches(['.', ',', ';', ':', ')', ']', '}']);
    let tail = &word[trimmed.len()..];
    let (lead, core) = match trimmed.find(|c: char| !matches!(c, '(' | '[' | '{')) {
        Some(index) => (&trimmed[..index], &trimmed[index..]),
        None => ("", trimmed),
    };
    // A `key=value` pair: the key is vocabulary, the value may not be.
    if let Some((key, value)) = core.split_once('=')
        && !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return format!("{lead}{key}={}{tail}", redact_word(value));
    }
    let replacement = if let Some((scheme, after)) = core.split_once("://")
        && matches!(scheme, "http" | "https")
    {
        let host = after.split('/').next().unwrap_or("");
        Some(format!("<url:{host}>"))
    } else if (core.starts_with('/') && core.len() > 1) || core.starts_with("~/") {
        Some("<path>".to_owned())
    } else if is_mail_address(core) {
        Some("<email>".to_owned())
    } else if is_guid(core) {
        Some(token("id", core))
    } else if is_item_id(core) {
        Some(token("item", core))
    } else if looks_like_a_secret(core) {
        Some("<secret>".to_owned())
    } else {
        None
    };
    match replacement {
        Some(text) => format!("{lead}{text}{tail}"),
        None => word.to_owned(),
    }
}

/// A bearer token or anything else shaped like one.
///
/// The bundle exists to be sent to someone else, so the redaction is the last
/// thing standing between a journal line and a stranger. It was not catching
/// tokens at all: a JWT went through `redact_text` unchanged, which was found
/// by asking rather than by reading, after a real access and refresh token were
/// printed to a terminal by a command that helpfully includes secrets.
///
/// Two shapes. A JWT is three base64url runs separated by dots and begins
/// `eyJ`, which is `{"` encoded -- that is unmistakable and cheap to spot. And
/// a long unbroken base64url run with no structure is a refresh token, a client
/// secret or a session id; nothing a person needs to read in a diagnostics
/// bundle looks like that. Item ids are caught earlier and by name, so they
/// keep their stable pseudonym rather than becoming `<secret>`.
fn looks_like_a_secret(word: &str) -> bool {
    let base64url = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '=')
    };
    let parts: Vec<&str> = word.split('.').collect();
    if parts.len() == 3 && parts[0].starts_with("eyJ") && parts.iter().all(|p| base64url(p)) {
        return true;
    }
    // Long enough that no ordinary word, path fragment or version reaches it,
    // and mixed enough that a run of hex or a hostname does not.
    word.len() >= 40
        && base64url(word)
        && word.chars().any(|c| c.is_ascii_uppercase())
        && word.chars().any(|c| c.is_ascii_lowercase())
        && word.chars().any(|c| c.is_ascii_digit())
}

fn is_mail_address(word: &str) -> bool {
    let Some((local, domain)) = word.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn is_guid(word: &str) -> bool {
    let parts: Vec<&str> = word.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(len, part)| part.len() == *len && part.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The opaque identifiers a provider hands out: item ids, and the id of the
/// drive itself.
///
/// Two shapes, because they do not look alike. An item id is a long run of
/// upper-case letters and digits, sometimes with a `!`. A drive id is a longer
/// base64-ish run, mixed case, with `-` and `_` and a leading `b!` -- so the
/// upper-case test walked straight past it, and the id of the drive a person
/// signed into reached a bundle meant for pasting into a support thread. It
/// arrives through free text, in journal lines like `collection=b!...`; the
/// status document was never affected, because there it is a named key.
///
/// The second shape is deliberately narrow: at least 32 characters, only
/// base64url characters, and all three of upper case, lower case and a digit.
/// No word of any language reaches that, and neither do the state names,
/// module paths and version strings a bundle is read for.
fn is_item_id(word: &str) -> bool {
    is_upper_case_item_id(word) || is_opaque_drive_id(word)
}

fn is_upper_case_item_id(word: &str) -> bool {
    word.len() >= 24
        && word
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '!')
        && word.chars().any(|c| c.is_ascii_digit())
        && word.chars().any(|c| c.is_ascii_uppercase())
}

fn is_opaque_drive_id(word: &str) -> bool {
    // A `b!` prefix is Microsoft's; strip any short prefix ending in `!` so the
    // run itself is what is measured.
    let core = match word.split_once('!') {
        Some((prefix, rest)) if prefix.len() <= 2 => rest,
        _ => word,
    };
    core.len() >= 32
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && core.chars().any(|c| c.is_ascii_uppercase())
        && core.chars().any(|c| c.is_ascii_lowercase())
        && core.chars().any(|c| c.is_ascii_digit())
}

/// The bundle's text.
/// `hostname` is replaced wherever it appears: the journal names the machine
/// on every line, and a machine's name is often a person's.
pub fn bundle(
    cli_version: &str,
    kernel: &str,
    hostname: &str,
    status: Option<&Value>,
    status_error: Option<&str>,
    journal: &str,
    since: &str,
) -> String {
    let mut out = String::new();
    out.push_str("# Cirrove diagnostics\n\n");
    out.push_str(
        "Read this before sharing it. Account names, tenants, folders, file names, \
         addresses and provider ids are replaced by tokens (the same value gives the same \
         token, so things still correlate); what remains is state, counts and timing. \
         The labels you chose for your connections are kept. The replacement works on \
         the shapes of things, not their meaning: a name that looks like an ordinary \
         word is not caught.\n\n",
    );
    out.push_str("## Versions\n\n");
    out.push_str(&format!("cirrove (command line): {cli_version}\n"));
    match status {
        Some(status) => out.push_str(&format!(
            "cirroved: {} (status protocol {})\n",
            status.get("version").and_then(Value::as_str).unwrap_or("?"),
            status
                .get("protocol_version")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        )),
        None => out.push_str("cirroved: not reachable\n"),
    }
    out.push_str(&format!("kernel: {kernel}\n\n"));
    out.push_str("## Status\n\n");
    match (status, status_error) {
        (Some(status), _) => {
            out.push_str(&serde_json::to_string_pretty(&redact_status(status)).unwrap_or_default());
            out.push('\n');
        }
        (None, Some(error)) => out.push_str(&format!("not available: {}\n", redact_text(error))),
        (None, None) => out.push_str("not available\n"),
    }
    out.push_str(&format!(
        "\n## Journal of cirroved.service, last {since}\n\n"
    ));
    if journal.trim().is_empty() {
        out.push_str("(nothing, or journalctl is not available)\n");
    } else {
        for line in journal.lines() {
            let line = if hostname.is_empty() {
                redact_text(line)
            } else {
                redact_text(&line.replace(hostname, "<host>"))
            };
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn free_text_loses_every_shape_that_carries_identity_and_keeps_the_rest() {
        let line = "2026-09-13T09:20:00 cirroved[1]: refresh failed for alex.langer@example.com \
                    at /home/alex/Cloud/work path=/home/alex/x item 01YQR2QYORVS6W3KEVUBD37GQP2RNIPFRA \
                    tenant 00000000-0000-4000-8000-000000000002 via https://graph.microsoft.com/v1.0/me \
                    named \"Quarterly report.docx\" (retry in 30s).";
        let redacted = redact_text(line);
        for gone in [
            "alex",
            "example.com",
            "/home/",
            "Quarterly",
            "01YQR2QY",
            "00000000-0000-4000-8000-000000000002",
            "/v1.0/me",
        ] {
            assert!(
                !redacted.contains(gone),
                "{gone:?} survived in {redacted:?}"
            );
        }
        for kept in [
            "2026-09-13T09:20:00",
            "refresh failed for",
            "<email>",
            "at <path>",
            "path=<path>",
            "<url:graph.microsoft.com>",
            "\"<redacted>\"",
            "(retry in 30s).",
        ] {
            assert!(
                redacted.contains(kept),
                "{kept:?} missing from {redacted:?}"
            );
        }
        assert!(redacted.contains("<item:"), "the item id becomes a token");
        assert!(redacted.contains("<id:"), "the tenant guid becomes a token");
    }

    #[test]
    fn the_same_value_gives_the_same_token_and_a_different_one_does_not() {
        assert_eq!(token("tenant", "a"), token("tenant", "a"));
        assert_ne!(token("tenant", "a"), token("tenant", "b"));
        assert!(token("tenant", "a").starts_with("<tenant:"));
    }

    #[test]
    fn a_status_keeps_its_shape_states_and_labels_and_loses_its_identities() {
        let status = serde_json::json!({
            "version": "0.1.0-dev",
            "protocol_version": 1,
            "accounts": [{
                "label": "work",
                "account": "alex@example.com",
                "tenant": "t-1",
                "drive": "Work documents",
                "mount_path": "/home/alex/Cloud/work",
                "state": "ready",
                "stuck_changes": 2,
                "feeds": [{"collection": "drive-1", "state": "ready",
                           "message": "throttled at /home/alex/Cloud/work/x"}],
                "pins": [{"item": "01ABC", "reserved": 10}]
            }, {
                "label": "personal",
                "account": "alex@example.com",
                "tenant": "t-1",
                "drive": "Personal",
                "mount_path": "/home/alex/Cloud/personal",
                "state": "disabled",
                "stuck_changes": 0,
                "feeds": [],
                "pins": []
            }]
        });
        let redacted = redact_status(&status);
        let text = redacted.to_string();
        for gone in [
            "alex",
            "example.com",
            "Work documents",
            "/home/",
            "drive-1",
            "01ABC",
            "t-1",
        ] {
            assert!(!text.contains(gone), "{gone:?} survived: {text}");
        }
        let accounts = redacted["accounts"].as_array().unwrap();
        assert_eq!(accounts[0]["label"], "work");
        assert_eq!(accounts[0]["state"], "ready");
        assert_eq!(accounts[0]["stuck_changes"], 2);
        assert_eq!(
            accounts[0]["account"], accounts[1]["account"],
            "the same account on both connections still reads as the same"
        );
        assert_ne!(accounts[0]["mount_path"], accounts[1]["mount_path"]);
        assert!(
            accounts[0]["feeds"][0]["message"]
                .as_str()
                .unwrap()
                .contains("throttled at <path>")
        );
        assert_eq!(
            redacted["version"], "0.1.0-dev",
            "versions are not identities"
        );
    }

    #[test]
    fn the_bundle_says_to_read_it_first_and_survives_a_missing_daemon() {
        let text = bundle(
            "0.1.0",
            "6.1",
            "box",
            None,
            Some("Cirrove service is not reachable"),
            "",
            "2h",
        );
        assert!(text.starts_with("# Cirrove diagnostics"));
        assert!(text.contains("Read this before sharing it."));
        assert!(text.contains("cirroved: not reachable"));
        assert!(text.contains("not available: Cirrove service is not reachable"));
        assert!(text.contains("(nothing, or journalctl is not available)"));
    }

    #[test]
    fn a_drive_id_in_a_journal_line_does_not_reach_the_bundle() {
        // The exact shape the engine logs when a collection stops updating.
        // A drive id is not an item id: it is mixed case with `-` and `_`, so
        // the upper-case-and-digits test that catches item ids walks past it,
        // and the id of the drive a person signed into went into a bundle
        // meant for pasting into a support thread.
        let drive = "b!xyhYD-ysPUSe1frzb1g0BEulqeqG8e9FlOQCLouOohVALpTibMRRQ53REotC5rqV";
        let journal = format!(
            "2026-09-13T13:44:44 host cirroved[1029]: WARN cirrove_service::engine: \
             a collection stopped updating collection={drive} state=sign_in_required\n"
        );
        let text = bundle("0.1.0", "6.1", "host", None, None, &journal, "2h");
        assert!(
            !text.contains(drive),
            "the drive id survived into the bundle:\n{text}"
        );
        // What the line is about must still read.
        assert!(text.contains("a collection stopped updating"));
        assert!(text.contains("state=sign_in_required"));
        assert!(text.contains("collection=<item:"));
    }

    /// The bundle is made to be sent to someone else, so this is the last
    /// thing between a journal line and a stranger -- and it was not catching
    /// tokens at all. Found by asking rather than by reading.
    #[test]
    fn a_token_does_not_survive_redaction() {
        let jwt = "eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJodHRwczovL2dyYXBoLm1pY3Jvc29mdC5jb20ifQ.SGVsbG9fdGhlcmVfZnJpZW5k";
        let out = super::redact_text(&format!("token was {jwt} in a log line"));
        assert!(!out.contains(jwt), "a JWT must not reach a bundle: {out}");
        assert!(
            out.contains("<secret>"),
            "and it must be visibly removed: {out}"
        );

        // A refresh token has no structure at all: one long opaque run.
        let refresh = "1AYEAhiBgzo1x8U2blpU4Fh7cBOhsw6zlzBMlK3brKwFMu0AAFOBAAbQABAwEAAAADAOzf";
        let out = super::redact_text(&format!("refresh_token={refresh}"));
        assert!(
            !out.contains(refresh),
            "an opaque secret must not reach a bundle: {out}"
        );

        // And the redaction must not eat the things a bundle is for.
        let kept = super::redact_text(
            "cirroved 0.1.0-dev state=ready feeds=2 stuck_changes=0 ENOSPC writable-preview",
        );
        for word in [
            "0.1.0-dev",
            "state=ready",
            "feeds=2",
            "ENOSPC",
            "writable-preview",
        ] {
            assert!(kept.contains(word), "{word} should survive: {kept}");
        }
    }

    #[test]
    fn ordinary_words_and_the_things_a_bundle_is_for_are_not_redacted() {
        // The shape test must not eat the vocabulary a reader needs.
        for keep in [
            "sign_in_required",
            "VerifyRequired",
            "cirrove_service::engine",
            "writable-preview",
            "0.1.0-dev",
            "quickXorHash",
            "unsupported database schema version",
            "stuck_changes=0",
            "internationalization",
        ] {
            assert_eq!(
                redact_text(keep),
                keep,
                "{keep:?} is vocabulary a bundle needs and must survive"
            );
        }
    }

    #[test]
    fn the_machines_name_is_not_in_the_journal_lines() {
        let journal = "2026-09-13T11:16:29+02:00 alexbox systemd[1]: Stopping Cirrove cloud filesystem service...\n";
        let text = bundle("0.1.0", "6.1", "alexbox", None, None, journal, "2h");
        assert!(!text.contains("alexbox"));
        assert!(text.contains("<host> systemd[1]: Stopping Cirrove"));
    }
}
