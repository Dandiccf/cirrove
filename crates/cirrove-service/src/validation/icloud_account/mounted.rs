use super::*;
use crate::writable::WritableSession;
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, ProviderError,
    ReadProvider,
};
use std::{sync::OnceLock, time::Duration};
mod guard;
use guard::{Guarded, Owned};

struct View {
    scope: Scope,
    root: Node,
    read: ICloudDrive,
    owned: OnceLock<Owned>,
}
impl View {
    fn nodes(
        &self,
        scope: &Scope,
    ) -> std::result::Result<std::collections::HashMap<String, Node>, ProviderError> {
        if scope != &self.scope {
            return Err(ProviderError::Permission);
        }
        self.owned
            .get()
            .ok_or(ProviderError::Unavailable)?
            .nodes()
            .map_err(|_| ProviderError::Unavailable)
    }
}
#[async_trait::async_trait]
impl MetadataProvider for View {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        scope: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        self.nodes(scope)?;
        Ok(ChangePage {
            changes: vec![Change::Upsert(self.root.clone())],
            checkpoint: Checkpoint::Complete(Cursor("owned-account-mount".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for View {
    fn unknown_directories_require_fetch(&self) -> bool {
        true
    }
    fn supports_same_parent_folder_rename(&self) -> bool {
        true
    }
    fn supports_cross_parent_folder_move(&self) -> bool {
        true
    }
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        c: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        let nodes = self.nodes(scope)?;
        let node = nodes.get(id).ok_or(ProviderError::Permission)?;
        if id == self.root.id {
            return Ok(self.root.clone());
        }
        let parent = node.parent_id.as_deref().ok_or(ProviderError::Permission)?;
        if !nodes.contains_key(parent) {
            return Err(ProviderError::Permission);
        }
        self.read
            .children(scope, parent, None, c)
            .await?
            .nodes
            .into_iter()
            .find(|node| node.id == id)
            .ok_or(ProviderError::NotFound)
    }
    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        c: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        if !self
            .nodes(scope)?
            .get(parent)
            .is_some_and(|node| node.kind == NodeKind::Folder)
        {
            return Err(ProviderError::Permission);
        }
        let mut page = self.read.children(scope, parent, cursor, c).await?;
        let nodes = self.nodes(scope)?;
        page.nodes.retain(|node| {
            nodes.contains_key(&node.id)
                && node.parent_id.as_deref() == Some(parent)
                && !node.package
                && node.target.is_none()
        });
        Ok(page)
    }
    async fn read_range(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        c: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        if !self
            .nodes(scope)?
            .get(&node.id)
            .is_some_and(|known| known.kind == NodeKind::File)
        {
            return Err(ProviderError::Permission);
        }
        self.read.read_range(scope, node, offset, length, c).await
    }
}

async fn mount(f: &Fixture) -> Result<WritableSession> {
    mount_with_boundary(f, None).await
}

async fn mount_with_boundary(
    f: &Fixture,
    boundary: Option<Arc<recovery::Boundary>>,
) -> Result<WritableSession> {
    mount_with_hooks(f, boundary, None).await
}

async fn mount_with_hooks(
    f: &Fixture,
    boundary: Option<Arc<recovery::Boundary>>,
    competing: Option<Arc<competing::Boundary>>,
) -> Result<WritableSession> {
    mount_with_all_hooks(f, boundary, competing, None).await
}

async fn mount_with_relocation(
    f: &Fixture,
    relocation: Arc<relocation::Boundary>,
) -> Result<WritableSession> {
    mount_with_all_hooks(f, None, None, Some(relocation)).await
}

async fn mount_with_all_hooks(
    f: &Fixture,
    boundary: Option<Arc<recovery::Boundary>>,
    competing: Option<Arc<competing::Boundary>>,
    relocation: Option<Arc<relocation::Boundary>>,
) -> Result<WritableSession> {
    let read = Arc::new(View {
        scope: f.scope.clone(),
        root: f.parent.clone(),
        read: ICloudDrive::on_demand_from_session_snapshot(
            f.scope.clone(),
            &f.account.identity.username,
            &f.snapshot,
        )?,
        owned: OnceLock::new(),
    });
    let mut view = f.account.clone();
    // Only this freshly owned subtree is exposed by the mount. Keep the real
    // parent in the metadata index for the account router's ancestry checks.
    view.root_id = f.parent.id.clone();
    view.mount_path = f.run_dir.join("mount");
    view.poll_seconds = 3600;
    private_dir(&view.mount_path)?;
    let engine = Engine::new(view, read.clone(), f.state.clone()).await?;
    let context = WriteContext::open(&engine, &f.state).await?;
    Store::open(context.metadata_db())?.observe_node(&f.scope, &f.parent)?;
    let owned = Owned {
        scope: f.scope.clone(),
        root: f.parent.clone(),
        journal: context.journal(),
    };
    read.owned
        .set(owned.clone())
        .map_err(|_| anyhow::anyhow!("ownership already initialized"))?;
    // Writer sees the real account root; Engine's mount view is a subtree.
    let provider = Arc::new(Guarded {
        inner: ICloudWriteProvider::new(&f.account, &context)?,
        owned,
        boundary,
        competing,
        relocation,
    });
    Ok(WritableSession::mount(engine, context.journal(), provider, context.checkpoints()).await?)
}

const APP: &str = r#"
import os,sys
path=os.path.join(sys.argv[1], 'Account Router.txt')
mode=sys.argv[2]
expected=b'' if mode in ('empty','read-empty') else b'Cirrove account-router replacement\n' if mode in ('replace','read') else b'Cirrove account-router original\n'
if mode not in ('read','read-empty'):
    flags=os.O_WRONLY | (os.O_CREAT | os.O_EXCL if mode == 'create' else os.O_TRUNC)
    fd=os.open(path,flags,0o600)
    try:
        assert os.write(fd,expected)==len(expected)
        os.fsync(fd)
    finally:
        os.close(fd)
with open(path,'rb') as f:
    assert f.read()==expected
"#;

async fn app(f: &Fixture, mode: &str) -> Result<()> {
    let output = tokio::process::Command::new("python3")
        .arg("-c")
        .arg(APP)
        .arg(f.run_dir.join("mount"))
        .arg(mode)
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "mounted application failed; state retained"
    );
    Ok(())
}

async fn uploaded(session: &WritableSession, count: usize) -> Result<Node> {
    tokio::time::timeout(Duration::from_secs(900), async {
        loop {
            let rows = session.uploads(0, 16).await?;
            ensure!(
                !rows
                    .iter()
                    .any(|row| matches!(row.state, UploadState::Failed | UploadState::Conflict)),
                "mounted write requires review"
            );
            if rows.len() == count && rows.iter().all(|row| row.state == UploadState::Uploaded) {
                return rows
                    .last()
                    .and_then(|row| row.remote.clone())
                    .context("missing mounted receipt");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("mounted transfer deadline; state retained")?
}

pub async fn icloud_account_mounted(run: Uuid) -> Result<()> {
    let f = prepare(run, "mounted").await?;
    let session = mount(&f).await?;
    let result: Result<()> = async {
        app(&f, "create").await?;
        let original = uploaded(&session, 1).await?;
        verify(&f.snapshot, &f.account, &f.parent, &original, FIRST).await?;
        record(&f.run_dir.join("create-verified.json"), &original)?;
        println!("Account mount: FUSE create and independent digest verified");
        app(&f, "replace").await?;
        let current = uploaded(&session, 2).await?;
        verify(&f.snapshot, &f.account, &f.parent, &current, SECOND).await?;
        let mut remote =
            ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
        ensure!(
            current.id != original.id && remote.exact_item_in_trash(&original.id).await?,
            "mounted replacement recovery not verified"
        );
        record(&f.run_dir.join("replace-verified.json"), &current)?;
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    let reopened = mount(&f).await?;
    let result = app(&f, "read").await;
    let shutdown = reopened.shutdown().await;
    result?;
    shutdown?;
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,"mounted_create":true,"mounted_replace":true,"independent_digest":true,"original_in_trash":true,"read_after_remount":true}),
    )?;
    println!("Account mount: FUSE replacement, recovery and remounted read verified");
    Ok(())
}

/// Fresh-process read of an already confirmed zero-byte fixture; no application writes.
pub async fn icloud_account_empty_read(run: Uuid) -> Result<()> {
    let run_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state")
        .join(format!("icloud-account-empty-{run}"));
    let account: Account = serde_json::from_slice(&std::fs::read(run_dir.join("account.json"))?)?;
    let parent: Node = serde_json::from_slice(&std::fs::read(run_dir.join("owned-folder.json"))?)?;
    let node: Node = serde_json::from_slice(&std::fs::read(run_dir.join("created.json"))?)?;
    ensure!(
        parent.name == format!("Cirrove Write Validation-{run}")
            && parent.parent_id.as_deref() == Some(ROOT_ID)
            && node.parent_id.as_ref() == Some(&parent.id)
            && node.size == 0
            && node.name == NAME
            && node.kind == NodeKind::File
            && !node.package
            && node.target.is_none(),
        "invalid empty fixture"
    );
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
    let reopened = engine(&f.state, &f.account, &f.scope, &f.snapshot).await?;
    let context = WriteContext::open(&reopened, &f.state).await?;
    let rows = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .list(0, 2)?;
    ensure!(
        rows.len() == 1
            && rows[0].state == UploadState::Uploaded
            && rows[0].remote.as_ref() == Some(&node),
        "empty fixture has unconfirmed operations"
    );
    drop(context);
    drop(reopened);
    verify(&f.snapshot, &f.account, &f.parent, &node, b"").await?;
    let session = mount(&f).await?;
    let result = async {
        let output = tokio::time::timeout(Duration::from_secs(120),
            tokio::process::Command::new("python3").arg("-c").arg(
                "import os,sys; p=os.path.join(sys.argv[1], 'Account Router.txt'); assert os.stat(p).st_size == 0; f=open(p,'rb'); assert f.read(1) == b''; f.close()")
                .arg(f.run_dir.join("mount")).kill_on_drop(true).output()).await??;
        ensure!(output.status.success(), "empty mounted read failed");
        Ok::<(), anyhow::Error>(())
    }.await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    record(
        &f.run_dir.join("mount-read-verified.json"),
        &serde_json::json!({"run":run,
        "reopened_journal":true,"independent_revision_digest":true,"mounted_zero_size_and_eof":true}),
    )?;
    println!("Empty file: fresh-process journal, revision checks and mounted EOF verified");
    Ok(())
}

/// Real truncate-to-zero and refill, restricted to this run's confirmed IDs.
pub async fn icloud_account_mounted_empty_replace(run: Uuid) -> Result<()> {
    let f = prepare(run, "mounted-empty-replace").await?;
    let session = mount(&f).await?;
    let result: Result<()> = async {
        app(&f, "create").await?;
        let original = uploaded(&session, 1).await?;
        verify(&f.snapshot, &f.account, &f.parent, &original, FIRST).await?;
        record(&f.run_dir.join("original.json"), &original)?;
        app(&f, "empty").await?;
        let empty = uploaded(&session, 2).await?;
        verify(&f.snapshot, &f.account, &f.parent, &empty, b"").await?;
        let mut remote =
            ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
        ensure!(
            empty.id != original.id
                && empty.size == 0
                && remote.exact_item_in_trash(&original.id).await?,
            "truncate-to-zero recovery not verified"
        );
        app(&f, "read-empty").await?;
        record(&f.run_dir.join("empty.json"), &empty)?;
        println!(
            "Mounted empty replacement: zero-byte receipt, EOF and original Trash identity verified"
        );
        app(&f, "replace").await?;
        let refilled = uploaded(&session, 3).await?;
        verify(&f.snapshot, &f.account, &f.parent, &refilled, SECOND).await?;
        ensure!(
            refilled.id != empty.id && remote.exact_item_in_trash(&empty.id).await?,
            "refill recovery not verified"
        );
        record(&f.run_dir.join("refilled.json"), &refilled)?;
        Ok(())
    }
    .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    let reopened = mount(&f).await?;
    let result = app(&f, "read").await;
    let shutdown = reopened.shutdown().await;
    result?;
    shutdown?;
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,
        "mounted_truncate_to_zero":true,"mounted_refill_empty":true,"independent_digests":true,
        "both_predecessors_in_trash":true,"remounted_read":true}),
    )?;
    println!(
        "Mounted empty replacement: refill, zero-byte Trash identity and remounted read verified"
    );
    Ok(())
}

mod large;
pub use large::icloud_account_mounted_large;

mod atomic;
pub use atomic::icloud_account_mounted_atomic;

mod recovery;
pub use recovery::{
    icloud_account_mounted_final_interrupt, icloud_account_mounted_final_recover,
    icloud_account_mounted_interrupt, icloud_account_mounted_recover,
};

mod competing;
pub use competing::{
    icloud_account_mounted_competing, icloud_account_mounted_competing_atomic,
    icloud_account_mounted_competing_autosaves,
};

mod relocation;
pub use relocation::{
    icloud_account_mounted_relocation_interrupt, icloud_account_mounted_relocation_recover,
};
