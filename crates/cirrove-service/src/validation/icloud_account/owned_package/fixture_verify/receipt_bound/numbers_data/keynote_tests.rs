#![allow(clippy::unwrap_used)]
use super::*;

fn keynote() -> Actual {
    actual_for(DataArm::Keynote)
}
fn decoded(plan: &Registration, arm: DataArm) -> Result<Registration> {
    let raw = serde_json::to_vec(plan)?;
    registration(&raw, &hex::encode(Sha256::digest(&raw)), arm)
}
fn settings(actual: &mut Actual, change: impl FnOnce(&mut serde_json::Value)) {
    let mut value = serde_json::json!({"version":2,"accounts":[{
        "id":actual.plan.account,"label":actual.plan.label,"registration":{"provider":"i_cloud"},
        "identity":{"tenant_id":"icloud","subject":"synthetic","username":"synthetic.invalid",
            "graph_user_id":"synthetic","display_name":"Synthetic"},
        "credential_id":Uuid::new_v4(),"access":"read_write",
        "drive":{"id":"drive","name":"Drive","driveType":"icloud_drive"},
        "root_id":cirrove_icloud::ROOT_ID,"mount_path":actual.plan.session_directory.join("mount"),
        "enabled":true,"poll_seconds":300,"cache_bytes":1048576}]});
    change(&mut value);
    let state = actual.temp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let raw = serde_json::to_vec(&value).unwrap();
    std::fs::write(state.join("accounts.json"), &raw).unwrap();
    std::fs::set_permissions(
        state.join("accounts.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    actual.plan.settings_sha256 = hex::encode(Sha256::digest(raw));
}

#[test]
fn owned_keynote_data_explicit_route_preserves_numbers_wire_and_refuses_cross_format() {
    let actual = keynote();
    decoded(&actual.plan, DataArm::Keynote).unwrap();
    assert!(decoded(&actual.plan, DataArm::Numbers).is_err());
    let pages = actual_for(DataArm::Pages);
    decoded(&pages.plan, DataArm::Pages).unwrap();
    assert!(decoded(&pages.plan, DataArm::Keynote).is_err());
    assert!(decoded(&actual.plan, DataArm::Pages).is_err());
    let numbers = actual_for(DataArm::Numbers);
    let raw = serde_json::to_value(&numbers.plan).unwrap();
    assert!(
        raw.get("arm").is_none()
            && raw.get("active_end").is_none()
            && raw.get("original_window").is_none()
    );
    decoded(&numbers.plan, DataArm::Numbers).unwrap();
    assert!(decoded(&numbers.plan, DataArm::Keynote).is_err());
    let mut with_clock = raw.clone();
    with_clock["original_window"] = serde_json::Value::Null;
    let bytes = serde_json::to_vec(&with_clock).unwrap();
    assert!(
        registration(
            &bytes,
            &hex::encode(Sha256::digest(&bytes)),
            DataArm::Numbers
        )
        .is_err()
    );
    for field in ["arm", "representation", "semantic", "source_layout"] {
        let mut value = serde_json::to_value(&actual.plan).unwrap();
        value[field] = "package".into();
        let raw = serde_json::to_vec(&value).unwrap();
        assert!(registration(&raw, &hex::encode(Sha256::digest(&raw)), DataArm::Keynote).is_err());
    }
}

#[test]
fn owned_keynote_data_original_window_refuses_renewed_expired_missing_or_wrong_reserve() {
    let actual = keynote();
    for arm in 0..5 {
        let mut p = actual.plan.clone();
        match arm {
            0 => p.original_window = None,
            1 => p.original_window.as_mut().unwrap().deadline_unix_ms += 1,
            2 => p.original_window.as_mut().unwrap().cleanup_seconds = 0,
            3 => {
                let w = p.original_window.as_mut().unwrap();
                w.started_unix_ms += 700_000;
                w.deadline_unix_ms += 700_000;
            }
            _ => {
                let w = p.original_window.as_mut().unwrap();
                w.started_unix_ms -= 700_000;
                w.deadline_unix_ms -= 700_000;
            }
        }
        assert!(
            decoded(&p, DataArm::Keynote).is_err(),
            "window arm {arm} accepted"
        );
    }
}

#[test]
fn owned_keynote_data_exact_root_refuses_coherently_rebound_foreign_namespace() {
    let actual = keynote();
    actual.plan.sources().validate().unwrap();
    let mut sources = actual.plan.sources();
    sources.session_directory = PathBuf::from(DataArm::Numbers.root(sources.run));
    sources.source_a.path = sources.session_directory.join("source-a.key");
    sources.source_b.path = sources.session_directory.join("source-b.key");
    assert!(
        sources.validate().is_err(),
        "foreign coherently rebound Keynote DATA root accepted"
    );
}

#[test]
fn owned_keynote_data_settings_refuse_rehashed_foreign_label_account_drive_and_mount() {
    let mut actual = keynote();
    settings(&mut actual, |_| {});
    account_binding(&actual.plan).unwrap();
    for arm in 0..7 {
        settings(&mut actual, |value| {
            let a = &mut value["accounts"][0];
            match arm {
                0 => a["label"] = "iCloudNumbersDataValidation".into(),
                1 => a["id"] = serde_json::json!(Uuid::new_v4()),
                2 => a["drive"]["driveType"] = "other".into(),
                3 => a["drive"]["id"] = "foreign".into(),
                4 => a["mount_path"] = "/var/tmp/foreign".into(),
                5 => a["credential_id"] = serde_json::json!(Uuid::nil()),
                _ => a["access"] = "read_only".into(),
            }
        });
        assert!(
            account_binding(&actual.plan).is_err(),
            "settings arm {arm} accepted"
        );
    }
}

#[test]
fn owned_keynote_data_sources_require_immutable_exact_raw_bytes_without_package_claim() {
    let actual = keynote();
    actual.plan.sources().verify().unwrap();
    let raw = serde_json::to_vec(&actual.plan.sources()).unwrap();
    let path = actual.temp.path().join("source-registration.json");
    std::fs::write(&path, &raw).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let hash = hex::encode(Sha256::digest(&raw));
    let proof = icloud_owned_keynote_data_source_verify(&path, &hash).unwrap();
    assert_eq!(proof["raw_source_identity_verified"], true);
    assert_eq!(proof["representation_observed"], false);
    assert!(icloud_owned_numbers_data_source_verify(&path, &hash).is_err());
    assert!(icloud_owned_pages_data_source_verify(&path, &hash).is_err());
    std::fs::set_permissions(
        &actual.plan.source_a.path,
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(
        actual.plan.sources().verify().is_err(),
        "mutable raw Keynote source accepted"
    );
    std::fs::write(&actual.plan.source_a.path, b"changed immutable source").unwrap();
    std::fs::set_permissions(
        &actual.plan.source_a.path,
        std::fs::Permissions::from_mode(0o400),
    )
    .unwrap();
    assert!(
        actual.plan.sources().verify().is_err(),
        "changed raw Keynote source accepted"
    );
}

#[test]
fn owned_keynote_data_actual_create_save_and_retired_completed_backup_are_bound() {
    for rehydrate in [false, true] {
        let mut actual = keynote();
        let (_, a, none, _) = actual.check().unwrap();
        assert_eq!(a, actual.original);
        assert!(none.is_none());
        actual.save(rehydrate);
        let (_, b, trash, _) = actual.check().unwrap();
        assert_ne!(b.id, a.id);
        assert_eq!(trash.as_ref().unwrap().id, a.id);
        assert_eq!(trash.as_ref().unwrap().name, actual.plan.name());
        actual.retire(b);
        actual.check().unwrap();
    }
}

#[test]
fn owned_keynote_data_completed_receipts_refuse_package_foreign_revision_working_and_identity() {
    for arm in 0..6 {
        let mut actual = keynote();
        actual.save(false);
        actual.check().unwrap();
        let journal = actual.writer();
        let mut row = journal.get(actual.plan.save.unwrap()).unwrap();
        match arm {
            0 => row.remote.as_mut().unwrap().package = true,
            1 => row.scope.account = Uuid::new_v4().to_string(),
            2 => {
                row.intent = UploadIntent::Replace {
                    item: actual.original.id.clone(),
                    expected_etag: "foreign".into(),
                }
            }
            3 => row.working_file = Some(Uuid::new_v4()),
            4 => row.remote.as_mut().unwrap().id = actual.original.id.clone(),
            _ => row.scope.collection = "foreign".into(),
        }
        actual
            .db()
            .execute(
                "UPDATE uploads SET body=?2 WHERE id=?1",
                rusqlite::params![row.id.to_string(), serde_json::to_string(&row).unwrap()],
            )
            .unwrap();
        drop(journal);
        assert!(
            actual.check().is_err(),
            "completed receipt arm {arm} accepted"
        );
    }
}

#[test]
fn owned_keynote_data_actual_sql_body_queue_and_dirty_frontier_refuse_substitution() {
    for arm in 0..4 {
        let actual = keynote();
        actual.check().unwrap();
        match arm {
            0 => {
                actual
                    .db()
                    .execute(
                        "UPDATE write_queue SET complete=0 WHERE id=?1",
                        [actual.plan.create.to_string()],
                    )
                    .unwrap();
            }
            1 => {
                actual
                    .db()
                    .execute(
                        "UPDATE uploads SET sequence=999 WHERE id=?1",
                        [actual.plan.create.to_string()],
                    )
                    .unwrap();
            }
            2 => {
                actual
                    .db()
                    .execute(
                        "DELETE FROM namespace_operations WHERE operation=?1",
                        [actual.plan.create.to_string()],
                    )
                    .unwrap();
            }
            _ => {
                let mut j = actual.writer();
                j.write_working(actual.plan.working_a, 0, b"dirty").unwrap();
            }
        }
        assert!(
            actual.check().is_err(),
            "actual frontier arm {arm} accepted"
        );
    }
}

#[test]
fn owned_keynote_data_prior_independent_actual_data_proof_refuses_package_and_cross_format() {
    let mut actual = keynote();
    actual.save(false);
    let value = prior_artifacts(&mut actual);
    prior_created(&actual.plan, &actual.original, &actual.parent).unwrap();
    for arm in 0..7 {
        let mut v = value.clone();
        match arm {
            0 => v["current"]["representation"] = "package".into(),
            1 => v["current"]["format"] = "numbers".into(),
            2 => v["current"]["etag"] = "foreign".into(),
            3 => v["current"]["item"] = "FILE::com.apple.CloudDocs::foreign".into(),
            4 => v["current"]["sha256"] = actual.plan.source_b.sha256.clone().into(),
            5 => v["current"]["account"] = serde_json::json!(Uuid::new_v4()),
            _ => v["current"]["parent"] = "FOLDER::com.apple.CloudDocs::foreign".into(),
        }
        let raw = serde_json::to_vec(&v).unwrap();
        let mut p = actual.plan.clone();
        p.created_proof_sha256 = Some(hex::encode(Sha256::digest(&raw)));
        assert!(
            created_proof_binding(&p, &actual.original, &actual.parent, &raw).is_err(),
            "prior actual DATA proof arm {arm} accepted"
        );
    }
}

#[test]
fn owned_keynote_data_completed_backup_full_projection_refuses_changed_receipt_identity() {
    let mut actual = keynote();
    actual.save(false);
    actual.check().unwrap();
    let journal = actual.writer();
    let row = journal.get(actual.plan.save.unwrap()).unwrap();
    let id = row.ordinary_validation_recovery_owner().unwrap();
    let mut recovery = journal.namespace_object(id).unwrap();
    recovery.node.modified_unix += 1;
    actual
        .db()
        .execute(
            "UPDATE namespace_objects SET body=?2 WHERE id=?1",
            rusqlite::params![id.to_string(), serde_json::to_string(&recovery).unwrap()],
        )
        .unwrap();
    drop(journal);
    assert!(
        actual.check().is_err(),
        "completed backup full identity projection accepted drift"
    );
}

#[test]
fn owned_keynote_data_publication_overrun_retains_late_artifact_but_refuses_success() {
    let mut actual = keynote();
    actual.plan.active_end =
        Some(tokio::time::Instant::now() + std::time::Duration::from_millis(25));
    let artifact = actual.temp.path().join("late-publication.json");
    let result = publish_result(&actual.plan, || {
        record(&artifact, &serde_json::json!({"synthetic":true}))?;
        std::thread::sleep(std::time::Duration::from_millis(40));
        Ok(())
    });
    assert!(artifact.is_file());
    assert!(
        result.is_err(),
        "late Keynote DATA publication accepted as successful endpoint"
    );
}

#[test]
fn owned_keynote_data_inner_fixture_requires_explicit_data_null_root_and_exact_keynote_route() {
    let actual = keynote();
    let value = serde_json::json!({
        "version":1,"run":actual.plan.run,"account":actual.plan.account,
        "session_directory":actual.plan.session_directory,"settings_sha256":actual.plan.settings_sha256,
        "parent":{"drivewsid":actual.parent.id,"docwsid":"owned-parent","parentId":cirrove_icloud::ROOT_ID,
            "name":actual.parent.name,"type":"FOLDER","zone":"com.apple.CloudDocs","etag":"folder-revision","size":0},
        "document":{"drivewsid":actual.original.id,"docwsid":"original-a","parentId":actual.parent.id,
            "name":actual.plan.name().strip_suffix(".key").unwrap(),"extension":"key","type":"FILE",
            "zone":"com.apple.CloudDocs","etag":actual.original.etag,"size":actual.original.size},
        "format":"keynote","representation":"data","source":actual.plan.source_a.path,
        "source_size":actual.plan.source_a.size,"source_sha256":actual.plan.source_a.sha256,
        "source_root":null,"expected_root":actual.plan.name(),"semantic":null});
    let f: Fixture = serde_json::from_value(value.clone()).unwrap();
    validate_keynote_data(&f).unwrap();
    assert!(
        validate(&f).is_err(),
        "public generic route silently admitted null-root Keynote DATA"
    );
    assert!(validate_pages_data(&f).is_err());
    assert!(
        validate_flat_pages(&f).is_err(),
        "FlatPages PACKAGE route admitted DATA"
    );
    for arm in 0..6 {
        let mut changed: Fixture = serde_json::from_value(value.clone()).unwrap();
        match arm {
            0 => changed.representation = FixtureRepresentation::Package,
            1 => changed.source_root = Some("Source.key".into()),
            2 => changed.format = "numbers".into(),
            3 => changed.source = changed.session_directory.join("source-a.numbers"),
            4 => changed.expected_root = format!("foreign-{}.key", actual.plan.run),
            _ => {
                changed.semantic = Some(
                    serde_json::from_value(serde_json::json!({
                        "version":2,"sha256":"a".repeat(64),"entries":2,"files":1,"expanded_bytes":1
                    }))
                    .unwrap(),
                )
            }
        }
        assert!(
            validate_keynote_data(&changed).is_err(),
            "inner DATA fixture arm {arm} accepted"
        );
    }
}
