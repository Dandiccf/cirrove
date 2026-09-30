//! Real competing content edit, limited to a newly owned tiny fixture.
use super::*;
use cirrove_core::upload::{UploadError, UploadRequest};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) struct Boundary {
    run_dir: PathBuf,
    scope: Scope,
    parent: Node,
    snapshot: SecretString,
    username: String,
    attempted: AtomicBool,
}
impl Boundary {
    fn new(f: &Fixture) -> Arc<Self> {
        Arc::new(Self {
            run_dir: f.run_dir.clone(),
            scope: f.scope.clone(),
            parent: f.parent.clone(),
            snapshot: f.snapshot.clone(),
            username: f.account.identity.username.clone(),
            attempted: AtomicBool::new(false),
        })
    }
    pub async fn before_commit(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> cirrove_core::upload::Result<()> {
        let UploadIntent::Replace {
            item,
            expected_etag,
        } = &request.intent
        else {
            return Ok(());
        };
        if self.attempted.load(Ordering::SeqCst) {
            return Err(UploadError::Uncertain);
        }
        let (phase, plan) = recovery::handoff(checkpoint)?;
        if phase == recovery::Phase::Stage {
            return Ok(());
        }
        if phase != recovery::Phase::MoveOld || request.scope != self.scope {
            return Err(UploadError::Invalid);
        }
        let plan = plan.ok_or(UploadError::CheckpointInvalid)?;
        let value = serde_json::to_value(&plan).map_err(|_| UploadError::Invalid)?;
        if value.get("folder_id").and_then(|v| v.as_str()) != Some(&self.parent.id)
            || value.get("folder_name").and_then(|v| v.as_str()) != Some(&self.parent.name)
            || value.get("original_id").and_then(|v| v.as_str()) != Some(item)
            || value.get("original_etag").and_then(|v| v.as_str()) != Some(expected_etag)
            || value.get("staged_sha256").and_then(|v| v.as_str()) != Some(&request.sha256)
            || request.size != SECOND.len() as u64
        {
            return Err(UploadError::Invalid);
        }
        let operation = Uuid::parse_str(operation).map_err(|_| UploadError::Invalid)?;
        if self.attempted.swap(true, Ordering::SeqCst) {
            return Err(UploadError::Uncertain);
        }
        record(
            &self.run_dir.join("competing-started.json"),
            &serde_json::json!({"operation":operation}),
        )
        .map_err(|_| UploadError::Uncertain)?;
        record(&self.run_dir.join("competing-plan.json"), &plan)
            .map_err(|_| UploadError::Uncertain)?;
        std::fs::File::open(&self.run_dir)
            .and_then(|f| f.sync_all())
            .map_err(|_| UploadError::Uncertain)?;
        let mut remote = ICloudReadSession::from_session_snapshot(&self.snapshot, &self.username)
            .map_err(|_| UploadError::Uncertain)?;
        let revised = remote
            .probe_owned_competing_edit(&plan, FIRST, SECOND)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        record(&self.run_dir.join("competing-confirmed.json"), &revised)
            .map_err(|_| UploadError::Uncertain)?;
        println!(
            "Competing edit independently confirmed; returning to the normal replacement adapter"
        );
        Ok(())
    }
}

async fn conflicted(session: &WritableSession) -> Result<crate::journal::UploadRecord> {
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let rows = session.uploads(0, 10).await?;
            ensure!(rows.len() == 2, "unexpected competing-edit queue");
            let latest = rows.last().context("missing competing save")?;
            if latest.state == UploadState::Conflict {
                return Ok(latest.clone());
            }
            ensure!(
                !matches!(
                    latest.state,
                    UploadState::Failed | UploadState::Uploaded | UploadState::Resolved
                ),
                "replacement did not retain a conflict"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("competing-edit deadline; state retained")?
}

async fn both_paths(f: &Fixture) -> Result<()> {
    let mount = f.run_dir.join("mount");
    let output = tokio::process::Command::new("python3").arg("-c").arg(r#"
import pathlib,sys,time
p=pathlib.Path(sys.argv[1]); deadline=time.monotonic()+30
cloud=bytearray(b'Cirrove account-router original\n'); cloud[0]^=1
while True:
    try:
        assert (p/'Account Router.txt').read_bytes()==cloud
        assert (p/'Account Router rescued.txt').read_bytes()==b'Cirrove account-router replacement\n'
        break
    except (AssertionError,FileNotFoundError):
        if time.monotonic()>=deadline: raise
        time.sleep(.1)
"#).arg(mount).kill_on_drop(true).output();
    let result = tokio::time::timeout(Duration::from_secs(60), output)
        .await
        .context("mounted competing reads timed out")??;
    ensure!(
        result.status.success(),
        "mounted versions did not remain independently readable"
    );
    Ok(())
}

pub async fn icloud_account_mounted_competing(run: Uuid) -> Result<()> {
    let f = prepare(run, "mounted-competing").await?;
    let boundary = Boundary::new(&f);
    let session = mount_with_hooks(&f, None, Some(boundary)).await?;
    let result: Result<(Uuid, Node)> = async {
        app(&f, "create").await?;
        let original = uploaded(&session, 1).await?;
        verify(&f.snapshot, &f.account, &f.parent, &original, FIRST).await?;
        record(&f.run_dir.join("original.json"), &original)?;
        app(&f, "replace").await?;
        let refused = conflicted(&session).await?;
        ensure!(refused.sha256 == hex::encode(Sha256::digest(SECOND)) && refused.size == SECOND.len() as u64, "conflicted payload differs");
        app(&f, "read").await?;
        let bytes = std::fs::read(f.run_dir.join("competing-confirmed.json"))?;
        ensure!(bytes.len() <= 8192, "revised receipt too large");
        let revised: Node = serde_json::from_slice(&bytes)?;
        ensure!(revised.id == original.id && revised.etag != original.etag, "competing edit changed identity or not revision");
        let mut expected = FIRST.to_vec(); expected[0] ^= 1;
        verify(&f.snapshot, &f.account, &f.parent, &revised, &expected).await?;
        let mut remote = ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
        remote.probe_exact_item_absent_from_trash(&original.id).await?;
        record(&f.run_dir.join("conflict-verified.json"), &serde_json::json!({"operation":refused.id,"cloud_preserved":true,"local_preserved":true}))?;
        println!("Normal mounted writer retained conflict and both full contents; restarting the isolated mount");
        Ok((refused.id, revised))
    }.await;
    let shutdown = session.shutdown().await;
    let (refused, revised) = result?;
    shutdown?;
    let session = mount(&f).await?;
    let result: Result<()> = async {
        ensure!(
            session
                .keep_both(vec![(
                    refused,
                    f.parent.id.clone(),
                    "Account Router rescued.txt".into()
                )])
                .await?
                == 1,
            "keep both did not resolve the owned conflict"
        );
        both_paths(&f).await?;
        let copy = tokio::time::timeout(Duration::from_secs(600), async {
            loop {
                let rows = session.uploads(0, 10).await?;
                ensure!(
                    rows.len() == 3
                        && rows
                            .iter()
                            .any(|r| r.id == refused && r.state == UploadState::Resolved),
                    "unexpected rescue queue"
                );
                let last = rows.last().context("rescue missing")?;
                if last.state == UploadState::Uploaded {
                    return last.remote.clone().context("rescue receipt missing");
                }
                ensure!(
                    !matches!(last.state, UploadState::Failed | UploadState::Conflict),
                    "rescue upload refused"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .context("rescue upload deadline")??;
        ensure!(
            copy.id != revised.id
                && copy.name == "Account Router rescued.txt"
                && copy.parent_id == revised.parent_id,
            "rescue identity or name differs"
        );
        let mut remote =
            ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
        let digest = remote
            .hash_file_in_folder_for_revision(
                &f.parent.id,
                &copy.id,
                copy.etag.as_deref().context("rescue revision missing")?,
                copy.size,
            )
            .await?;
        ensure!(
            digest == hex::encode(Sha256::digest(SECOND)),
            "remote rescue digest differs"
        );
        let mut expected = FIRST.to_vec();
        expected[0] ^= 1;
        verify(&f.snapshot, &f.account, &f.parent, &revised, &expected).await?;
        record(&f.run_dir.join("rescue-verified.json"), &copy)?;
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    let session = mount(&f).await?;
    let result = both_paths(&f).await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,"real_competing_content_edit":true,"normal_adapter_conflict":true,"local_payload_preserved":true,"cloud_content_preserved":true,"keep_both_after_restart":true,"rescue_uploaded_and_hashed":true,"both_read_after_remount":true,"staging_cleanup":"retained_pending_separate_validation"}),
    )?;
    println!(
        "Mounted competing-edit test passed; both versions independently verified after remount"
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn boundary(path: &Path) -> Boundary {
        Boundary {
            run_dir: path.into(),
            scope: Scope {
                account: "test".into(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            parent: Node {
                id: "folder".into(),
                parent_id: Some(ROOT_ID.into()),
                name: "owned".into(),
                kind: NodeKind::Folder,
                size: 0,
                modified_unix: 0,
                etag: Some("tag".into()),
                content_version: None,
                target: None,
                package: false,
            },
            snapshot: SecretString::from("invalid synthetic snapshot"),
            username: "synthetic".into(),
            attempted: AtomicBool::new(false),
        }
    }
    fn request(b: &Boundary) -> UploadRequest {
        UploadRequest {
            scope: b.scope.clone(),
            intent: UploadIntent::Replace {
                item: "old".into(),
                expected_etag: "tag".into(),
            },
            size: SECOND.len() as u64,
            sha256: hex::encode(Sha256::digest(SECOND)),
        }
    }
    fn checkpoint() -> SecretString {
        SecretString::from(serde_json::json!({"phase":{"Handoff":{"inner":serde_json::json!({"phase":"move_old","recovery_mode":"trash"}).to_string(),"plan":{
            "version":3,"folder_parent_id":ROOT_ID,"folder_id":"folder","folder_name":"owned","original_id":"old","original_doc_id":"old","original_etag":"tag","staged_id":"new","staged_doc_id":"new","staged_etag":"new-tag","staged_name":"stage","recovery_name":"recovery","target_name":NAME,"original_sha256":hex::encode(Sha256::digest(FIRST)),"staged_sha256":hex::encode(Sha256::digest(SECOND))
        }}}}).to_string())
    }
    #[tokio::test]
    async fn failed_injection_is_durable_and_never_reentered() {
        let temp = tempfile::tempdir().unwrap();
        let b = boundary(temp.path());
        let first = Uuid::new_v4().to_string();
        assert!(matches!(
            b.before_commit(&first, &request(&b), &checkpoint()).await,
            Err(UploadError::Uncertain)
        ));
        let saved = std::fs::read(temp.path().join("competing-started.json")).unwrap();
        assert!(
            b.before_commit(&Uuid::new_v4().to_string(), &request(&b), &checkpoint())
                .await
                .is_err()
        );
        assert_eq!(
            saved,
            std::fs::read(temp.path().join("competing-started.json")).unwrap()
        );
        assert!(!temp.path().join("competing-confirmed.json").exists());
    }
    #[tokio::test]
    async fn foreign_scope_and_malformed_checkpoint_never_arm_the_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let b = boundary(temp.path());
        let mut r = request(&b);
        r.scope.account = "foreign".into();
        assert!(
            b.before_commit(&Uuid::new_v4().to_string(), &r, &checkpoint())
                .await
                .is_err()
        );
        assert!(
            b.before_commit(
                &Uuid::new_v4().to_string(),
                &request(&b),
                &SecretString::from("bad")
            )
            .await
            .is_err()
        );
        assert!(!b.attempted.load(Ordering::SeqCst));
        assert!(!temp.path().join("competing-started.json").exists());
    }
}
