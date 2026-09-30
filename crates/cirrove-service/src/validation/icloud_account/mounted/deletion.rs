//! Abrupt process exit after recoverable Trash, before the mounted journal receipt.
use super::*;
use crate::journal::MutationState;
use cirrove_core::mutation::{MutationError, MutationIntent, MutationReceipt, MutationRequest};
use std::sync::atomic::{AtomicUsize, Ordering};

const KIND: &str = "mounted-delete-recovery";
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path)?;
    ensure!(bytes.len() <= 32 * 1024, "validation record too large");
    Ok(serde_json::from_slice(&bytes)?)
}
fn expected(r: &MutationRequest, scope: &Scope, original: &Node) -> bool {
    r.scope == *scope
        && matches!(&r.intent, MutationIntent::RemoveFile { before }
        if before.id == original.id && before.parent_id == original.parent_id
            && before.name == original.name && before.etag == original.etag
            && before.size == original.size && before.kind == NodeKind::File
            && !before.package && before.target.is_none())
}
pub(super) struct Boundary {
    directory: std::path::PathBuf,
    scope: Scope,
    forbidden: Option<Uuid>,
    replays: AtomicUsize,
}
impl Boundary {
    fn new(f: &Fixture, forbidden: Option<Uuid>) -> Arc<Self> {
        Arc::new(Self {
            directory: f.run_dir.clone(),
            scope: f.scope.clone(),
            forbidden,
            replays: AtomicUsize::new(0),
        })
    }
    pub fn before(
        &self,
        operation: &str,
        r: &MutationRequest,
    ) -> cirrove_core::mutation::Result<()> {
        let id = Uuid::parse_str(operation).map_err(|_| MutationError::Invalid)?;
        // Recovery is read-only for all mutations, including an accidental new ID.
        if self.forbidden.is_some() {
            self.replays.fetch_add(1, Ordering::SeqCst);
            return Err(MutationError::Invalid);
        }
        let original: Node =
            read(&self.directory.join("original.json")).map_err(|_| MutationError::Invalid)?;
        if id.is_nil() || !expected(r, &self.scope, &original) {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }
    pub fn after(
        &self,
        operation: &str,
        r: &MutationRequest,
        receipt: &MutationReceipt,
    ) -> cirrove_core::mutation::Result<()> {
        self.before(operation, r)?;
        let MutationIntent::RemoveFile { before } = &r.intent else {
            return Err(MutationError::Invalid);
        };
        if !matches!(receipt, MutationReceipt::Removed { item } if item == &before.id) {
            return Err(MutationError::Invalid);
        }
        record(&self.directory.join("interrupted.json"), &serde_json::json!({
            "operation":operation,"pid":std::process::id(),"boundary":"trash_before_journal_receipt",
            "receipt":receipt,"acknowledgement_not_returned":true
        })).map_err(|_| MutationError::Uncertain)?;
        std::fs::File::open(&self.directory)
            .and_then(|f| f.sync_all())
            .map_err(|_| MutationError::Uncertain)?;
        std::process::exit(86);
    }
}
async fn application(f: &Fixture, script: &str) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::process::Command::new("python3")
            .arg("-c")
            .arg(script)
            .arg(f.run_dir.join("mount"))
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "mounted deletion application failed; state retained"
    );
    Ok(())
}
async fn absence(f: &Fixture) -> Result<()> {
    application(f, "import pathlib,sys; p=pathlib.Path(sys.argv[1]); assert not (p/'Account Router.txt').exists(); assert not list(p.iterdir())").await
}
async fn completed(session: &WritableSession, id: Uuid, original: &Node) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(900), async {
        loop {
            let rows = session.mutations(0, 2).await?;
            ensure!(
                rows.len() == 1 && rows[0].id == id,
                "unexpected deletion operation"
            );
            let row = &rows[0];
            ensure!(
                !matches!(
                    row.state,
                    MutationState::Conflict | MutationState::Failed | MutationState::NeedsReview
                ),
                "deletion needs review; state retained"
            );
            if row.state == MutationState::Applied {
                ensure!(
                    row.receipt
                        == Some(MutationReceipt::Removed {
                            item: original.id.clone()
                        }),
                    "wrong deletion receipt"
                );
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("deletion recovery deadline; state retained")?
}
pub async fn icloud_account_mounted_delete_interrupt(run: Uuid) -> Result<()> {
    let f = prepare(run, KIND).await?;
    let boundary = Boundary::new(&f, None);
    let session = mount_with_all_hooks(&f, None, None, None, Some(boundary)).await?;
    let result: Result<()> = async {
        app(&f, "create").await?;
        let original = uploaded(&session, 1).await?;
        verify(&f.snapshot, &f.account, &f.parent, &original, FIRST).await?;
        record(&f.run_dir.join("original.json"), &original)?;
        // Exercise unlink with a held descriptor, checking preserved bytes before
        // releasing it. The cloud deletion still runs through the ordinary router.
        application(
            &f,
            r#"
import pathlib,sys
p=pathlib.Path(sys.argv[1])/'Account Router.txt'
f=p.open('rb'); p.unlink(); assert not p.exists()
assert f.read()==b'Cirrove account-router original\n'; f.close()
"#,
        )
        .await?;
        record(
            &f.run_dir.join("application-verified.json"),
            &serde_json::json!({"held_read_after_unlink":true}),
        )?;
        tokio::time::timeout(Duration::from_secs(900), async {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
        .await
        .context("registered deletion boundary was not reached")?;
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    Ok(())
}
pub async fn icloud_account_mounted_delete_recover(run: Uuid) -> Result<()> {
    let run_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state")
        .join(format!("icloud-account-{KIND}-{run}"));
    let account: Account = read(&run_dir.join("account.json"))?;
    let parent: Node = read(&run_dir.join("owned-folder.json"))?;
    let original: Node = read(&run_dir.join("original.json"))?;
    let marker: serde_json::Value = read(&run_dir.join("interrupted.json"))?;
    let application: serde_json::Value = read(&run_dir.join("application-verified.json"))?;
    ensure!(
        application["held_read_after_unlink"] == true,
        "application evidence missing"
    );
    ensure!(
        matches!(account.registration, AppRegistration::ICloud)
            && !account.enabled
            && account.access == AccessMode::ReadWrite
            && account.label == "iCloudAccountRouterValidation"
            && account.root_id == ROOT_ID
            && account.mount_path == run_dir.join("unused-mount")
            && parent.name == format!("Cirrove Write Validation-{run}")
            && parent.parent_id.as_deref() == Some(ROOT_ID)
            && parent.kind == NodeKind::Folder
            && !parent.package
            && parent.target.is_none()
            && original.parent_id.as_ref() == Some(&parent.id)
            && original.name == NAME
            && original.kind == NodeKind::File
            && original.size == FIRST.len() as u64
            && !original.package
            && original.target.is_none()
            && marker["boundary"] == "trash_before_journal_receipt"
            && marker["acknowledgement_not_returned"] == true
            && marker["pid"]
                .as_u64()
                .is_some_and(|p| p != u64::from(std::process::id())),
        "invalid owned deletion fixture"
    );
    let operation = Uuid::parse_str(marker["operation"].as_str().context("missing operation")?)?;
    let receipt: MutationReceipt = serde_json::from_value(marker["receipt"].clone())?;
    ensure!(
        receipt
            == MutationReceipt::Removed {
                item: original.id.clone()
            },
        "interrupted receipt differs"
    );
    let state = run_dir.join("state");
    let snapshot = SealedSessionVault::new(&state, &account.id)?
        .load(&account.credential_id)
        .await?
        .context("isolated session missing")?;
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let f = Fixture {
        state,
        account,
        scope,
        snapshot,
        parent,
        run_dir,
    };
    let engine = engine(&f.state, &f.account, &f.scope, &f.snapshot).await?;
    let context = WriteContext::open(&engine, &f.state).await?;
    {
        let j = context.journal();
        let j = j.lock().map_err(|_| anyhow::anyhow!("journal lock"))?;
        let rows = j.list_mutations(0, 2)?;
        let uploads = j.list(0, 2)?;
        ensure!(
            rows.len() == 1
                && rows[0].id == operation
                && rows[0].state == MutationState::VerifyRequired
                && rows[0].receipt.is_none()
                && rows[0].prepared_item.as_deref() == Some(&original.id)
                && expected(&rows[0].request, &f.scope, &original)
                && uploads.len() == 1
                && uploads[0].state == UploadState::Uploaded
                && uploads[0].remote.as_ref() == Some(&original),
            "interrupted deletion journal differs"
        );
    }
    drop(context);
    drop(engine);
    record(
        &f.run_dir.join("recovery-preflight.json"),
        &serde_json::json!({"journal_requires_verification":true,"exact_prepared_identity":true}),
    )?;
    let boundary = Boundary::new(&f, Some(operation));
    let session = mount_with_all_hooks(&f, None, None, None, Some(boundary.clone())).await?;
    let result: Result<()> = async {
        completed(&session, operation, &original).await?;
        absence(&f).await?;
        let mut remote =
            ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
        ensure!(
            remote.exact_item_in_trash(&original.id).await?,
            "deleted identity absent from Trash"
        );
        ensure!(
            !remote
                .list_folder(&f.parent.id)
                .await?
                .iter()
                .any(|n| n.drivewsid == original.id || n.display_name() == NAME),
            "deleted file still active"
        );
        ensure!(
            boundary.replays.load(Ordering::SeqCst) == 0,
            "deletion replay attempted"
        );
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    let session = mount_with_all_hooks(&f, None, None, None, Some(boundary.clone())).await?;
    let result = async {
        completed(&session, operation, &original).await?;
        absence(&f).await
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    ensure!(
        boundary.replays.load(Ordering::SeqCst) == 0,
        "remount replay attempted"
    );
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,"mounted_unlink":true,"held_read_after_unlink":true,
        "process_exit_before_journal_receipt":true,"recovery_mutation_replays":0,"exact_removed_receipt":true,
        "independent_trash_presence":true,"independent_active_absence":true,"remounted_absence":true,"permanent_deletions":0}),
    )?;
    println!(
        "Mounted deletion recovered without replay; exact Trash identity and remounted absence verified"
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn fixture(path: &Path) -> (Boundary, MutationRequest) {
        let original = Node {
            id: "owned".into(),
            parent_id: Some("parent".into()),
            name: NAME.into(),
            kind: NodeKind::File,
            size: FIRST.len() as u64,
            etag: Some("revision".into()),
            modified_unix: 0,
            content_version: None,
            package: false,
            target: None,
        };
        record(&path.join("original.json"), &original).unwrap();
        let scope = Scope {
            account: "synthetic".into(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let r = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::RemoveFile { before: original },
        };
        (
            Boundary {
                directory: path.into(),
                scope,
                forbidden: None,
                replays: AtomicUsize::new(0),
            },
            r,
        )
    }
    #[test]
    fn interruption_refuses_foreign_identity_revision_scope_and_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let (b, r) = fixture(dir.path());
        let op = Uuid::new_v4().to_string();
        assert!(b.before(&op, &r).is_ok());
        for field in [
            "scope", "id", "parent", "etag", "size", "name", "kind", "package",
        ] {
            let mut changed = r.clone();
            if field == "scope" {
                changed.scope.account = "foreign".into();
            }
            if let MutationIntent::RemoveFile { before } = &mut changed.intent {
                match field {
                    "id" => before.id = "foreign".into(),
                    "parent" => before.parent_id = Some("foreign".into()),
                    "etag" => before.etag = Some("changed".into()),
                    "size" => before.size += 1,
                    "name" => before.name = "other".into(),
                    "kind" => before.kind = NodeKind::Folder,
                    "package" => before.package = true,
                    _ => (),
                }
            }
            assert!(b.before(&op, &changed).is_err(), "{field}");
        }
        assert!(b.before("invalid", &r).is_err());
        assert!(
            b.after(
                &op,
                &r,
                &MutationReceipt::Removed {
                    item: "foreign".into()
                }
            )
            .is_err()
        );
        assert!(!dir.path().join("interrupted.json").exists());
    }
    #[test]
    fn recovery_refuses_original_and_unexpected_mutation_ids() {
        let dir = tempfile::tempdir().unwrap();
        let (mut b, r) = fixture(dir.path());
        let op = Uuid::new_v4();
        b.forbidden = Some(op);
        assert!(b.before(&op.to_string(), &r).is_err());
        assert!(b.before(&Uuid::new_v4().to_string(), &r).is_err());
        assert_eq!(b.replays.load(Ordering::SeqCst), 2);
        assert!(!dir.path().join("interrupted.json").exists());
    }
}
