#![allow(clippy::unwrap_used)]
use super::*;
fn fixture() -> (Registration, Marker, UploadRecord, UploadRecord) {
    let run = Uuid::new_v4();
    let competitor_run = Uuid::new_v4();
    let account = Uuid::new_v4();
    let op = Uuid::new_v4();
    let attempt = Uuid::new_v4();
    let imported = Uuid::new_v4();
    let root = PathBuf::from(format!("/var/tmp/cirrove-native-final-{run}"));
    let parent:Node=serde_json::from_value(serde_json::json!({"id":"FOLDER::com.apple.CloudDocs::owned","parent_id":cirrove_icloud::ROOT_ID,"name":format!("Cirrove-Native-{run}"),"kind":"folder","size":0,"etag":"parent-r1","target":null})).unwrap();
    let old:Node=serde_json::from_value(serde_json::json!({"id":"FILE::com.apple.CloudDocs::original","parent_id":parent.id,"name":format!("Cirrove-Numbers-Parent-{run}.numbers"),"kind":"folder","package":true,"size":123,"etag":"original-r1","target":null})).unwrap();
    let semantic_a:PackageSemanticIdentity=serde_json::from_value(serde_json::json!({"version":2,"entries":2,"files":1,"expanded_bytes":3,"sha256":"a".repeat(64)})).unwrap();
    let mut semantic_b = semantic_a.clone();
    semantic_b.sha256 = "b".repeat(64);
    let r = Registration {
        version: 1,
        phase: Phase::Capture,
        run,
        account,
        collection: "drive".into(),
        session_directory: root.clone(),
        competitor_run,
        competitor_directory: format!("/var/tmp/cirrove-native-final-{competitor_run}").into(),
        settings_sha256: "c".repeat(64),
        competitor_settings_sha256: "d".repeat(64),
        operation: op,
        competitor_import: imported,
        parent: parent.clone(),
        marker_sha256: "e".repeat(64),
        source_a: Source {
            path: root.join("source-a.numbers"),
            size: 123,
            sha256: "a".repeat(64),
            root: None,
            semantic: semantic_a.clone(),
        },
        source_b: Source {
            path: root.join("source-b.numbers"),
            size: 124,
            sha256: "b".repeat(64),
            root: None,
            semantic: semantic_b.clone(),
        },
        started_unix: now().unwrap(),
        deadline_unix: now().unwrap() + 600,
        baseline: None,
    };
    let row:UploadRecord=serde_json::from_value(serde_json::json!({"id":op,"sequence":3,"scope":scope(&r),"intent":{"kind":"replace","item":old.id,"expected_etag":"original-r1"},"representation":{"kind":"flat_numbers_replacement_archive","original":old,"semantic":semantic_b,"original_semantic":semantic_a},"state":"uploading","size":124,"sha256":"b".repeat(64),"attempt":attempt,"remote":null,"session_key":op})).unwrap();
    let other:UploadRecord=serde_json::from_value(serde_json::json!({"id":imported,"sequence":1,"scope":scope(&r),"intent":{"kind":"create","parent":parent.id,"name":format!("recovery-by-cirrove-{op}.numbers")},"representation":{"kind":"flat_numbers_archive","semantic":semantic_b},"package_completion":semantic_b,"state":"uploaded","size":124,"sha256":"b".repeat(64),"attempt":null,"remote":{"id":"FILE::com.apple.CloudDocs::occupant","parent_id":parent.id,"name":format!("recovery-by-cirrove-{op}.numbers"),"kind":"folder","package":true,"size":124,"etag":"occupant-r1","target":null}})).unwrap();
    let marker = Marker {
        version: 1,
        run,
        operation: op,
        attempt,
        checkpoint_sha256: "f".repeat(64),
        phase: "handoff-move-old-armed".into(),
        inner_commit_called: false,
        original_id: old.id,
        staged_id: "FILE::com.apple.CloudDocs::stage".into(),
        parent_id: parent.id,
    };
    (r, marker, row, other)
}
#[test]
fn native_pre_trash_three_identity_admission_and_foreign_authority_refusal() {
    for arm in 0..17 {
        let (mut r, mut m, mut row, mut other) = fixture();
        check(&r, &m, &row, &other).unwrap();
        match arm {
            0 => r.collection = "foreign".into(),
            1 => r.competitor_run = r.run,
            2 => m.operation = Uuid::new_v4(),
            3 => m.run = Uuid::new_v4(),
            4 => m.original_id = m.staged_id.clone(),
            5 => m.staged_id = m.original_id.clone(),
            6 => m.parent_id = "foreign".into(),
            7 => m.phase = "handoff-inspect-install".into(),
            8 => m.inner_commit_called = true,
            9 => row.session_key = None,
            10 => row.attempt = Some(Uuid::new_v4()),
            11 => other.scope.account = Uuid::new_v4().to_string(),
            12 => other.state = UploadState::VerifyRequired,
            13 => other.remote.as_mut().unwrap().id = m.staged_id.clone(),
            14 => {
                other.intent = UploadIntent::Create {
                    parent: r.parent.id.clone(),
                    name: r.parent.name.clone(),
                }
            }
            15 => r.source_b.root = Some("Source.numbers".into()),
            16 => r.deadline_unix = r.started_unix + 601,
            _ => unreachable!(),
        };
        assert!(check(&r, &m, &row, &other).is_err(), "arm {arm}");
    }
}
#[test]
fn native_pre_trash_exact_revision_and_name_peer_refusal() {
    let (r, m, row, _) = fixture();
    let old = original(&row).unwrap();
    let e = DriveEntry {
        drivewsid: old.id.clone(),
        docwsid: "original".into(),
        item_id: String::new(),
        zone: "com.apple.CloudDocs".into(),
        name: old.name.trim_end_matches(".numbers").into(),
        extension: "numbers".into(),
        parent_id: r.parent.id.clone(),
        etag: "original-r1".into(),
        kind: "FILE".into(),
        size: 123,
        items: vec![],
        number_of_items: None,
    };
    exact(
        &[e.clone()],
        &old.id,
        &old.name,
        &m.parent_id,
        old.etag.as_deref(),
    )
    .unwrap();
    let mut stale = e.clone();
    stale.etag = "newer-r2".into();
    assert!(
        exact(
            &[stale],
            &old.id,
            &old.name,
            &m.parent_id,
            old.etag.as_deref()
        )
        .is_err()
    );
    let mut peer = e.clone();
    peer.drivewsid = "FILE::com.apple.CloudDocs::foreign".into();
    peer.docwsid = "foreign".into();
    assert!(
        exact(
            &[e.clone(), peer],
            &old.id,
            &old.name,
            &m.parent_id,
            old.etag.as_deref()
        )
        .is_err()
    );
    assert!(
        exact(
            &[e.clone(), e],
            &old.id,
            &old.name,
            &m.parent_id,
            old.etag.as_deref()
        )
        .is_err()
    );
}
#[test]
fn native_pre_trash_source_requires_explicit_null_and_no_extra_authority() {
    let (r, _, _, _) = fixture();
    let raw = serde_json::json!({"path":r.source_b.path,"size":r.source_b.size,"sha256":r.source_b.sha256,"root":null,"semantic":r.source_b.semantic});
    assert!(serde_json::from_value::<Source>(raw.clone()).is_ok());
    let mut missing = raw.clone();
    missing.as_object_mut().unwrap().remove("root");
    assert!(serde_json::from_value::<Source>(missing).is_err());
    let mut extra = raw;
    extra["allow_foreign_name"] = true.into();
    assert!(serde_json::from_value::<Source>(extra).is_err());
}

