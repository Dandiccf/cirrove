//! Linux user service and account coordination.
pub mod accounts;
mod activity;
pub use activity::DirectoryFreshness;
pub mod content;
pub mod engine;
pub mod filesystem;
pub mod journal;
pub mod manager;
pub mod mutations;
pub mod transfers;
pub mod validation;
pub mod writable;
use anyhow::{Context, Result, bail};
use cirrove_core::{CancellationToken, MetadataProvider, ProviderError, Scope};
use cirrove_store::Store;
use serde::{Deserialize, Serialize};
use std::{
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

pub const STATUS_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    /// Additive desktop control contract; legacy daemons deserialize as zero.
    #[serde(default)]
    pub protocol_version: u32,
    pub version: String,
    pub milestone: String,
    pub indexed_feeds: u64,
    pub indexed_items: u64,
    pub active_mounts: u64,
    #[serde(default)]
    pub accounts: Vec<manager::AccountStatus>,
    /// How many times this process has returned free pages to the kernel, and
    /// how much the allocator is holding free right now.
    ///
    /// Process-wide rather than per account. Reported because their absence is
    /// what hid a real defect: the reclamation tick logged at debug against a
    /// daemon running at info, so "has it ever fired?" could not be answered
    /// from outside, and a trigger that could not fire on a read workload went
    /// unnoticed until a memory measurement went looking for a cause.
    #[serde(default)]
    pub allocator_trims: u64,
    #[serde(default)]
    pub free_arena_bytes: u64,
}

/// A pin request carried over the control socket.
///
/// The command line used to write `Store::pin` directly, which is why
/// `Engine::materialise_pin` and `Engine::pin_folder` had no caller outside their
/// tests: a process that is not the daemon has no engine to reach. Routing the
/// request to the daemon puts the budget rule, the scope resolution and the
/// fetching in the one place that owns them.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PinRequest {
    /// Account label. Empty means the only account, and is an error when there
    /// is more than one.
    #[serde(default)]
    pub label: String,
    /// Provider item id. Exactly one of `item` or `path` must be set.
    #[serde(default)]
    pub item: Option<String>,
    /// Mount-relative path, resolved by the daemon because only it can list an
    /// unindexed directory or follow a shortcut into a linked collection.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub recursive: bool,
    /// Bytes to reserve. `None` means the daemon decides from what it can see,
    /// which is the only figure that agrees with the walk.
    #[serde(default)]
    pub bytes: Option<u64>,
}
/// What the daemon actually did, as opposed to what was asked for.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PinReply {
    pub accepted: bool,
    #[serde(default)]
    pub item: String,
    #[serde(default)]
    pub reserved: u64,
    /// Files the walk found. Zero for a single-file pin.
    #[serde(default)]
    pub files: u64,
    /// False when part of the subtree is not indexed yet. The pin is still
    /// recorded and honoured for what is known; it converges as the index fills.
    #[serde(default)]
    pub complete: bool,
    /// Present when the request was refused, carrying the reason a user can act
    /// on rather than a status code.
    #[serde(default)]
    pub refusal: Option<String>,
}

pub fn state_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => {
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".local/state")
        }
    };
    if !base.is_absolute() {
        bail!("state directory must be absolute");
    }
    Ok(base.join("cirrove"))
}
pub fn socket_path() -> Result<PathBuf> {
    let base = PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set; use --socket")?,
    );
    if !base.is_absolute() {
        bail!("runtime directory must be absolute");
    }
    Ok(base.join("cirrove/control.sock"))
}
/// Never repair permissions on an arbitrary existing user directory. Refuse an
/// unsafe path and explain what the caller must supply instead.
pub fn private_dir(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!("private directory must be absolute");
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
        bail!(
            "directory must be a real private directory (mode 0700): {}",
            path.display()
        );
    }
    // /proc/self is owned by the effective user; avoid unsafe libc calls.
    if meta.uid() != std::fs::metadata("/proc/self")?.uid() {
        bail!("private directory has a different owner");
    }
    Ok(())
}

