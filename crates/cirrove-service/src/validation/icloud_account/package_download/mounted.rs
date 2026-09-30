//! A captured package archive through normal Engine publication and read-only FUSE.
use super::*;
use crate::filesystem::CloudFs;
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, ReadProvider,
    reads::ReadSession,
};
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

struct View {
    scope: Scope,
    root: Node,
    artifact: Node,
    source: Arc<dyn ReadSession>,
    offline: AtomicBool,
    staged: AtomicUsize,
    unexpected_reads: AtomicUsize,
}
impl View {
    fn check(&self, scope: &Scope) -> std::result::Result<(), ProviderError> {
        if scope != &self.scope {
            return Err(ProviderError::Permission);
        }
        if self.offline.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        Ok(())
    }
}
#[async_trait]
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
        self.check(scope)?;
        Ok(ChangePage {
            changes: vec![Change::Upsert(self.root.clone())],
            checkpoint: Checkpoint::Complete(Cursor("captured-package".into())),
        })
    }
}
#[async_trait]
impl ReadProvider for View {
    fn unknown_directories_require_fetch(&self) -> bool {
        true
    }
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        self.check(scope)?;
        if id == self.root.id {
            Ok(self.root.clone())
        } else {
            Err(ProviderError::NotFound)
        }
    }
    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        self.check(scope)?;
        if parent != self.root.id || cursor.is_some() {
            return Err(ProviderError::Permission);
        }
        Ok(DirectoryPage {
            nodes: vec![self.artifact.clone()],
            next: None,
        })
    }
    async fn staged_content_session(
        &self,
        scope: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<Arc<dyn ReadSession>>, ProviderError> {
        self.check(scope)?;
        if node != &self.artifact {
            return Err(ProviderError::Permission);
        }
        self.staged.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.source.clone()))
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.unexpected_reads.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Unavailable)
    }
}
const APP: &str = r#"
import os,sys,hashlib
path=os.path.join(sys.argv[1], 'Package Preview.pages')
assert os.statvfs(sys.argv[1]).f_flag & os.ST_RDONLY
assert os.stat(path).st_size==int(sys.argv[2])
h=hashlib.sha256()
with open(path,'rb',buffering=0) as f:
    while True:
        b=f.read(65536)
        if not b: break
        h.update(b)
assert h.hexdigest()==sys.argv[3]
"#;

async fn arm(
    account: Account,
    view: Arc<View>,
    state: &Path,
    artifact: &cirrove_icloud::PackageDownload,
) -> Result<()> {
    let mount = account.mount_path.clone();
    private_dir(&mount)?;
    let engine = Engine::new(account, view, state.to_owned()).await?;
    engine.start().await?;
    let session = CloudFs::new(engine.clone())?.mount(&mount)?;
    let result: Result<()> = async {
        let fs = tokio::process::Command::new("findmnt")
            .args(["-n", "-o", "FSTYPE", "-T"])
            .arg(&mount)
            .output()
            .await?;
        ensure!(
            fs.status.success() && fs.stdout == b"fuse.cirrove\n",
            "package preview is not the expected FUSE mount"
        );
        let app = tokio::time::timeout(
            Duration::from_secs(180),
            tokio::process::Command::new("python3")
                .arg("-c")
                .arg(APP)
                .arg(&mount)
                .arg(artifact.size.to_string())
                .arg(&artifact.sha256)
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        ensure!(
            app.status.success(),
            "mounted package application failed; state retained"
        );
        Ok(())
    }
    .await;
    engine.stop().await;
    let unmounted = tokio::task::spawn_blocking(move || session.umount_and_join()).await?;
    result?;
    unmounted?;
    ensure!(
        std::fs::read_dir(&mount)?.next().is_none(),
        "package preview mount did not detach cleanly"
    );
    Ok(())
}

pub(super) async fn verify_mount(
    run_dir: &Path,
    session_run: Uuid,
    run: Uuid,
    scope: Scope,
    node: Node,
    source: Arc<dyn ReadSession>,
    artifact: &cirrove_icloud::PackageDownload,
) -> Result<()> {
    let account_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state")
        .join(format!(
            "icloud-account-mounted-empty-replace-{session_run}"
        ))
        .join("account.json");
    let mut account: Account = serde_json::from_slice(&std::fs::read(account_path)?)?;
    account.id = run.to_string();
    account.enabled = false;
    account.access = AccessMode::ReadOnly;
    account.mount_path = run_dir.join("mount");
    account.root_id = ROOT_ID.into();
    account.drive.id = scope.collection.clone();
    account.poll_seconds = 3600;
    account.cache_bytes = 64 * 1024 * 1024;
    let root = Node {
        id: ROOT_ID.into(),
        parent_id: None,
        name: "Captured Package".into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: Some("captured".into()),
        content_version: None,
        target: None,
        package: false,
    };
    let view = Arc::new(View {
        scope,
        root,
        artifact: node,
        source,
        offline: AtomicBool::new(false),
        staged: AtomicUsize::new(0),
        unexpected_reads: AtomicUsize::new(0),
    });
    let state = run_dir.join("mounted-state");
    arm(account.clone(), view.clone(), &state, artifact).await?;
    ensure!(
        view.staged.load(Ordering::SeqCst) == 1,
        "mounted package was not staged exactly once"
    );
    view.offline.store(true, Ordering::SeqCst);
    arm(account, view.clone(), &state, artifact).await?;
    ensure!(
        view.staged.load(Ordering::SeqCst) == 1
            && view.unexpected_reads.load(Ordering::SeqCst) == 0,
        "offline remount requested provider content"
    );
    Ok(())
}
