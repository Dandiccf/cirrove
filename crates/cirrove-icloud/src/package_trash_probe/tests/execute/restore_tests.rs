use super::*;
use crate::package_trash_probe::restore::{
    self, OwnedPackageRestoreProbe, PackageRestorePhase, RestoreCheckpoint,
};

#[derive(Clone, Copy)]
enum RestoreFault {
    None,
    FailArm,
    PauseArm,
    FailReceipt,
}
pub(super) struct RestoreVault {
    latest: Mutex<Option<String>>,
    fault: RestoreFault,
    entered: Notify,
    release: Notify,
}
#[async_trait::async_trait]
impl CredentialVault for RestoreVault {
    async fn load(&self, _: &str) -> Result<Option<SecretString>> {
        Ok(self.latest.lock().unwrap().clone().map(SecretString::from))
    }
    async fn save(&self, _: &str, value: SecretString) -> Result<()> {
        let saved: RestoreCheckpoint = serde_json::from_str(value.expose_secret())?;
        if (matches!(self.fault, RestoreFault::FailArm)
            && saved.phase == PackageRestorePhase::RestoreArmed)
            || (matches!(self.fault, RestoreFault::FailReceipt)
                && saved.phase == PackageRestorePhase::ReceiptObserved)
        {
            anyhow::bail!("injected restore persistence failure");
        }
        *self.latest.lock().unwrap() = Some(value.expose_secret().into());
        if matches!(self.fault, RestoreFault::PauseArm)
            && saved.phase == PackageRestorePhase::RestoreArmed
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
    async fn remove(&self, _: &str) -> Result<()> {
        anyhow::bail!("unexpected restore removal")
    }
}
impl RestoreVault {
    pub(super) fn phase(&self) -> PackageRestorePhase {
        self.saved().phase
    }
    fn saved(&self) -> RestoreCheckpoint {
        serde_json::from_str(self.latest.lock().unwrap().as_deref().unwrap()).unwrap()
    }
}
fn fresh_session(server: &Server, source: &Checkpoint) -> ICloudReadSession {
    let mut fresh = ICloudReadSession::new().unwrap();
    fresh.http = server.client.clone();
    fresh.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
    fresh.docs_endpoint = fresh.drive_endpoint.clone();
    fresh.account_hash = Some(source.account_hash.clone());
    fresh
}
async fn prepared(
    mode: &'static str,
    fault: RestoreFault,
) -> (
    tempfile::TempDir,
    OwnedPackageRestoreProbe,
    Arc<RestoreVault>,
    Server,
) {
    let (dir, owner, trash_vault, server) = fixture(Fault::None, 200, false, false).await;
    {
        let mut remote = server.state.lock().unwrap();
        remote.stale_reply = Some(
            json!({"items":[{"status":"ETAG_CONFLICT","drivewsid":owner.saved.imported.drivewsid,"parentId":owner.saved.plan.parent,"etag":"E1"}]}),
        );
        remote.restore_path = Some(json!(format!(
            "{}/{}",
            owner.saved.plan.parent_name,
            owner.saved.renamed_name()
        )));
    }
    owner.execute(&CancellationToken::new()).await.unwrap();
    let source: Checkpoint =
        serde_json::from_str(trash_vault.latest.lock().unwrap().as_deref().unwrap()).unwrap();
    let session = fresh_session(&server, &source);
    let saved = RestoreCheckpoint {
        version: 1,
        source,
        phase: PackageRestorePhase::Prepared,
        trash_etag: "trash-E1".into(),
        receipt_etag: None,
        receipt_http_status: None,
    };
    let vault = Arc::new(RestoreVault {
        latest: Mutex::new(Some(serde_json::to_string(&saved).unwrap())),
        fault,
        entered: Notify::new(),
        release: Notify::new(),
    });
    {
        let mut remote = server.state.lock().unwrap();
        remote.restore_vault = Some(vault.clone());
        remote.restore_mode = mode;
    }
    let probe = OwnedPackageRestoreProbe {
        session,
        apple_account: "fixture@example.com".into(),
        saved,
        vault: vault.clone(),
        staging: dir.path().into(),
        _marker: anonymous_staging(dir.path()).unwrap(),
    };
    (dir, probe, vault, server)
}
#[tokio::test]
async fn owned_native_restore_executes_once_and_verifies_identity_semantics_and_actual_name() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for mode in ["preserve", "different-name"] {
            let (dir, probe, vault, server) = prepared(mode, RestoreFault::None).await;
            let result = probe.execute(&CancellationToken::new()).await.unwrap();
            assert!(
                result.restore_receipt_recorded
                    && result.restored_same_identity_and_semantics
                    && result.owned_source_semantics_verified
            );
            assert_eq!(result.restored_name_preserved, Some(mode == "preserve"));
            assert!(!result.destination_vacancy_atomicity_proven);
            assert_eq!(vault.phase(), PackageRestorePhase::Verified);
            let restored = vault.saved();
            let mut fresh = fresh_session(&server, &restored.source);
            let observed = restore::inspect_saved_restore(
                &mut fresh,
                &restored,
                "fixture@example.com",
                dir.path(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            assert!(
                observed.restored_same_identity_and_semantics && observed.restore_receipt_recorded
            );
            assert_eq!(server.state.lock().unwrap().restore_calls, 1);
            server.finish().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn owned_native_restore_failed_or_cancelled_arm_sends_nothing() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, probe, vault, server) = prepared("preserve", RestoreFault::FailArm).await;
        assert_eq!(
            probe
                .execute(&CancellationToken::new())
                .await
                .err()
                .unwrap()
                .to_string(),
            "injected restore persistence failure"
        );
        assert_eq!(vault.phase(), PackageRestorePhase::Prepared);
        assert_eq!(server.state.lock().unwrap().restore_calls, 0);
        server.finish().await;
        let (_dir, probe, vault, server) = prepared("preserve", RestoreFault::PauseArm).await;
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let task = tokio::spawn(async move { probe.execute(&token).await });
        vault.entered.notified().await;
        assert_eq!(server.state.lock().unwrap().restore_calls, 0);
        cancel.cancel();
        vault.release.notify_one();
        assert!(task.await.unwrap().is_err());
        assert_eq!(vault.phase(), PackageRestorePhase::RestoreArmed);
        assert_eq!(server.state.lock().unwrap().restore_calls, 0);
        server.finish().await;
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn owned_native_restore_uncertain_results_are_inspected_without_replay() {
    tokio::time::timeout(Duration::from_secs(45), async {
        for (mode, fault, still_trash) in [
            ("lost", RestoreFault::None, false),
            ("receipt-foreign", RestoreFault::None, false),
            ("preserve", RestoreFault::FailReceipt, false),
            ("body-refused", RestoreFault::None, true),
            ("http409", RestoreFault::None, true),
        ] {
            let (dir, probe, vault, server) = prepared(mode, fault).await;
            assert!(probe.execute(&CancellationToken::new()).await.is_err());
            assert_eq!(vault.phase(), PackageRestorePhase::RestoreArmed);
            let saved = vault.saved();
            let mut fresh = fresh_session(&server, &saved.source);
            let observed = restore::inspect_saved_restore(
                &mut fresh,
                &saved,
                "fixture@example.com",
                dir.path(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(observed.still_recoverable_trash, still_trash);
            assert_eq!(observed.restored_same_identity_and_semantics, !still_trash);
            assert!(!observed.restore_receipt_recorded);
            assert_eq!(server.state.lock().unwrap().restore_calls, 1);
            server.finish().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn owned_native_restore_rejects_changed_identity_revision_and_native_content() {
    tokio::time::timeout(Duration::from_secs(40), async {
        for mode in [
            "active-foreign",
            "active-revision",
            "active-parent",
            "target-corrupt",
            "source-corrupt",
        ] {
            let (_dir, probe, vault, server) = prepared(mode, RestoreFault::None).await;
            assert!(
                probe.execute(&CancellationToken::new()).await.is_err(),
                "{mode}"
            );
            assert_eq!(vault.phase(), PackageRestorePhase::ReceiptObserved);
            assert_eq!(server.state.lock().unwrap().restore_calls, 1);
            server.finish().await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn owned_native_restore_checkpoint_binds_scope_source_digest_identity_and_receipt_phase() {
    let mut source = saved();
    source.version = 2;
    source.phase = PackageTrashPhase::Recovered;
    source.renamed_etag = Some("E1".into());
    source.revision_refusal = Some(PackageTrashRefusal::ItemEtagConflict { http_status: 200 });
    source.plan.operation = Uuid::parse_str("97be33d2-b216-49bf-9e49-465b1ca85d1d").unwrap();
    let plan = source.plan.clone();
    let mut session = ICloudReadSession::new().unwrap();
    session.account_hash = Some(source.account_hash.clone());
    let base = RestoreCheckpoint {
        version: 1,
        source,
        phase: PackageRestorePhase::Prepared,
        trash_etag: "T1".into(),
        receipt_etag: None,
        receipt_http_status: None,
    };
    base.validate(&session, "fixture@example.com", &plan)
        .unwrap();
    let encoded = serde_json::to_string(&base).unwrap();
    for field in [
        "scope",
        "digest",
        "document",
        "trash-etag",
        "phase",
        "status",
        "account",
    ] {
        let mut altered: RestoreCheckpoint = serde_json::from_str(&encoded).unwrap();
        match field {
            "scope" => altered.source.plan.scope.collection = "foreign".into(),
            "digest" => altered.source.plan.archive_sha256 = "c".repeat(64),
            "document" => altered.source.imported.docwsid = "foreign".into(),
            "trash-etag" => altered.trash_etag.clear(),
            "phase" => altered.phase = PackageRestorePhase::Verified,
            "status" => altered.receipt_http_status = Some(200),
            "account" => altered.source.account_hash = "foreign".into(),
            _ => unreachable!(),
        }
        assert!(
            altered
                .validate(&session, "fixture@example.com", &plan)
                .is_err(),
            "{field}"
        );
    }
}

#[tokio::test]
async fn owned_native_restore_aborted_armed_task_is_inspect_only_without_dispatch() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (dir, probe, vault, server) = prepared("preserve", RestoreFault::PauseArm).await;
        let task = tokio::spawn(async move { probe.execute(&CancellationToken::new()).await });
        vault.entered.notified().await;
        assert_eq!(server.state.lock().unwrap().restore_calls, 0);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let saved = vault.saved();
        assert_eq!(saved.phase, PackageRestorePhase::RestoreArmed);
        let mut fresh = fresh_session(&server, &saved.source);
        let observed = restore::inspect_saved_restore(
            &mut fresh,
            &saved,
            "fixture@example.com",
            dir.path(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(observed.still_recoverable_trash && !observed.restore_receipt_recorded);
        assert_eq!(server.state.lock().unwrap().restore_calls, 0);
        server.finish().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn owned_native_restore_preflight_rejects_destination_collision_and_changed_trash_revision() {
    tokio::time::timeout(Duration::from_secs(20), async {
        for collision in [true, false] {
            let (_dir, probe, vault, server) = prepared("preserve", RestoreFault::None).await;
            {
                let mut state = server.state.lock().unwrap();
                if collision {
                    state.restore_collision = true;
                } else {
                    state.current.etag = "new-Trash-revision".into();
                }
            }
            assert!(probe.execute(&CancellationToken::new()).await.is_err());
            assert_eq!(vault.phase(), PackageRestorePhase::Prepared);
            assert_eq!(server.state.lock().unwrap().restore_calls, 0);
            server.finish().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn owned_native_restore_accepts_target_item_id_projection_difference_without_relaxing_bindings()
 {
    tokio::time::timeout(Duration::from_secs(60), async {
        let (dir, probe, vault, server) = prepared("item-id-projection", RestoreFault::None).await;
        let result = probe
            .execute(&CancellationToken::new())
            .await
            .expect("equivalent list/detail target must verify despite optional item_id");
        assert!(
            result.restore_receipt_recorded
                && result.restored_same_identity_and_semantics
                && result.owned_source_semantics_verified
        );
        let saved = vault.saved();
        let mut fresh = fresh_session(&server, &saved.source);
        let observed = restore::inspect_saved_restore(
            &mut fresh,
            &saved,
            "fixture@example.com",
            dir.path(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(observed.restored_same_identity_and_semantics);
        assert_eq!(server.state.lock().unwrap().restore_calls, 1);
        server.finish().await;
        for mode in [
            "source-item-id-projection",
            "item-id-plus-id",
            "item-id-plus-doc",
            "item-id-plus-etag",
            "item-id-plus-size",
            "item-id-plus-name",
            "item-id-plus-extension",
            "item-id-plus-parent",
            "item-id-plus-kind",
            "item-id-plus-zone",
            "item-id-plus-items",
            "item-id-plus-number",
        ] {
            let (_dir, probe, vault, server) = prepared(mode, RestoreFault::None).await;
            assert!(
                probe.execute(&CancellationToken::new()).await.is_err(),
                "binding relaxed: {mode}"
            );
            assert_eq!(vault.phase(), PackageRestorePhase::ReceiptObserved);
            assert_eq!(server.state.lock().unwrap().restore_calls, 1);
            server.finish().await;
        }
    })
    .await
    .unwrap();
}
