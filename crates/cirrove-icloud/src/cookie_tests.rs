//! Synthetic cookie persistence controls; no Apple requests or wall-clock sleeps.
use super::*;

const SOURCE: &str = "https://setup.icloud.com/setup/ws/1/accountLogin";
const PAST: i64 = 1_000_000;
const FUTURE: i64 = 4_000_000_000;

fn dated_cookie(source: &str, value: &str, at: i64, expires: Option<i64>) -> serde_json::Value {
    json!({
        "source": source,
        "set_cookie": value,
        "lifetime": {"received_at_unix": at, "expires_at_unix": expires},
    })
}

fn restore_cookies(values: Vec<serde_json::Value>) -> Result<RecordingCookies> {
    let store = RecordingCookies::default();
    store.restore(serde_json::from_value(json!(values))?)?;
    Ok(store)
}

fn cookie_pairs(store: &RecordingCookies, url: &str) -> Result<Vec<String>> {
    let mut pairs = store
        .cookies(&Url::parse(url)?)
        .map(|header| {
            header
                .to_str()
                .expect("synthetic cookie pairs are ASCII")
                .split("; ")
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    pairs.sort();
    Ok(pairs)
}

#[test]
fn saved_cookie_elapsed_max_age_is_not_restarted_by_restore() -> Result<()> {
    // On the original implementation serde ignores the lifetime, and Jar makes
    // this already-expired cookie live for another 60 seconds. This is the RED.
    let mut session = ICloudReadSession::new()?;
    session.account_hash = Some(account_hash("synthetic@example.com")?);
    session.headers.session_token = "synthetic-session-token".into();
    session.drive_endpoint = Some(checked_drive_endpoint("https://p01-drivews.icloud.com/")?);
    session.docs_endpoint = Some(checked_drive_endpoint("https://p01-docws.icloud.com/")?);
    let mut snapshot: serde_json::Value =
        serde_json::from_str(session.session_snapshot()?.expose_secret())?;
    snapshot["cookies"] = json!([dated_cookie(
        SOURCE,
        "synthetic=expired; Max-Age=60; Path=/; Secure; HttpOnly",
        PAST,
        Some(PAST + 60),
    )]);
    let snapshot = SecretString::from(serde_json::to_string(&snapshot)?);
    let restored = ICloudReadSession::from_session_snapshot(&snapshot, "synthetic@example.com")?;
    assert!(cookie_pairs(&restored.cookies, SOURCE)?.is_empty());
    Ok(())
}

#[test]
fn saved_cookie_repeated_snapshots_retain_original_deadline() -> Result<()> {
    let value = dated_cookie(
        SOURCE,
        "synthetic=future; Max-Age=60; Path=/; Secure",
        FUTURE,
        Some(FUTURE + 60),
    );
    let first = restore_cookies(vec![value.clone()])?;
    let original_records = serde_json::to_value(first.records()?)?;
    assert_eq!(original_records[0]["lifetime"], value["lifetime"]);
    let second = RecordingCookies::default();
    second.restore(serde_json::from_value(original_records.clone())?)?;
    let third = RecordingCookies::default();
    third.restore(serde_json::from_value(serde_json::to_value(
        second.records()?,
    )?)?)?;
    assert_eq!(serde_json::to_value(third.records()?)?, original_records);
    assert_eq!(cookie_pairs(&third, SOURCE)?, ["synthetic=future"]);
    Ok(())
}

#[test]
fn saved_cookie_capture_records_receipt_and_max_age_precedence() -> Result<()> {
    let store = RecordingCookies::default();
    let source = Url::parse(SOURCE)?;
    let header = HeaderValue::from_static(
        "synthetic=fresh; Max-Age=60; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/; Secure",
    );
    let before = cookie::time::OffsetDateTime::now_utc().unix_timestamp();
    store.set_cookies(&mut [&header].into_iter(), &source);
    let after = cookie::time::OffsetDateTime::now_utc().unix_timestamp();
    let records = serde_json::to_value(store.records()?)?;
    let lifetime = &records[0]["lifetime"];
    let received = lifetime["received_at_unix"]
        .as_i64()
        .expect("a freshly received cookie records its receipt second");
    assert!((before..=after).contains(&received));
    assert_eq!(lifetime["expires_at_unix"], received + 60);
    assert!(lifetime.get("restored_at_unix").is_none());
    assert_eq!(cookie_pairs(&store, SOURCE)?, ["synthetic=fresh"]);
    Ok(())
}

#[test]
fn saved_cookie_expired_update_deletes_prior_cookie_without_affecting_other_paths() -> Result<()> {
    let store = restore_cookies(vec![
        dated_cookie(SOURCE, "same=old; Path=/; Secure", PAST, None),
        dated_cookie(SOURCE, "same=other; Path=/setup; Secure", PAST, None),
        dated_cookie(
            SOURCE,
            "same=expired-update; Max-Age=60; Path=/; Secure",
            PAST,
            Some(PAST + 60),
        ),
    ])?;
    assert_eq!(cookie_pairs(&store, SOURCE)?, ["same=other"]);
    assert!(cookie_pairs(&store, "https://setup.icloud.com/elsewhere")?.is_empty());
    Ok(())
}

#[test]
fn saved_cookie_zero_and_negative_max_age_delete_despite_future_expires() -> Result<()> {
    for age in ["0", "-60"] {
        let store = restore_cookies(vec![
            dated_cookie(SOURCE, "same=old; Path=/; Secure", PAST, None),
            dated_cookie(
                SOURCE,
                &format!(
                    "same=deleted; Max-Age={age}; Expires=Fri, 01 Jan 2100 00:00:00 GMT; Path=/; Secure"
                ),
                PAST,
                Some(0),
            ),
        ])?;
        assert!(cookie_pairs(&store, SOURCE)?.is_empty());
    }
    Ok(())
}

#[test]
fn saved_cookie_replay_preserves_domain_host_only_path_and_update_order() -> Result<()> {
    let store = restore_cookies(vec![
        dated_cookie(
            SOURCE,
            "shared=old; Domain=.icloud.com; Path=/; Secure; Max-Age=60",
            FUTURE,
            Some(FUTURE + 60),
        ),
        dated_cookie(
            SOURCE,
            "shared=new; Domain=.icloud.com; Path=/; Secure; Max-Age=120",
            FUTURE,
            Some(FUTURE + 120),
        ),
        dated_cookie(SOURCE, "local=host; Path=/; Secure", PAST, None),
        dated_cookie(
            SOURCE,
            "shared=expired-other-path; Domain=.icloud.com; Path=/other; Secure; Max-Age=60",
            PAST,
            Some(PAST + 60),
        ),
    ])?;
    assert_eq!(cookie_pairs(&store, SOURCE)?, ["local=host", "shared=new"]);
    assert_eq!(
        cookie_pairs(&store, "https://www.icloud.com/other/file")?,
        ["shared=new"]
    );
    assert!(cookie_pairs(&store, "https://idmsa.apple.com/")?.is_empty());
    Ok(())
}

#[test]
fn saved_cookie_absolute_expiration_and_session_cookie_are_preserved() -> Result<()> {
    let store = restore_cookies(vec![
        dated_cookie(
            SOURCE,
            "past=gone; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/; Secure",
            PAST,
            Some(0),
        ),
        dated_cookie(
            SOURCE,
            "future=kept; Expires=Fri, 01 Jan 2100 00:00:00 GMT; Path=/; Secure",
            PAST,
            Some(4_102_444_800),
        ),
        dated_cookie(SOURCE, "session=kept; Path=/; Secure", PAST, None),
    ])?;
    assert_eq!(
        cookie_pairs(&store, SOURCE)?,
        ["future=kept", "session=kept"]
    );
    Ok(())
}

#[test]
fn saved_cookie_inconsistent_or_ambiguous_lifetime_is_refused() -> Result<()> {
    let valid = dated_cookie(SOURCE, "synthetic=value; Max-Age=60", PAST, Some(PAST + 60));
    let mut wrong_expiration = valid.clone();
    wrong_expiration["lifetime"]["expires_at_unix"] = (PAST + 61).into();
    assert!(restore_cookies(vec![wrong_expiration]).is_err());
    let mut two_anchors = valid.clone();
    two_anchors["lifetime"]["restored_at_unix"] = PAST.into();
    assert!(restore_cookies(vec![two_anchors]).is_err());
    let mut no_anchor = valid;
    no_anchor["lifetime"]
        .as_object_mut()
        .expect("synthetic lifetime is an object")
        .remove("received_at_unix");
    assert!(restore_cookies(vec![no_anchor]).is_err());
    Ok(())
}

#[test]
fn saved_cookie_legacy_anchor_is_honest_and_retained_after_explicit_snapshot() -> Result<()> {
    let legacy = json!({
        "source": SOURCE,
        "set_cookie": "legacy=kept; Max-Age=60; Path=/; Secure",
    });
    let first = restore_cookies(vec![legacy])?;
    assert_eq!(cookie_pairs(&first, SOURCE)?, ["legacy=kept"]);
    let saved = serde_json::to_value(first.records()?)?;
    let lifetime = &saved[0]["lifetime"];
    assert!(lifetime.get("received_at_unix").is_none());
    let restored_at = lifetime["restored_at_unix"]
        .as_i64()
        .expect("legacy adoption records restoration, not original receipt");
    assert_eq!(lifetime["expires_at_unix"], restored_at + 60);
    let second = RecordingCookies::default();
    second.restore(serde_json::from_value(saved.clone())?)?;
    assert_eq!(serde_json::to_value(second.records()?)?, saved);
    Ok(())
}

#[test]
fn saved_cookie_exact_expiry_boundary_has_no_grace_or_reanchor() -> Result<()> {
    let record = dated_cookie(
        SOURCE,
        "boundary=value; Max-Age=60; Path=/; Secure",
        FUTURE,
        Some(FUTURE + 60),
    );
    for (now, expected) in [
        (FUTURE + 59, true),
        (FUTURE + 60, false),
        (FUTURE + 61, false),
    ] {
        let store = RecordingCookies::default();
        store.restore_at(serde_json::from_value(json!([record.clone()]))?, now)?;
        assert_eq!(!cookie_pairs(&store, SOURCE)?.is_empty(), expected);
        assert_eq!(
            serde_json::to_value(store.records()?)?[0]["lifetime"]["expires_at_unix"],
            FUTURE + 60,
        );
    }
    Ok(())
}

#[test]
fn saved_cookie_extreme_max_age_is_bounded_without_overflow() -> Result<()> {
    let store = restore_cookies(vec![dated_cookie(
        SOURCE,
        "synthetic=bounded; Max-Age=999999999999999999999999; Path=/; Secure",
        FUTURE,
        Some(253_402_300_799),
    )])?;
    assert_eq!(cookie_pairs(&store, SOURCE)?, ["synthetic=bounded"]);
    Ok(())
}