/// Metadata-only refresh. Each blocking SQLite operation is moved off the async
/// executor. Network awaits never hold a SQLite transaction or database lock.
pub async fn refresh(
    provider: &dyn MetadataProvider,
    scope: &Scope,
    db_path: &Path,
    reset: bool,
    cancel: &CancellationToken,
) -> Result<u64> {
    let path = db_path.to_owned();
    let s = scope.clone();
    let mut cursor =
        tokio::task::spawn_blocking(move || Store::open(path)?.begin(&s, reset)).await??;
    let mut pages = 0u64;
    loop {
        let page = provider.changes(scope, cursor.as_ref(), cancel).await?;
        if !page.checkpoint.complete() && Some(page.checkpoint.cursor()) == cursor.as_ref() {
            bail!("provider returned a repeated continuation cursor");
        }
        let complete = page.checkpoint.complete();
        let next = page.checkpoint.cursor().clone();
        let path = db_path.to_owned();
        let s = scope.clone();
        let expected = cursor.clone();
        tokio::task::spawn_blocking(move || Store::open(path)?.stage(&s, expected.as_ref(), &page))
            .await??;
        pages += 1;
        if complete {
            return Ok(pages);
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled.into());
        }
        cursor = Some(next);
    }
}

pub async fn status(socket: &Path) -> Result<Status> {
    request(socket, "status", None::<()>, "Cirrove status").await
}
/// Ask the daemon to pin an item. See [`PinRequest`].
pub async fn pin(socket: &Path, body: &PinRequest) -> Result<PinReply> {
    request(socket, "pin", Some(body), "Cirrove pin").await
}
/// Ask the daemon to release a pin and reclaim the space it held.
pub async fn unpin(socket: &Path, body: &PinRequest) -> Result<PinReply> {
    request(socket, "unpin", Some(body), "Cirrove unpin").await
}
async fn request<B: Serialize, R: for<'a> Deserialize<'a>>(
    socket: &Path,
    verb: &str,
    body: Option<B>,
    what: &str,
) -> Result<R> {
    let mut line = verb.to_owned();
    if let Some(body) = body {
        line.push(' ');
        line.push_str(&serde_json::to_string(&body)?);
    }
    line.push('\n');
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = UnixStream::connect(socket)
            .await
            .context("Cirrove service is not reachable")?;
        stream.write_all(line.as_bytes()).await?;
        let mut reply = Vec::new();
        stream.take(1024 * 1024 + 1).read_to_end(&mut reply).await?;
        if reply.len() > 1024 * 1024 {
            bail!("oversized control response");
        }
        Ok(serde_json::from_slice(&reply)?)
    })
    .await
    .with_context(|| format!("{what} timed out"))?
}

/// Handle a control verb other than `status`.
///
/// Errors become a reply rather than a dropped connection: a user who typed a
/// verb this daemon does not know should be told so, not left waiting.
async fn handle_control(
    verb: &str,
    body: &str,
    manager: &Option<std::sync::Arc<manager::Manager>>,
) -> Result<PinReply> {
    let Some(manager) = manager else {
        return Ok(PinReply {
            refusal: Some("this daemon manages no accounts".into()),
            ..Default::default()
        });
    };
    let request: PinRequest = match verb {
        "pin" | "unpin" => serde_json::from_str(body).context("malformed request body")?,
        other => {
            return Ok(PinReply {
                refusal: Some(format!("unknown control request {other:?}")),
                ..Default::default()
            });
        }
    };
    let engine = match manager.engine(&request.label).await {
        Ok(engine) => engine,
        Err(error) => {
            return Ok(PinReply {
                refusal: Some(error.to_string()),
                ..Default::default()
            });
        }
    };
    match verb {
        "pin" => engine.apply_pin_request(&request).await,
        _ => engine.apply_unpin_request(&request).await,
    }
}

/// Read one request line, bounded. A client that sends no newline, or more than
/// this, gets an error rather than a daemon that waits or allocates for it.
async fn read_request_line(stream: &mut UnixStream) -> Result<String> {
    const LIMIT: usize = 8 * 1024;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stream.read_exact(&mut byte).await.is_err() {
            bail!("control request ended without a newline");
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() > LIMIT {
            bail!("oversized control request");
        }
    }
    String::from_utf8(line).context("control request is not UTF-8")
}

