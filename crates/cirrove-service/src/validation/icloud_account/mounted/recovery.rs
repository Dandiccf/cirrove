//! A deliberate process exit after confirmed Trash, before the worker can save
//! its next checkpoint. This module is compiled only into the explicit probe.
use super::*;
use cirrove_core::upload::{UploadError, UploadRequest, UploadStep};
use cirrove_icloud::{HandoffObserved, HandoffPlan};
use secrecy::ExposeSecret;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Stage,
    MoveOld,
    InspectInstall,
    InstallInspected,
}

// Decode only the bounded, local checkpoint envelope, never a provider body.
// Unknown/legacy phases fail closed rather than missing the chosen boundary.
fn handoff(
    checkpoint: &SecretString,
) -> cirrove_core::upload::Result<(Phase, Option<HandoffPlan>)> {
    if checkpoint.expose_secret().len() > 32 * 1024 {
        return Err(UploadError::CheckpointInvalid);
    }
    let value: serde_json::Value = serde_json::from_str(checkpoint.expose_secret())
        .map_err(|_| UploadError::CheckpointInvalid)?;
    let phase = value
        .get("phase")
        .and_then(|v| v.as_object())
        .ok_or(UploadError::CheckpointInvalid)?;
    if phase.len() != 1 {
        return Err(UploadError::CheckpointInvalid);
    }
    if phase.contains_key("Stage") {
        return Ok((Phase::Stage, None));
    }
    let saved = phase.get("Handoff").ok_or(UploadError::CheckpointInvalid)?;
    let inner = saved
        .get("inner")
        .and_then(|v| v.as_str())
        .filter(|v| v.len() <= 8192)
        .ok_or(UploadError::CheckpointInvalid)?;
    let inner: serde_json::Value =
        serde_json::from_str(inner).map_err(|_| UploadError::CheckpointInvalid)?;
    if inner.get("recovery_mode").and_then(|v| v.as_str()) != Some("trash") {
        return Err(UploadError::CheckpointInvalid);
    }
    let phase = match inner.get("phase").and_then(|v| v.as_str()) {
        Some("move_old") => Phase::MoveOld,
        Some("inspect_install") => Phase::InspectInstall,
        Some("install_inspected") => Phase::InstallInspected,
        _ => return Err(UploadError::CheckpointInvalid),
    };
    let plan = serde_json::from_value(
        saved
            .get("plan")
            .cloned()
            .ok_or(UploadError::CheckpointInvalid)?,
    )
    .map_err(|_| UploadError::CheckpointInvalid)?;
    Ok((phase, Some(plan)))
}