#[test]
fn native_pre_trash_genuine_competitor_requires_receipt_revision_and_plain_import() {
    for arm in 0..8 {
        let (r, m, row, mut other) = fixture();
        check(&r, &m, &row, &other).unwrap();
        match arm {
            0 => other.remote.as_mut().unwrap().etag = None,
            1 => other.remote.as_mut().unwrap().etag = Some("*".into()),
            2 => other.remote.as_mut().unwrap().etag = Some("bad\nrevision".into()),
            3 => other.remote.as_mut().unwrap().content_version = Some("different".into()),
            4 => other.working_file = Some(Uuid::new_v4()),
            5 => other.remote.as_mut().unwrap().etag = Some(String::new()),
            6 => {
                other.base = Some(crate::journal::UploadBase {
                    predecessor: Uuid::new_v4(),
                    resolved: false,
                })
            }
            7 => {
                let mut v = serde_json::to_value(&other).unwrap();
                v["identity_handoff"] = serde_json::json!({"recovery_object":Uuid::new_v4(),"old_item":"FILE::com.apple.CloudDocs::old","recovery_name":"recovery.numbers"});
                other = serde_json::from_value(v).unwrap();
            }
            _ => unreachable!(),
        };
        assert!(check(&r, &m, &row, &other).is_err(), "arm {arm}");
    }
    let (r, m, row, mut other) = fixture();
    let alias = other.remote.as_ref().unwrap().etag.clone();
    other.remote.as_mut().unwrap().content_version = alias;
    check(&r, &m, &row, &other).unwrap();
}
#[test]
fn native_pre_trash_sql_body_sequence_and_complete_queue_are_both_authority() {
    let (_, _, row, other) = fixture();
    for actual in [&row, &other] {
        let id = actual.id.to_string();
        let state = serde_json::to_value(actual.state)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        let seq = i64::try_from(actual.sequence).unwrap();
        let complete = if actual.state == UploadState::Uploaded {
            1
        } else {
            0
        };
        let q = vec![(seq, id.clone(), complete)];
        row_tuple(actual, &id, seq, &state, &q).unwrap();
        for arm in 0..6 {
            let (mut column, mut queued) = (seq, q.clone());
            match arm {
                0 => column += 1,
                1 => column = -1,
                2 => queued.clear(),
                3 => queued[0].0 += 1,
                4 => queued[0].1 = Uuid::new_v4().to_string(),
                5 => queued[0].2 = 1 - complete,
                _ => unreachable!(),
            };
            assert!(
                row_tuple(actual, &id, column, &state, &queued).is_err(),
                "arm {arm}"
            );
        }
    }
}

