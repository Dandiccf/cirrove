//! Held real mappings alongside the large metadata traversal. No cloud traffic.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

const ID: &str = "mapping-probe";
const NAME: &str = "mapped-content.bin";
const SIZE: u64 = 8192;

pub(super) fn node(revision: usize) -> Node {
    Node {
        id: ID.into(),
        parent_id: Some(format!("level-{:02}", DEPTH - 1)),
        name: NAME.into(),
        kind: NodeKind::File,
        size: SIZE,
        modified_unix: 1_700_000_000,
        etag: Some(format!("mapping-content-{revision}")),
        content_version: None,
        target: None,
    }
}
pub(super) fn paths(routes: &[PathBuf; 3]) -> [PathBuf; 3] {
    (*routes).clone().map(|route| route.join(NAME))
}

pub(super) struct Provider {
    pub generated: Arc<GeneratedLibrary>,
    pub revision: AtomicUsize,
    pub offline: AtomicBool,
    pub node_requests: AtomicU64,
    pub content_requests: AtomicU64,
}
impl Provider {
    pub fn new(generated: Arc<GeneratedLibrary>) -> Self {
        Self {
            generated,
            revision: AtomicUsize::new(0),
            offline: AtomicBool::new(false),
            node_requests: AtomicU64::new(0),
            content_requests: AtomicU64::new(0),
        }
    }
    fn check(&self, scope: &Scope, cancel: &CancellationToken) -> Result<(), ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if self.offline.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        if !matches!(scope.collection.as_str(), "capacity-drive" | "shared") {
            return Err(ProviderError::NotFound);
        }
        Ok(())
    }
}
#[async_trait]
impl MetadataProvider for Provider {
    fn provider_id(&self) -> &'static str {
        self.generated.provider_id()
    }
    async fn changes(
        &self,
        scope: &Scope,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        self.generated.changes(scope, cursor, cancel).await
    }
}
#[async_trait]
impl ReadProvider for Provider {
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        if id != ID {
            return self.generated.node(scope, id, cancel).await;
        }
        self.node_requests.fetch_add(1, Ordering::SeqCst);
        self.check(scope, cancel)?;
        Ok(node(self.revision.load(Ordering::SeqCst)))
    }
    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        self.generated.children(scope, parent, cursor, cancel).await
    }
    async fn read_range(
        &self,
        scope: &Scope,
        requested: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        self.content_requests.fetch_add(1, Ordering::SeqCst);
        self.check(scope, cancel)?;
        let revision = self.revision.load(Ordering::SeqCst);
        if requested != &node(revision) {
            return Err(ProviderError::VersionChanged);
        }
        let bias = if scope.collection == "shared" { 71 } else { 0 };
        let end = offset.saturating_add(u64::from(length)).min(SIZE);
        Ok((offset.min(SIZE)..end)
            .map(|index| ((index + 17 * (revision as u64 % 251) + bias) % 251) as u8)
            .collect())
    }
}

fn command(mode: &str, revision: usize, paths: &[PathBuf; 3]) -> Command {
    let mut command = Command::new("python3");
    command
        .args(["-u", "-c", include_str!("mappings.py"), mode])
        .arg(revision.to_string())
        .args(paths)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    command
}
pub(super) struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
impl Client {
    pub async fn start(routes: &[PathBuf; 3], revision: usize) -> Self {
        let mut child = command("hold", revision, &paths(routes)).spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            input,
            output,
        };
        client.expect("mapped").await;
        client
    }
    async fn expect(&mut self, expected: &str) {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(10), self.output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            line.trim(),
            expected,
            "mapping client did not complete its phase"
        );
    }
    pub async fn update(&mut self) {
        self.input.write_all(b"update\n").await.unwrap();
        self.expect("updated").await;
    }
    pub async fn finish(mut self) {
        self.input.write_all(b"finish\n").await.unwrap();
        self.expect("released").await;
        assert!(
            tokio::time::timeout(Duration::from_secs(10), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
}
pub(super) async fn offline(routes: &[PathBuf; 3], revision: usize, inodes: [u64; 3]) {
    let mut child = command("offline", revision, &paths(routes))
        .args(inodes.map(|inode| inode.to_string()))
        .spawn()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    assert!(result.unwrap().unwrap().success());
}