pub(super) struct Boundary {
    stop: bool,
    run_dir: PathBuf,
    refused_replays: AtomicUsize,
}
impl Boundary {
    fn new(run_dir: PathBuf, stop: bool) -> Arc<Self> {
        Arc::new(Self {
            stop,
            run_dir,
            refused_replays: AtomicUsize::new(0),
        })
    }
    pub fn before_commit(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> cirrove_core::upload::Result<()> {
        if !self.stop && matches!(request.intent, UploadIntent::Replace { .. }) {
            let (phase, _) = handoff(checkpoint)?;
            // Recovery must reconcile the old checkpoint to a later phase;
            // even a single attempted replay invalidates this validation arm.
            if matches!(phase, Phase::MoveOld | Phase::Stage) {
                self.refused_replays.fetch_add(1, Ordering::SeqCst);
                return Err(UploadError::Invalid);
            }
        }
        Ok(())
    }
    pub fn after_commit(
        &self,
        operation: &str,
        request: &UploadRequest,
        before: &SecretString,
        step: &UploadStep,
    ) -> cirrove_core::upload::Result<()> {
        if !self.stop || !matches!(request.intent, UploadIntent::Replace { .. }) {
            return Ok(());
        }
        let (old, _) = handoff(before)?;
        if old != Phase::MoveOld {
            return Ok(());
        }
        let UploadStep::Commit(next) = step else {
            return Err(UploadError::Invalid);
        };
        if handoff(next)?.0 != Phase::InspectInstall {
            return Err(UploadError::Invalid);
        }
        let operation = Uuid::parse_str(operation).map_err(|_| UploadError::Invalid)?;
        record(
            &self.run_dir.join("interrupted.json"),
            &serde_json::json!({
                "operation": operation, "pid": std::process::id(),
                "old_trash_confirmed": true, "next_checkpoint_not_returned": true,
            }),
        )
        .map_err(|_| UploadError::Uncertain)?;
        std::fs::File::open(&self.run_dir)
            .and_then(|f| f.sync_all())
            .map_err(|_| UploadError::Uncertain)?;
        // No destructor or orderly WritableSession shutdown: the next process
        // must recover the journal/vault exactly as a terminated process left it.
        std::process::exit(86);
    }
}

pub async fn icloud_account_mounted_interrupt(run: Uuid) -> Result<()> {
    let f = prepare(run, "mounted-recovery").await?;
    let boundary = Boundary::new(f.run_dir.clone(), true);
    let session = mount_with_boundary(&f, Some(boundary)).await?;
    let result: Result<()> = async {
        app(&f, "create").await?;
        let original = uploaded(&session, 1).await?;
        verify(&f.snapshot, &f.account, &f.parent, &original, FIRST).await?;
        record(&f.run_dir.join("original.json"), &original)?;
        println!("Recovery arm: original independently verified; starting replacement");
        app(&f, "replace").await?;
        uploaded(&session, 2).await?;
        anyhow::bail!("replacement completed without reaching the registered interruption");
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    Ok(())
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path)?;
    ensure!(bytes.len() <= 32 * 1024, "validation record too large");
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid validation record"))
}

pub async fn icloud_account_mounted_recover(run: Uuid) -> Result<()> {
    let run_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state")
        .join(format!("icloud-account-mounted-recovery-{run}"));
    let account: Account = read(&run_dir.join("account.json"))?;
    let parent: Node = read(&run_dir.join("owned-folder.json"))?;
    let original: Node = read(&run_dir.join("original.json"))?;
    let marker: serde_json::Value = read(&run_dir.join("interrupted.json"))?;
    ensure!(
        matches!(account.registration, AppRegistration::ICloud)
            && !account.enabled
            && account.access == AccessMode::ReadWrite
            && account.label == "iCloudAccountRouterValidation"
            && account.root_id == ROOT_ID
            && account.mount_path == run_dir.join("unused-mount")
            && parent.name == format!("Cirrove Write Validation-{run}")
            && parent.kind == NodeKind::Folder
            && !parent.package
            && parent.target.is_none()
            && parent.parent_id.as_deref() == Some(ROOT_ID)
            && original.parent_id.as_ref() == Some(&parent.id)
            && original.name == NAME
            && original.kind == NodeKind::File
            && !original.package
            && original.target.is_none()
            && original.size == FIRST.len() as u64
            && marker.get("old_trash_confirmed").and_then(|v| v.as_bool()) == Some(true)
            && marker
                .get("next_checkpoint_not_returned")
                .and_then(|v| v.as_bool())
                == Some(true)
            && marker
                .get("pid")
                .and_then(|v| v.as_u64())
                .is_some_and(|pid| pid != u64::from(std::process::id())),
        "invalid owned recovery fixture"
    );
    let operation = marker
        .get("operation")
        .and_then(|v| v.as_str())
        .context("missing interrupted operation")?
        .parse::<Uuid>()?;
    // One recovery arm only. A failed arm is retained for audit, not re-run.
    record(
        &run_dir.join("recovery-started.json"),
        &serde_json::json!({"pid":std::process::id()}),
    )?;
    let state = run_dir.join("state");
    let snapshot = SealedSessionVault::new(&state, &account.id)?
        .load(&account.credential_id)
        .await?
        .context("isolated session unavailable")?;
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
    let pending = {
        let journal = context.journal();
        let journal = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock failed"))?;
        let rows = journal.list(0, 3)?;
        ensure!(
            rows.len() == 2
                && rows[0].state == UploadState::Uploaded
                && rows[0].remote.as_ref() == Some(&original),
            "unexpected original receipt"
        );
        let pending = journal.get(operation)?;
        ensure!(
            pending.state == UploadState::VerifyRequired
                && pending.remote.is_none()
                && pending.identity_handoff.is_some()
                && pending.scope == f.scope
                && pending.size == SECOND.len() as u64
                && pending.sha256 == hex::encode(Sha256::digest(SECOND)),
            "interrupted operation did not retain uncertain replacement state"
        );
        use std::io::Read;
        let mut bytes = Vec::new();
        journal
            .payload(operation)?
            .take(4096)
            .read_to_end(&mut bytes)?;
        ensure!(bytes == SECOND, "pending local bytes differ");
        pending
    };
    let checkpoint = context
        .checkpoints()
        .load(&format!("upload/{operation}"))
        .await?
        .context("missing interrupted checkpoint")?;
    let (phase, plan) = handoff(&checkpoint)?;
    ensure!(
        phase == Phase::MoveOld,
        "next checkpoint was unexpectedly saved"
    );
    let plan = plan.context("missing captured handoff")?;
    drop(context);
    drop(engine);
    let mut remote =
        ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
    ensure!(
        remote.probe_inspect_trash_handoff(&plan).await? == HandoffObserved::OldAtRecovery,
        "interrupted remote identities or bytes differ"
    );
    record(
        &f.run_dir.join("recovery-preflight.json"),
        &serde_json::json!({"pending_local_bytes":true,"old_and_staged_digests":true,"saved_phase":"move_old"}),
    )?;
    let boundary = Boundary::new(f.run_dir.clone(), false);
    let session = mount_with_boundary(&f, Some(boundary.clone())).await?;
    let result: Result<()> = async {
        let current = uploaded(&session, 2).await?;
        ensure!(
            boundary.refused_replays.load(Ordering::SeqCst) == 0,
            "recovery attempted to replay Trash or staging"
        );
        verify(&f.snapshot, &f.account, &f.parent, &current, SECOND).await?;
        ensure!(
            current.id != original.id
                && remote.probe_inspect_trash_handoff(&plan).await? == HandoffObserved::Complete,
            "recovered current and backup identities or bytes differ"
        );
        app(&f, "read").await?;
        record(&f.run_dir.join("recovered.json"), &current)?;
        ensure!(
            session
                .uploads(0, 3)
                .await?
                .iter()
                .any(|r| r.id == pending.id && r.state == UploadState::Uploaded),
            "recovery changed operation identity"
        );
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    let session = mount(&f).await?;
    let result = app(&f, "read").await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,"process_interruption":true,"pending_bytes_retained":true,"old_and_new_digests":true,"trash_or_stage_replays":0,"same_operation_completed":true,"read_after_remount":true}),
    )?;
    println!(
        "Recovery arm: same operation completed, both versions verified, no Trash replay, remounted read passed"
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn checkpoint(phase: &str) -> SecretString {
        let plan = serde_json::json!({"version":3,"folder_parent_id":ROOT_ID,
            "folder_id":"FOLDER::com.apple.CloudDocs::owned","folder_name":"owned",
            "original_id":"FILE::com.apple.CloudDocs::old","original_doc_id":"old",
            "original_etag":"etag-old","staged_id":"FILE::com.apple.CloudDocs::new",
            "staged_doc_id":"new","staged_etag":"etag-new","staged_name":"staged",
            "recovery_name":"backup","target_name":NAME,
            "original_sha256":"a".repeat(64),"staged_sha256":"b".repeat(64)});
        SecretString::from(serde_json::json!({"phase":{"Handoff":{
            "inner":serde_json::json!({"phase":phase,"recovery_mode":"trash"}).to_string(),"plan":plan}}}).to_string())
    }
    fn request() -> UploadRequest {
        UploadRequest {
            scope: Scope {
                account: "owned".into(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            intent: UploadIntent::Replace {
                item: "old".into(),
                expected_etag: "etag-old".into(),
            },
            size: SECOND.len() as u64,
            sha256: hex::encode(Sha256::digest(SECOND)),
        }
    }
    #[test]
    fn recovery_refuses_replaying_the_destructive_or_staging_phase() {
        let temp = tempfile::tempdir().unwrap();
        let boundary = Boundary::new(temp.path().into(), false);
        let request = request();
        assert!(
            boundary
                .before_commit(&request, &checkpoint("move_old"))
                .is_err()
        );
        assert!(
            boundary
                .before_commit(
                    &request,
                    &SecretString::from(r#"{"phase":{"Stage":{}}}"#.to_owned())
                )
                .is_err()
        );
        assert_eq!(boundary.refused_replays.load(Ordering::SeqCst), 2);
        for phase in ["inspect_install", "install_inspected"] {
            assert!(boundary.before_commit(&request, &checkpoint(phase)).is_ok());
        }
        let mut create = request;
        create.intent = UploadIntent::Create {
            parent: "owned".into(),
            name: NAME.into(),
        };
        assert!(
            boundary
                .before_commit(
                    &create,
                    &SecretString::from("not a replacement checkpoint".to_owned())
                )
                .is_ok()
        );
    }
    #[test]
    fn unknown_legacy_ambiguous_and_oversized_checkpoints_fail_closed() {
        for value in [
            checkpoint("install_new"),
            checkpoint("unknown"),
            SecretString::from("x".repeat(32769)),
            SecretString::from("invalid secret content".to_owned()),
            SecretString::from(r#"{"phase":{"Stage":{},"Handoff":{}}}"#.to_owned()),
        ] {
            assert!(matches!(
                handoff(&value),
                Err(UploadError::CheckpointInvalid)
            ));
        }
        let value = checkpoint("move_old")
            .expose_secret()
            .replace("trash", "rename");
        assert!(handoff(&SecretString::from(value)).is_err());
    }
    #[test]
    fn unrelated_commit_steps_do_not_trigger_an_interruption() {
        let temp = tempfile::tempdir().unwrap();
        let boundary = Boundary::new(temp.path().into(), true);
        let after = UploadStep::Commit(checkpoint("install_inspected"));
        assert!(
            boundary
                .after_commit(
                    &Uuid::new_v4().to_string(),
                    &request(),
                    &checkpoint("inspect_install"),
                    &after
                )
                .is_ok()
        );
        assert!(!temp.path().join("interrupted.json").exists());
    }
    #[test]
    #[ignore = "subprocess only; intentionally exits without returning"]
    fn interruption_child() {
        let path =
            std::env::var_os("CIRROVE_RECOVERY_BOUNDARY_FIXTURE").expect("private fixture path");
        let boundary = Boundary::new(PathBuf::from(path), true);
        boundary
            .after_commit(
                "00000000-0000-4000-8000-000000000001",
                &request(),
                &checkpoint("move_old"),
                &UploadStep::Commit(checkpoint("inspect_install")),
            )
            .unwrap();
        panic!("interruption returned to the worker");
    }
    #[test]
    fn process_exit_records_the_boundary_without_returning_the_next_checkpoint() {
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "validation::icloud_account::mounted::recovery::tests::interruption_child",
                "--ignored",
            ])
            .env("CIRROVE_RECOVERY_BOUNDARY_FIXTURE", temp.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(86));
        let value: serde_json::Value = read(&temp.path().join("interrupted.json")).unwrap();
        assert_eq!(value["old_trash_confirmed"], true);
        assert_eq!(value["next_checkpoint_not_returned"], true);
        assert_ne!(value["pid"].as_u64(), Some(u64::from(std::process::id())));
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