#[test]
fn native_pre_trash_publication_preexpired_refuses_before_disk_record() -> Result<()> {
    let directory = tempfile::Builder::new()
        .prefix("native-preservation-deadline-")
        .tempdir()?
        .keep();
    let value = serde_json::json!({"version":1,"scope":"synthetic deadline control"});
    let accepted = directory.join("accepted.json");
    publish_result(
        tokio::time::Instant::now() + Duration::from_secs(60),
        || record(&accepted, &value),
    )?;
    let original = std::fs::read(&accepted)?;
    ensure!(serde_json::from_slice::<serde_json::Value>(&original)? == value);
    let expired = tokio::time::Instant::now()
        .checked_sub(Duration::from_secs(1))
        .ok_or_else(|| anyhow::anyhow!("synthetic expired clock unavailable"))?;
    let refused = directory.join("expired.json");
    let invoked = std::cell::Cell::new(false);
    let actual = publish_result(expired, || {
        invoked.set(true);
        record(&refused, &value)
    });
    ensure!(std::fs::read(&accepted)? == original);
    ensure!(
        actual.is_err() && !invoked.get() && !refused.exists(),
        "expired native preservation publication created a result"
    );
    Ok(())
}

#[test]
fn native_pre_trash_publication_synchronous_overrun_refuses_after_disk_record() -> Result<()> {
    let directory = tempfile::Builder::new()
        .prefix("native-preservation-overrun-")
        .tempdir()?
        .keep();
    let path = directory.join("overrun.json");
    let value = serde_json::json!({"version":1,"scope":"synthetic publication overrun"});
    let active_end = tokio::time::Instant::now() + Duration::from_millis(50);
    let actual = publish_result(active_end, || {
        // Use the real result recorder and its fsync, then consume the remaining
        // clock synchronously. No fake provider, timer runtime or callback data.
        record(&path, &value)?;
        std::thread::sleep(
            active_end.saturating_duration_since(tokio::time::Instant::now())
                + Duration::from_millis(10),
        );
        Ok(())
    });
    // Establish real disk publication before the desired refusal assertion.
    ensure!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path)?)? == value);
    ensure!(std::fs::metadata(&path)?.mode() & 0o7777 == 0o600);
    ensure!(tokio::time::Instant::now() >= active_end);
    ensure!(
        actual.is_err(),
        "native preservation synchronous publication overrun was accepted"
    );
    Ok(())
}
