#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::Scope;
use serde_json::json;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;
use uuid::Uuid;
fn saved() -> Checkpoint {
    let operation = Uuid::from_u128(13);
    let source: DriveEntry = serde_json::from_value(json!({"drivewsid":"FILE::com.apple.CloudDocs::source","docwsid":"source","zone":"com.apple.CloudDocs","name":format!("Cirrove Package Source {operation}"),"extension":"pages","parentId":"FOLDER::com.apple.CloudDocs::parent","etag":"source-etag","size":4,"type":"FILE"})).unwrap();
    let imported: DriveEntry = serde_json::from_value(json!({"drivewsid":"FILE::com.apple.CloudDocs::allocated","docwsid":"allocated","zone":"com.apple.CloudDocs","name":format!("Cirrove Package Import {operation}"),"extension":"pages","parentId":"FOLDER::com.apple.CloudDocs::parent","etag":"E0","size":4,"type":"FILE"})).unwrap();
    Checkpoint {
        version: 1,
        plan: OwnedPackagePlan {
            scope: Scope {
                account: Uuid::from_u128(14).to_string(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            operation,
            parent: source.parent_id.clone(),
            parent_name: format!("Cirrove Package Validation {operation}"),
            source,
            destination: imported.display_name(),
            archive_size: 4,
            archive_sha256: "a".repeat(64),
        },
        account_hash: crate::account_hash("fixture@example.com").unwrap(),
        imported,
        semantic: PackageSemanticIdentity {
            version: 1,
            sha256: "b".repeat(64),
            entries: 1,
            files: 1,
            expanded_bytes: 4,
        },
        phase: PackageTrashPhase::Prepared,
        renamed_etag: None,
    }
}
struct Vault {
    value: Mutex<Option<String>>,
    fail: bool,
    pause: bool,
    entered: Notify,
    release: Notify,
}
impl Vault {
    fn new(fail: bool, pause: bool) -> Self {
        Self {
            value: Mutex::new(None),
            fail,
            pause,
            entered: Notify::new(),
            release: Notify::new(),
        }
    }
}
#[async_trait::async_trait]
impl CredentialVault for Vault {
    async fn load(&self, _: &str) -> Result<Option<SecretString>> {
        Ok(self.value.lock().unwrap().clone().map(SecretString::from))
    }
    async fn save(&self, _: &str, value: SecretString) -> Result<()> {
        if self.fail {
            anyhow::bail!("injected durable persistence failure");
        }
        *self.value.lock().unwrap() = Some(value.expose_secret().to_owned());
        if self.pause {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
    async fn remove(&self, _: &str) -> Result<()> {
        anyhow::bail!("probe never removes checkpoints")
    }
}
fn phases() -> [(PackageTrashPhase, PackageTrashPhase); 3] {
    [
        (PackageTrashPhase::Prepared, PackageTrashPhase::RenameArmed),
        (
            PackageTrashPhase::Renamed,
            PackageTrashPhase::StaleTrashArmed,
        ),
        (
            PackageTrashPhase::StaleRefused,
            PackageTrashPhase::CurrentTrashArmed,
        ),
    ]
}
#[tokio::test]
async fn package_trash_each_failed_checkpoint_prevents_dispatch_and_reentry() {
    for (before, armed) in phases() {
        let mut saved = saved();
        saved.phase = before;
        saved.renamed_etag = Some("E1".into());
        let vault = Vault::new(true, false);
        let cancel = CancellationToken::new();
        let dispatches = AtomicUsize::new(0);
        if arm(&mut saved, &vault, armed, &cancel).await.is_ok() {
            dispatches.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert_eq!(saved.phase, armed, "failed save must not permit reentry");
        assert!(
            arm(&mut saved, &Vault::new(false, false), armed, &cancel)
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn package_trash_cancellation_during_each_durable_arm_never_dispatches() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for (before, armed) in phases() {
            let mut saved = saved();
            saved.phase = before;
            saved.renamed_etag = Some("E1".into());
            let vault = Arc::new(Vault::new(false, true));
            let worker = vault.clone();
            let cancel = CancellationToken::new();
            let token = cancel.clone();
            let task = tokio::spawn(async move {
                let dispatched = arm(&mut saved, worker.as_ref(), armed, &token)
                    .await
                    .is_ok();
                (saved, dispatched)
            });
            vault.entered.notified().await;
            cancel.cancel();
            vault.release.notify_one();
            let (mut saved, dispatched) = task.await.unwrap();
            assert!(!dispatched, "cancelled armed mutation would be sent");
            let restored: Checkpoint =
                serde_json::from_str(vault.value.lock().unwrap().as_deref().unwrap()).unwrap();
            assert_eq!(restored.phase, armed);
            assert!(
                arm(
                    &mut saved,
                    &Vault::new(false, false),
                    armed,
                    &CancellationToken::new()
                )
                .await
                .is_err(),
                "armed checkpoint replay admitted"
            );
        }
    })
    .await
    .unwrap();
}
#[test]
fn package_trash_checkpoint_binds_account_scope_allocated_identity_and_revision() {
    let mut saved = saved();
    let mut session = ICloudReadSession::new().unwrap();
    session.account_hash = Some(saved.account_hash.clone());
    saved
        .validate(&session, &saved.plan, "fixture@example.com")
        .unwrap();
    assert!(
        saved
            .validate(&session, &saved.plan, "another@example.com")
            .is_err()
    );
    let mut other = saved.plan.clone();
    other.scope.account = Uuid::from_u128(15).to_string();
    assert!(
        saved
            .validate(&session, &other, "fixture@example.com")
            .is_err()
    );
    saved.imported.docwsid = "foreign".into();
    assert!(
        saved
            .validate(&session, &saved.plan, "fixture@example.com")
            .is_err()
    );
    saved.imported.docwsid = "allocated".into();
    saved.phase = PackageTrashPhase::Renamed;
    for etag in [None, Some(String::new()), Some("E0".into())] {
        saved.renamed_etag = etag;
        assert!(
            saved
                .validate(&session, &saved.plan, "fixture@example.com")
                .is_err()
        );
    }
    saved.renamed_etag = Some("E1".into());
    saved
        .validate(&session, &saved.plan, "fixture@example.com")
        .unwrap();
}
#[test]
fn package_trash_rename_requires_same_owned_id_new_revision_and_exact_name() {
    let mut saved = saved();
    let mut renamed = saved.imported.clone();
    renamed.name = format!("Cirrove Package Trash {}", saved.plan.operation);
    assert!(
        check_active(&renamed, &saved, true).is_err(),
        "unchanged ETag accepted"
    );
    renamed.etag = "E1".into();
    check_active(&renamed, &saved, true).unwrap();
    saved.renamed_etag = Some("E1".into());
    let mut changed = renamed.clone();
    changed.etag = "E2".into();
    assert!(check_active(&changed, &saved, true).is_err());
    changed = renamed.clone();
    changed.docwsid = "foreign".into();
    assert!(check_active(&changed, &saved, true).is_err());
    changed = renamed.clone();
    changed.parent_id = ROOT_ID.into();
    assert!(check_active(&changed, &saved, true).is_err());
    changed = renamed;
    changed.name = "personal".into();
    assert!(check_active(&changed, &saved, true).is_err());
}
#[test]
fn package_trash_inspection_does_not_claim_stale_refusal_when_stale_request_trashed_item() {
    let mut saved = saved();
    saved.phase = PackageTrashPhase::StaleTrashArmed;
    let observed = inspection(&saved, PackageTrashLocation::RecoverableTrash);
    assert!(!observed.metadata_stale_refusal_recorded);
    assert!(!observed.current_trash_semantic_recovery_verified);
    saved.phase = PackageTrashPhase::CurrentTrashArmed;
    let recovered = inspection(&saved, PackageTrashLocation::RecoverableTrash);
    assert!(
        recovered.metadata_stale_refusal_recorded
            && recovered.current_trash_semantic_recovery_verified
    );
    let still_active = inspection(&saved, PackageTrashLocation::RenamedActive);
    assert!(!still_active.current_trash_semantic_recovery_verified);
}
#[test]
fn package_trash_anonymous_staging_explicitly_sets_private_permissions() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let file = anonymous_staging(dir.path()).unwrap();
    let meta = file.metadata().unwrap();
    assert!(meta.is_file() && meta.len() == 0 && meta.nlink() == 0);
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

#[tokio::test]
async fn package_trash_each_dispatch_is_preceded_by_matching_durable_armed_state() {
    for (before, armed) in phases() {
        let mut saved = saved();
        saved.phase = before;
        saved.renamed_etag = Some("E1".into());
        let vault = Vault::new(false, false);
        arm(&mut saved, &vault, armed, &CancellationToken::new())
            .await
            .unwrap();
        // This is the dispatch boundary: the durable record must already identify
        // this operation, exact imported ID, and its potentially-sent phase.
        let durable: Checkpoint =
            serde_json::from_str(vault.value.lock().unwrap().as_deref().unwrap()).unwrap();
        assert_eq!(durable.phase, armed);
        assert_eq!(durable.plan.operation, saved.plan.operation);
        assert_eq!(durable.imported.drivewsid, saved.imported.drivewsid);
        assert!(
            arm(&mut saved, &vault, armed, &CancellationToken::new())
                .await
                .is_err()
        );
    }
}

mod execute;

#[test]
fn package_trash_state_paths_refuse_aliases_symlinks_and_nonprivate_directories() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("import");
    let second = root.path().join("probe");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    for path in [&first, &second] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    separate_directories(&first, &second).unwrap();
    assert!(separate_directories(&first, &first.join(".")).is_err());
    let link = root.path().join("alias");
    std::os::unix::fs::symlink(&first, &link).unwrap();
    assert!(separate_directories(&first, &link).is_err());
    std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(separate_directories(&first, &second).is_err());
}
