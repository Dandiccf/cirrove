//! Mounted combined relocation, with process loss after the final cloud receipt.
use super::*;
use crate::journal::{MutationRecord, MutationState};
use cirrove_core::mutation::{MutationError, MutationIntent, MutationReceipt, MutationRequest};
use cirrove_icloud::SealedFolderCheckpointVault;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
mod folders;

const KIND: &str = "mounted-relocation-recovery";
#[derive(Serialize, Deserialize)]
struct Setup {
    original: Node,
    destination: Node,
    blocker: Node,
    source_blocker: Node,
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path)?;
    ensure!(bytes.len() <= 32 * 1024, "validation record too large");
    serde_json::from_slice(&bytes).context("invalid validation record")
}
fn target(r: &MutationRequest) -> bool {
    matches!(&r.intent, MutationIntent::Relocate { before, parent, name }
        if before.kind == NodeKind::File && before.name == NAME && name == "Combined.txt"
            && before.parent_id.as_deref() != Some(parent))
}
fn expected(r: &MutationRequest, setup: &Setup) -> bool {
    matches!(&r.intent, MutationIntent::Relocate { before, parent, name }
        if before.id == setup.original.id && before.parent_id == setup.original.parent_id
            && before.name == NAME && before.etag == setup.original.etag
            && before.kind == NodeKind::File && !before.package && before.target.is_none()
            && before.size == FIRST.len() as u64
            && parent == &setup.destination.id && name == "Combined.txt")
}
pub(super) struct Boundary {
    run_dir: PathBuf,
    scope: Scope,
    stop: bool,
    forbidden: Option<Uuid>,
    replays: AtomicUsize,
}
impl Boundary {
    fn new(f: &Fixture, stop: bool, forbidden: Option<Uuid>) -> Arc<Self> {
        Arc::new(Self {
            run_dir: f.run_dir.clone(),
            scope: f.scope.clone(),
            stop,
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
        if self.forbidden == Some(id) {
            self.replays.fetch_add(1, Ordering::SeqCst);
            return Err(MutationError::Invalid);
        }
        if self.stop && target(r) {
            let setup: Setup =
                read(&self.run_dir.join("setup.json")).map_err(|_| MutationError::Invalid)?;
            if r.scope != self.scope || !expected(r, &setup) {
                return Err(MutationError::Invalid);
            }
        }
        Ok(())
    }
    pub fn after(
        &self,
        operation: &str,
        r: &MutationRequest,
        receipt: &MutationReceipt,
    ) -> cirrove_core::mutation::Result<()> {
        if !self.stop || !target(r) {
            return Ok(());
        }
        let setup: Setup =
            read(&self.run_dir.join("setup.json")).map_err(|_| MutationError::Invalid)?;
        let MutationReceipt::Upsert(node) = receipt else {
            return Err(MutationError::Invalid);
        };
        if r.scope != self.scope
            || !expected(r, &setup)
            || node.id != setup.original.id
            || node.parent_id.as_ref() != Some(&setup.destination.id)
            || node.name != "Combined.txt"
            || node.kind != NodeKind::File
            || node.size != FIRST.len() as u64
            || node.package
            || node.target.is_some()
        {
            return Err(MutationError::Invalid);
        }
        let operation = Uuid::parse_str(operation).map_err(|_| MutationError::Invalid)?;
        record(
            &self.run_dir.join("interrupted.json"),
            &serde_json::json!({
                "operation":operation,"pid":std::process::id(),"acknowledgement_not_returned":true,
                "boundary":"combined_complete_before_ack","receipt":node
            }),
        )
        .map_err(|_| MutationError::Uncertain)?;
        std::fs::File::open(&self.run_dir)
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
    .await
    .context("relocation application deadline; state retained")??;
    ensure!(
        output.status.success(),
        "relocation application failed; state retained"
    );
    Ok(())
}
async fn mutations(session: &WritableSession, count: usize) -> Result<Vec<MutationRecord>> {
    tokio::time::timeout(Duration::from_secs(900), async {
        loop {
            let rows = session.mutations(0, 32).await?;
            ensure!(
                !rows.iter().any(|r| matches!(
                    r.state,
                    MutationState::Conflict | MutationState::Failed | MutationState::NeedsReview
                )),
                "relocation requires review; state retained"
            );
            if rows.len() == count && rows.iter().all(|r| r.state == MutationState::Applied) {
                return Ok(rows);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("relocation confirmation deadline; state retained")?
}
fn upsert(rows: &[MutationRecord], parent: &str, name: &str) -> Result<Node> {
    let nodes: Vec<_> = rows
        .iter()
        .filter_map(|r| match &r.receipt {
            Some(MutationReceipt::Upsert(n))
                if n.name == name && n.parent_id.as_deref() == Some(parent) =>
            {
                Some(n.clone())
            }
            _ => None,
        })
        .collect();
    ensure!(nodes.len() == 1, "ambiguous relocation fixture receipt");
    Ok(nodes[0].clone())
}
async fn hash(f: &Fixture, node: &Node, expected: &[u8]) -> Result<()> {
    let mut remote =
        ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
    let digest = remote
        .hash_file_in_folder_for_revision(
            node.parent_id.as_deref().context("parent missing")?,
            &node.id,
            node.etag.as_deref().context("revision missing")?,
            node.size,
        )
        .await?;
    ensure!(
        digest == hex::encode(Sha256::digest(expected)),
        "relocated full content differs"
    );
    Ok(())
}
async fn file_paths(f: &Fixture) -> Result<()> {
    application(f, r#"
import pathlib,sys
p=pathlib.Path(sys.argv[1])
assert not (p/'Account Router.txt').exists()
assert (p/'Combined.txt').is_dir()
assert (p/'Destination/Combined.txt').read_bytes()==b'Cirrove account-router original\n'
assert (p/'Destination/Account Router.txt').read_bytes()==b'Cirrove account-router replacement\n'
assert not any(n.name.startswith('.cirrove-move-') for d in (p,p/'Destination') for n in d.iterdir())
"#).await
}

pub async fn icloud_account_mounted_relocation_interrupt(run: Uuid) -> Result<()> {
    let f = prepare(run, KIND).await?;
    let session = mount_with_relocation(&f, Boundary::new(&f, true, None)).await?;
    let result: Result<()> = async {
        application(&f, r#"
import pathlib,sys
p=pathlib.Path(sys.argv[1]); (p/'Destination').mkdir(); (p/'Combined.txt').mkdir()
"#).await?;
        let folders = mutations(&session,2).await?;
        let destination = upsert(&folders,&f.parent.id,"Destination")?;
        let source_blocker = upsert(&folders,&f.parent.id,"Combined.txt")?;
        application(&f,r#"
import os,pathlib,sys
p=pathlib.Path(sys.argv[1])
for path,data in [(p/'Account Router.txt',b'Cirrove account-router original\n'),(p/'Destination/Account Router.txt',b'Cirrove account-router replacement\n')]:
    with path.open('xb') as f:
        f.write(data); f.flush(); os.fsync(f.fileno())
"#).await?;
        uploaded(&session,2).await?;
        let uploads = session.uploads(0,8).await?;
        let receipts: Vec<Node> = uploads.into_iter().filter_map(|r| r.remote).collect();
        let original = receipts.iter().find(|n| n.parent_id.as_ref()==Some(&f.parent.id)).context("original receipt missing")?.clone();
        let blocker = receipts.iter().find(|n| n.parent_id.as_ref()==Some(&destination.id)).context("blocker receipt missing")?.clone();
        hash(&f,&original,FIRST).await?; hash(&f,&blocker,SECOND).await?;
        record(&f.run_dir.join("setup.json"), &Setup { original,destination,blocker,source_blocker })?;
        println!("Mounted relocation setup confirmed; both naive orderings collide");
        application(&f,r#"
import os,pathlib,sys
p=pathlib.Path(sys.argv[1]); old=(p/'Account Router.txt').open('rb')
os.rename(p/'Account Router.txt',p/'Destination/Combined.txt')
assert old.read()==b'Cirrove account-router original\n'; old.close()
assert (p/'Destination/Combined.txt').read_bytes()==b'Cirrove account-router original\n'
"#).await?;
        mutations(&session,3).await?;
        anyhow::bail!("relocation completed without the registered process interruption")
    }.await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    Ok(())
}

pub async fn icloud_account_mounted_relocation_recover(run: Uuid) -> Result<()> {
    let run_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state")
        .join(format!("icloud-account-{KIND}-{run}"));
    let account: Account = read(&run_dir.join("account.json"))?;
    let parent: Node = read(&run_dir.join("owned-folder.json"))?;
    let setup: Setup = read(&run_dir.join("setup.json"))?;
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
            && setup.original.parent_id.as_ref() == Some(&parent.id)
            && setup.original.name == NAME
            && setup.original.kind == NodeKind::File
            && setup.original.size == FIRST.len() as u64
            && setup.destination.parent_id.as_ref() == Some(&parent.id)
            && setup.destination.name == "Destination"
            && setup.destination.kind == NodeKind::Folder
            && setup.source_blocker.parent_id.as_ref() == Some(&parent.id)
            && setup.source_blocker.name == "Combined.txt"
            && setup.source_blocker.kind == NodeKind::Folder
            && setup.blocker.parent_id.as_ref() == Some(&setup.destination.id)
            && setup.blocker.name == NAME
            && setup.blocker.kind == NodeKind::File
            && marker["boundary"].as_str() == Some("combined_complete_before_ack")
            && marker["acknowledgement_not_returned"].as_bool() == Some(true)
            && marker["pid"]
                .as_u64()
                .is_some_and(|pid| pid != u64::from(std::process::id())),
        "invalid owned relocation recovery fixture"
    );
    let operation = Uuid::parse_str(
        marker["operation"]
            .as_str()
            .context("missing interrupted operation")?,
    )?;
    let receipt: Node = serde_json::from_value(marker["receipt"].clone())?;
    ensure!(
        receipt.id == setup.original.id
            && receipt.parent_id.as_ref() == Some(&setup.destination.id)
            && receipt.name == "Combined.txt"
            && receipt.kind == NodeKind::File
            && receipt.size == FIRST.len() as u64
            && !receipt.package
            && receipt.target.is_none(),
        "interrupted receipt differs"
    );
    record(
        &run_dir.join("recovery-started.json"),
        &serde_json::json!({"pid":std::process::id()}),
    )?;
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
    let pending = {
        let j = context.journal();
        let j = j
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock failed"))?;
        let rows = j.list_mutations(0, 8)?;
        let pending = j.mutation(operation)?;
        ensure!(
            rows.len() == 3
                && rows
                    .iter()
                    .filter(|r| r.state == MutationState::Applied)
                    .count()
                    == 2
                && pending.state == MutationState::VerifyRequired
                && pending.receipt.is_none()
                && pending.request.scope == f.scope
                && expected(&pending.request, &setup),
            "interrupted journal differs"
        );
        ensure!(
            j.list(0, 8)?
                .iter()
                .all(|r| r.state == UploadState::Uploaded),
            "unexpected pending upload"
        );
        pending
    };
    let sealed = SealedFolderCheckpointVault::new(&f.state, &f.account.id)?
        .load(&format!("icloud-folder-plan/{}/{operation}", f.account.id))
        .await?
        .context("sealed relocation plan missing")?;
    ensure!(
        sealed.expose_secret().len() <= 32 * 1024,
        "relocation plan too large"
    );
    let plan: serde_json::Value =
        serde_json::from_str(sealed.expose_secret()).context("invalid sealed relocation plan")?;
    ensure!(
        plan["phase"].as_str() == Some("complete")
            && plan["step"].as_u64() == Some(3)
            && plan["operation"].as_str() == Some(&operation.to_string())
            && plan["request"] == serde_json::to_value(&pending.request)?
            && plan["current"] == serde_json::to_value(&receipt)?,
        "relocation plan did not durably complete before acknowledgement loss"
    );
    drop(context);
    drop(engine);
    hash(&f, &receipt, FIRST).await?;
    hash(&f, &setup.blocker, SECOND).await?;
    record(
        &f.run_dir.join("recovery-preflight.json"),
        &serde_json::json!({"journal_requires_verification":true,"sealed_three_step_plan_complete":true,"remote_digest_verified":true,"blocker_preserved":true}),
    )?;
    let boundary = Boundary::new(&f, false, Some(operation));
    let session = mount_with_relocation(&f, boundary.clone()).await?;
    let result: Result<()> = async {
        let rows = mutations(&session, 3).await?;
        let recovered = rows
            .iter()
            .find(|r| r.id == operation)
            .context("recovered operation missing")?;
        ensure!(
            matches!(&recovered.receipt,Some(MutationReceipt::Upsert(n)) if n==&receipt)
                && boundary.replays.load(Ordering::SeqCst) == 0,
            "recovery changed receipt or replayed relocation"
        );
        file_paths(&f).await?;
        record(&f.run_dir.join("recovered-file.json"), &receipt)?;
        println!("Combined mounted file move recovered by inspection without mutation replay");
        folders::run(&f, &session, &setup.destination).await?;
        ensure!(
            boundary.replays.load(Ordering::SeqCst) == 0,
            "relocation replay attempted"
        );
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    let session = mount(&f).await?;
    let result: Result<()> = async {
        mutations(&session, 7).await?;
        file_paths(&f).await?;
        folders::paths(&f).await?;
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,"mounted_combined_file":true,
        "file_final_acknowledgement_loss":true,"recovery_mutation_replays":boundary.replays.load(Ordering::SeqCst),
        "mounted_combined_populated_folder":true,"naive_order_collisions_preserved":true,
        "independent_remote_digests":true,"remounted_paths":true,"permanent_deletions":0}),
    )?;
    println!("Combined mounted file recovery and populated-folder move passed after remount");
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn node(id: &str, parent: &str, name: &str, kind: NodeKind) -> Node {
        Node {
            id: id.into(),
            parent_id: Some(parent.into()),
            name: name.into(),
            kind,
            size: FIRST.len() as u64,
            etag: Some("tag".into()),
            content_version: None,
            modified_unix: 0,
            target: None,
            package: false,
        }
    }
    fn fixture(path: &Path) -> (Boundary, MutationRequest) {
        let scope = Scope {
            account: "synthetic".into(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let setup = Setup {
            original: node("original", "root", NAME, NodeKind::File),
            destination: node("destination", "root", "Destination", NodeKind::Folder),
            blocker: node("blocker", "destination", NAME, NodeKind::File),
            source_blocker: node("source-blocker", "root", "Combined.txt", NodeKind::Folder),
        };
        record(&path.join("setup.json"), &setup).unwrap();
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::Relocate {
                before: setup.original,
                parent: "destination".into(),
                name: "Combined.txt".into(),
            },
        };
        (
            Boundary {
                run_dir: path.into(),
                scope,
                stop: true,
                forbidden: None,
                replays: AtomicUsize::new(0),
            },
            request,
        )
    }
    #[test]
    fn interruption_requires_the_owned_source_scope_revision_and_final_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let (b, r) = fixture(temp.path());
        let id = Uuid::new_v4().to_string();
        assert!(b.before(&id, &r).is_ok());
        for field in ["scope", "id", "etag", "parent", "size"] {
            let mut other = r.clone();
            if field == "scope" {
                other.scope.account = "foreign".into();
            }
            if let MutationIntent::Relocate { before, parent, .. } = &mut other.intent {
                match field {
                    "id" => before.id = "foreign".into(),
                    "etag" => before.etag = Some("changed".into()),
                    "parent" => *parent = "foreign".into(),
                    "size" => before.size += 1,
                    _ => (),
                }
            }
            assert!(b.before(&id, &other).is_err(), "{field}");
        }
        assert!(b.before("invalid", &r).is_err());
        let wrong = MutationReceipt::Upsert(node(
            "foreign",
            "destination",
            "Combined.txt",
            NodeKind::File,
        ));
        assert!(b.after(&id, &r, &wrong).is_err());
        assert!(!temp.path().join("interrupted.json").exists());
    }
    #[test]
    fn recovery_refuses_the_exact_interrupted_operation_before_any_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        let (mut b, r) = fixture(temp.path());
        let id = Uuid::new_v4();
        b.stop = false;
        b.forbidden = Some(id);
        assert!(b.before(&id.to_string(), &r).is_err());
        assert_eq!(b.replays.load(Ordering::SeqCst), 1);
        assert!(b.before(&Uuid::new_v4().to_string(), &r).is_ok());
        assert_eq!(b.replays.load(Ordering::SeqCst), 1);
        assert!(!temp.path().join("interrupted.json").exists());
    }
}
