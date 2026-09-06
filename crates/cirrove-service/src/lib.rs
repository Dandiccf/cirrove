//! Linux service plumbing; the daemon does not mount or modify cloud files yet.
use anyhow::{Context, Result, bail};
use cirrove_core::{CancellationToken, MetadataProvider, ProviderError, Scope};
use cirrove_store::Store;
use serde::{Deserialize, Serialize};
use std::{
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub milestone: String,
    pub indexed_feeds: u64,
    pub indexed_items: u64,
    pub active_mounts: u64,
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
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = UnixStream::connect(socket)
            .await
            .context("Cirrove service is not reachable")?;
        stream.write_all(b"status\n").await?;
        let mut body = Vec::new();
        stream.take(8193).read_to_end(&mut body).await?;
        if body.len() > 8192 {
            bail!("oversized control response");
        }
        Ok(serde_json::from_slice(&body)?)
    })
    .await
    .context("Cirrove status timed out")?
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
pub async fn serve(db_path: PathBuf, socket: PathBuf, cancel: CancellationToken) -> Result<()> {
    private_dir(socket.parent().context("socket needs a parent directory")?)?;
    // Refuse existing paths, including stale sockets. systemd RuntimeDirectory
    // handles cleanup for the packaged service; manual runs must resolve leftovers.
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
                let path=db_path.clone();
                requests.spawn(async move {
                    let result=tokio::time::timeout(Duration::from_secs(3),async {
                        let mut command=[0u8;7]; stream.read_exact(&mut command).await?;
                        if &command!=b"status\n" {bail!("unknown control request");}
                        let (feeds,items)=tokio::task::spawn_blocking(move || Store::open(path)?.counts()).await??;
                        let reply=Status{version:env!("CARGO_PKG_VERSION").into(),milestone:"metadata-foundation".into(),indexed_feeds:feeds,indexed_items:items,active_mounts:0};
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