struct SocketGuard {
    path: PathBuf,
    inode: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Ok(meta) = std::fs::symlink_metadata(&self.path)
            && meta.ino() == self.inode
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
/// Recover only a disconnected socket owned by this user. The caller must hold
/// the daemon ownership lock before recovery and retain it while serving.
pub async fn recover_control_socket(socket: &Path) -> Result<()> {
    private_dir(socket.parent().context("socket needs a parent directory")?)?;
    let before = match std::fs::symlink_metadata(socket) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !before.file_type().is_socket() || before.uid() != std::fs::metadata("/proc/self")?.uid() {
        bail!("control socket path is not a socket owned by this user");
    }
    match tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(socket)).await {
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => (),
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        _ => bail!("control socket is active or its state cannot be verified"),
    }
    let after = std::fs::symlink_metadata(socket)?;
    if after.ino() != before.ino() || after.dev() != before.dev() {
        bail!("control socket changed during recovery");
    }
    std::fs::remove_file(socket)?;
    Ok(())
}
pub async fn serve(db_path: PathBuf, socket: PathBuf, cancel: CancellationToken) -> Result<()> {
    serve_managed(db_path, socket, cancel, None).await
}
pub async fn serve_managed(
    db_path: PathBuf,
    socket: PathBuf,
    cancel: CancellationToken,
    manager: Option<std::sync::Arc<manager::Manager>>,
) -> Result<()> {
    private_dir(socket.parent().context("socket needs a parent directory")?)?;
    // The daemon recovers disconnected sockets while holding its ownership lock.
    // Binding itself never overwrites a path or takes over another listener.
    let listener = UnixListener::bind(&socket)
        .context("cannot bind control socket; another daemon or a stale socket may exist")?;
    let _guard = SocketGuard {
        inode: std::fs::symlink_metadata(&socket)?.ino(),
        path: socket.clone(),
    };
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let mut requests = tokio::task::JoinSet::new();
    loop {
        tokio::select! { biased;
            _=cancel.cancelled()=>break,
            Some(_)=requests.join_next(),if !requests.is_empty()=>{},
            accepted=listener.accept()=>{
                let (mut stream,_)=accepted?;
                if requests.len()>=16 {drop(stream);continue;}
                let path=db_path.clone();let manager=manager.clone();
                requests.spawn(async move {
                    let result=tokio::time::timeout(Duration::from_secs(3),async {
                        // Was a fixed seven-byte read compared against b"status\n",
                        // which is why there has only ever been one verb. Bounded
                        // line read instead; `status\n` stays byte-identical on the
                        // wire so an older client keeps working, and
                        // STATUS_PROTOCOL_VERSION does not move -- the desktop
                        // compares it for equality, so a bump would make every
                        // mismatched pair report incompatible over an added verb
                        // that changes no payload it reads.
                        let line=read_request_line(&mut stream).await?;
                        let (verb,body)=match line.split_once(' ') {
                            Some((verb,body))=>(verb,body.trim()),
                            None=>(line.as_str(),""),
                        };
                        if verb!="status" {
                            let reply=handle_control(verb,body,&manager).await;
                            stream.write_all(&serde_json::to_vec(&reply?)?).await?;
                            return Ok::<_,anyhow::Error>(());
                        }
                        let (mut feeds,mut items)=tokio::task::spawn_blocking(move || Store::open(path)?.counts()).await??;
                        let accounts=match &manager {Some(m)=>m.status.read().await.clone(),None=>vec![]};
                        if manager.is_some() {feeds=accounts.iter().map(|a|a.indexed_feeds).sum();items=accounts.iter().map(|a|a.indexed_items).sum();}
                        let active_mounts=accounts.iter().filter(|a|a.mounted).count() as u64;
                        let reply=Status{protocol_version:STATUS_PROTOCOL_VERSION,version:env!("CARGO_PKG_VERSION").into(),milestone:"readonly-preview".into(),indexed_feeds:feeds,indexed_items:items,active_mounts,accounts,allocator_trims:crate::filesystem::allocator_trims(),free_arena_bytes:cirrove_allocator::free_arena_bytes()};
                        stream.write_all(&serde_json::to_vec(&reply)?).await?;
                        Ok::<_,anyhow::Error>(())
                    }).await;
                    if !matches!(result,Ok(Ok(()))) {tracing::debug!("control request did not complete");}
                });
            }
        }
    }
    requests.abort_all();
    while requests.join_next().await.is_some() {}
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn service_answers_status_with_idle_client_and_cleans_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("run/control.sock");
        let db = dir.path().join("metadata.db");
        let cancel = CancellationToken::new();
        let task = tokio::spawn(serve(db, socket.clone(), cancel.clone()));
        tokio::time::timeout(Duration::from_secs(3), async {
            while !socket.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let _idle = UnixStream::connect(&socket).await.unwrap();
        let reply = status(&socket).await.unwrap();
        assert_eq!(reply.active_mounts, 0);
        assert_eq!(reply.indexed_items, 0);
        cancel.cancel();
        task.await.unwrap().unwrap();
        assert!(!socket.exists());
    }
    #[tokio::test]
    async fn refuses_to_overwrite_existing_socket_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run/control.sock");
        private_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"keep").unwrap();
        assert!(
            serve(
                dir.path().join("metadata.db"),
                path.clone(),
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"keep");
    }
    #[tokio::test]
    async fn socket_recovery_preserves_live_sockets_files_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run/control.sock");
        private_dir(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        assert!(recover_control_socket(&path).await.is_err());
        // Drain the recovery probe before closing the listener. Unaccepted
        // connections can leave a transient kernel state after listener close;
        // recovery intentionally refuses to unlink an uncertain socket.
        drop(
            tokio::time::timeout(Duration::from_secs(1), listener.accept())
                .await
                .unwrap()
                .unwrap(),
        );
        let client = UnixStream::connect(&path).await.unwrap();
        drop(listener.accept().await.unwrap());
        drop(client);
        drop(listener);
        // Other tests spawn processes concurrently. Between fork and exec a
        // child may briefly retain the listener's CLOEXEC descriptor after our
        // drop. Recovery must keep refusing a connectable socket during that
        // window, then succeed once the fixture is actually disconnected.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if recover_control_socket(&path).await.is_ok() {
                    break;
                }
                assert!(path.exists());
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(!path.exists());
        std::fs::write(&path, "preserve").unwrap();
        assert!(recover_control_socket(&path).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve");
        std::fs::remove_file(&path).unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "preserve").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(recover_control_socket(&path).await.is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"preserve");
    }
    struct InterruptedProvider;
    #[async_trait::async_trait]
    impl MetadataProvider for InterruptedProvider {
        fn provider_id(&self) -> &'static str {
            "fixture"
        }
        async fn changes(
            &self,
            _scope: &Scope,
            cursor: Option<&cirrove_core::Cursor>,
            _cancel: &CancellationToken,
        ) -> std::result::Result<cirrove_core::ChangePage, ProviderError> {
            if cursor.is_some() {
                return Err(ProviderError::Unavailable);
            }
            Ok(cirrove_core::ChangePage {
                changes: vec![],
                checkpoint: cirrove_core::Checkpoint::Continue(cirrove_core::Cursor(
                    "resume-here".into(),
                )),
            })
        }
    }
    #[tokio::test]
    async fn coordinator_failure_preserves_resume_cursor_and_visible_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        let scope = Scope {
            account: "a".into(),
            provider: "fixture".into(),
            collection: "d".into(),
        };
        assert!(
            refresh(
                &InterruptedProvider,
                &scope,
                &path,
                false,
                &CancellationToken::new()
            )
            .await
            .is_err()
        );
        let mut store = Store::open(&path).unwrap();
        assert!(store.cursor(&scope).unwrap().is_none());
        assert_eq!(
            store.begin(&scope, false).unwrap(),
            Some(cirrove_core::Cursor("resume-here".into()))
        );
    }
}
