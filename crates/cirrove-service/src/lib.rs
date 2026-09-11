//! Linux user service and account coordination.
pub mod accounts;
mod activity;
pub use activity::DirectoryFreshness;
pub mod content;
pub mod engine;
pub mod events;
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
    /// Resident memory that is not live heap -- what a trim could return. This
    /// is what the reclamation trigger reads.
    #[serde(default)]
    pub retained_bytes: u64,
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
/// What this daemon can do beyond `status`. An older daemon answers with an
/// empty set rather than an error. See [`Capabilities`].
pub async fn capabilities(socket: &Path) -> Result<Capabilities> {
    request(socket, "capabilities", None::<()>, "Cirrove capabilities").await
}
/// A live subscription to daemon changes.
///
/// The client half of `subscribe`. Kept here rather than in the desktop crate
/// because a file-manager extension needs exactly the same thing, and two
/// readers of one wire format is how the two drift apart.
///
/// Dropping this unsubscribes; the daemon notices the closed stream and gives
/// the slot back.
#[derive(Debug)]
pub struct Subscription {
    lines: tokio::io::Lines<tokio::io::BufReader<UnixStream>>,
    /// The first event, already read. `open` has to consume one line to tell a
    /// stream from a refusal, and that line is a real event which the caller is
    /// owed -- it is part of the priming that makes a `status` call unnecessary.
    pending: Option<events::Event>,
}

impl Subscription {
    /// Open a subscription. The first events describe current state, so a caller
    /// that renders from them needs no `status` call to start.
    ///
    /// A daemon too old for the verb answers with a refusal instead of a stream.
    /// That is reported here rather than left to surface as a parse error on the
    /// first `next`, because "your service is too old" and "the wire is corrupt"
    /// are different problems with different remedies.
    pub async fn open(socket: &Path) -> Result<Self> {
        use tokio::io::AsyncBufReadExt;
        let mut stream = UnixStream::connect(socket)
            .await
            .context("Cirrove service is not reachable")?;
        tokio::time::timeout(EXCHANGE_TIMEOUT, stream.write_all(b"subscribe\n"))
            .await
            .context("could not ask for a subscription in time")??;
        let mut lines = tokio::io::BufReader::new(stream).lines();
        let first = tokio::time::timeout(EXCHANGE_TIMEOUT, lines.next_line())
            .await
            .context("the service did not answer the subscription in time")??
            .context("the service closed the subscription immediately")?;
        // One line, two possible meanings. An event parses as an event; anything
        // else is the refusal shape every other verb answers with.
        match serde_json::from_str::<events::Event>(&first) {
            Ok(event) => Ok(Self {
                lines,
                pending: Some(event),
            }),
            Err(_) => {
                let refusal: PinReply = serde_json::from_str(&first)
                    .context("the service sent neither an event nor a refusal")?;
                bail!(
                    "{}",
                    refusal
                        .refusal
                        .unwrap_or_else(|| "this Cirrove service cannot stream changes".into())
                )
            }
        }
    }

    /// The next change, or `None` when the daemon closed the stream.
    pub async fn next(&mut self) -> Result<Option<events::Event>> {
        if let Some(event) = self.pending.take() {
            return Ok(Some(event));
        }
        // Skip what this build cannot render rather than ending the stream on
        // it. A newer daemon may send events this client has never heard of, and
        // the right response to one is to carry on: see `Event::Unknown`.
        loop {
            let Some(line) = self.lines.next_line().await? else {
                return Ok(None);
            };
            let event: events::Event = serde_json::from_str(&line)?;
            if event != events::Event::Unknown {
                return Ok(Some(event));
            }
        }
    }
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
/// How long one exchange may take: reading a request line, or writing one
/// reply or one event. Deliberately not a budget for a whole connection, which
/// is what it used to be -- a subscription is meant to stay open and idle.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(3);
/// Request slots that a long-lived subscriber can never consume, so the CLI
/// keeps working no matter how many desktop clients are watching.
const REQUEST_SLOTS: usize = 16;
/// Concurrent subscriptions. A tray, a file-manager extension and a settings
/// window is three; the rest is room for a second session and a client that
/// leaked one and has not been restarted yet.
const SUBSCRIPTION_SLOTS: usize = 8;

/// What this daemon can do beyond `status`, asked for by name.
///
/// Exists because `STATUS_PROTOCOL_VERSION` cannot move: the desktop compares it
/// for equality, so bumping it to advertise an added verb would make every
/// mismatched pair refuse each other over a feature the older side never asked
/// for. Each capability carries its own version instead.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Absent on a daemon that predates this verb, whose reply to an unknown
    /// request is a refusal carrying no such key. Defaulting to empty turns that
    /// into the answer the client wants -- "this daemon can do nothing extra" --
    /// with no branch on the refusal sentence, which is prose and will change.
    #[serde(default)]
    pub capabilities: std::collections::BTreeMap<String, u32>,
}

impl Capabilities {
    /// Whether this daemon can stream events, and at which version.
    pub fn events(&self) -> Option<u32> {
        self.capabilities.get("events").copied()
    }

    pub fn current() -> Self {
        Self {
            capabilities: [("events".to_string(), events::EVENT_PROTOCOL_VERSION)]
                .into_iter()
                .collect(),
        }
    }
}

/// One JSON reply, then the caller closes. No trailing newline: `status` has
/// always been framed by end-of-stream and clients parse it that way.
async fn write_reply<T: Serialize>(stream: &mut UnixStream, reply: &T) -> Result<()> {
    let body = serde_json::to_vec(reply)?;
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.write_all(&body))
        .await
        .context("control reply could not be written in time")??;
    Ok(())
}

/// One event, newline-delimited, because the stream continues afterwards.
async fn write_event(stream: &mut UnixStream, event: &events::Event) -> Result<()> {
    let mut body = serde_json::to_vec(event)?;
    body.push(b'\n');
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.write_all(&body))
        .await
        .context("event could not be written in time")??;
    Ok(())
}

/// Hold the connection open and write changes as they happen.
///
/// Order matters at the start: subscribe first, then prime. The other way round
/// leaves a window in which a change lands between reading current state and
/// attaching to the stream, and that change is lost with nothing to indicate it.
/// Subscribing first can only repeat a level, which costs a client nothing.
async fn serve_subscription(
    mut stream: UnixStream,
    manager: Option<std::sync::Arc<manager::Manager>>,
    slots: std::sync::Arc<tokio::sync::Semaphore>,
) -> Result<()> {
    let Some(manager) = manager else {
        // Same shape as `handle_control`'s refusal, so a client parses one thing.
        return write_reply(
            &mut stream,
            &PinReply {
                refusal: Some("this daemon manages no accounts".into()),
                ..Default::default()
            },
        )
        .await;
    };
    let Ok(_permit) = slots.try_acquire_owned() else {
        // Refused with a reply rather than dropped. An over-budget connection is
        // dropped silently at accept today, which leaves a client unable to tell
        // a busy daemon from a broken socket; a subscriber that is told can back
        // off instead of reconnecting in a loop.
        return write_reply(
            &mut stream,
            &PinReply {
                refusal: Some("too many subscriptions; retry shortly".into()),
                ..Default::default()
            },
        )
        .await;
    };
    let mut changes = manager.subscribe();
    // The guard is released by the end of this statement, before any write. A
    // read lock held across a write to a client would let one stalled subscriber
    // block the manager's status update for the whole exchange timeout, which is
    // the same mistake as holding a lock across a network await.
    let primed = events::prime(manager.status.read().await.as_slice());
    for event in primed {
        write_event(&mut stream, &event).await?;
    }
    write_event(&mut stream, &events::Event::Ready).await?;
    loop {
        match changes.recv().await {
            Ok(event) => write_event(&mut stream, &event).await?,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                // Say so, then re-prime. Everything here is a level, so current
                // state is the cure for having missed some; the flag is for a
                // client that was animating and needs to know it saw a jump.
                write_event(&mut stream, &events::Event::Lagged { dropped }).await?;
                let primed = events::prime(manager.status.read().await.as_slice());
                for event in primed {
                    write_event(&mut stream, &event).await?;
                }
                write_event(&mut stream, &events::Event::Ready).await?;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
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
    // Subscriptions live for hours, so they cannot share the request budget:
    // three desktop clients holding one each would be three of sixteen slots
    // gone permanently, and a leaked one never comes back. The JoinSet is sized
    // for both kinds together while this semaphore bounds the long-lived half,
    // which leaves REQUEST_SLOTS always available to the CLI no matter how many
    // clients are watching.
    let subscriptions = std::sync::Arc::new(tokio::sync::Semaphore::new(SUBSCRIPTION_SLOTS));
    loop {
        tokio::select! { biased;
            _=cancel.cancelled()=>break,
            Some(_)=requests.join_next(),if !requests.is_empty()=>{},
            accepted=listener.accept()=>{
                let (mut stream,_)=accepted?;
                if requests.len()>=REQUEST_SLOTS+SUBSCRIPTION_SLOTS {drop(stream);continue;}
                let path=db_path.clone();let manager=manager.clone();
                let slots=subscriptions.clone();
                requests.spawn(async move {
                    // The timeout is per exchange, not per connection. It used to
                    // wrap this whole block, which is correct for a request and
                    // fatal for a subscription: the stream is meant to stay open
                    // and mostly idle, so a connection-lifetime deadline would
                    // kill every watcher three seconds in.
                    let result: Result<()> = async {
                        // Was a fixed seven-byte read compared against b"status\n",
                        // which is why there has only ever been one verb. Bounded
                        // line read instead; `status\n` stays byte-identical on the
                        // wire so an older client keeps working, and
                        // STATUS_PROTOCOL_VERSION does not move -- the desktop
                        // compares it for equality, so a bump would make every
                        // mismatched pair report incompatible over an added verb
                        // that changes no payload it reads.
                        let line=tokio::time::timeout(EXCHANGE_TIMEOUT,read_request_line(&mut stream)).await
                            .context("control request did not arrive in time")??;
                        let (verb,body)=match line.split_once(' ') {
                            Some((verb,body))=>(verb,body.trim()),
                            None=>(line.as_str(),""),
                        };
                        if verb=="subscribe" {
                            return serve_subscription(stream,manager,slots).await;
                        }
                        if verb=="capabilities" {
                            // Discovery without moving STATUS_PROTOCOL_VERSION. An
                            // older daemon answers this verb through
                            // `handle_control`'s unknown-request fallback, whose
                            // reply carries no `capabilities` key -- which is the
                            // rule a client should use, rather than matching on a
                            // refusal sentence that will be reworded.
                            return write_reply(&mut stream,&Capabilities::current()).await;
                        }
                        if verb!="status" {
                            let reply=handle_control(verb,body,&manager).await?;
                            return write_reply(&mut stream,&reply).await;
                        }
                        let (mut feeds,mut items)=tokio::task::spawn_blocking(move || Store::open(path)?.counts()).await??;
                        let accounts=match &manager {Some(m)=>m.status.read().await.clone(),None=>vec![]};
                        if manager.is_some() {feeds=accounts.iter().map(|a|a.indexed_feeds).sum();items=accounts.iter().map(|a|a.indexed_items).sum();}
                        let active_mounts=accounts.iter().filter(|a|a.mounted).count() as u64;
                        let reply=Status{protocol_version:STATUS_PROTOCOL_VERSION,version:env!("CARGO_PKG_VERSION").into(),milestone:"readonly-preview".into(),indexed_feeds:feeds,indexed_items:items,active_mounts,accounts,allocator_trims:crate::filesystem::allocator_trims(),free_arena_bytes:cirrove_allocator::free_arena_bytes(),retained_bytes:cirrove_allocator::retained_bytes()};
                        write_reply(&mut stream,&reply).await
                    }.await;
                    if result.is_err() {tracing::debug!("control request did not complete");}
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
    /// Start a daemon on a private socket and wait until it is listening.
    async fn serving(
        manager: Option<std::sync::Arc<manager::Manager>>,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        CancellationToken,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("run/control.sock");
        let db = dir.path().join("metadata.db");
        let cancel = CancellationToken::new();
        let task = tokio::spawn(serve_managed(db, socket.clone(), cancel.clone(), manager));
        tokio::time::timeout(Duration::from_secs(3), async {
            while !socket.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        (dir, socket, cancel, task)
    }

    async fn ask(socket: &Path, request: &str) -> String {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        reply
    }

    fn account(label: &str, mounted: bool) -> manager::AccountStatus {
        manager::AccountStatus {
            account_id: format!("id-{label}"),
            label: label.into(),
            state: "ready".into(),
            mounted,
            enabled: true,
            mount_path: PathBuf::from(format!("/mnt/{label}")),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn capabilities_names_the_event_stream_without_moving_the_status_version() {
        let (_dir, socket, cancel, task) = serving(None).await;
        let reply = ask(&socket, "capabilities\n").await;
        let parsed: Capabilities = serde_json::from_str(&reply).unwrap();
        assert_eq!(
            parsed.capabilities.get("events"),
            Some(&events::EVENT_PROTOCOL_VERSION)
        );
        // The whole point of asking by name: a client that learns about events
        // this way needs no version bump, and an existing client sees no change.
        assert_eq!(STATUS_PROTOCOL_VERSION, 1);
        let status_reply = ask(&socket, "status\n").await;
        assert!(
            !status_reply.contains("capabilities"),
            "status payload changed: {status_reply}"
        );
        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_unknown_verb_carries_no_capabilities_key() {
        // This is the rule a client uses against an older daemon, so it is worth
        // holding: discovery must not depend on the wording of the refusal.
        let (_dir, socket, cancel, task) = serving(None).await;
        let reply = ask(&socket, "nonsense\n").await;
        let parsed: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert!(parsed.get("capabilities").is_none(), "{parsed}");
        // And that reply read as a Capabilities is the answer "nothing extra",
        // which is what lets a client use one code path against both daemons.
        let as_capabilities: Capabilities = serde_json::from_str(&reply).unwrap();
        assert_eq!(as_capabilities.events(), None);
        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_subscriber_is_primed_with_current_state_before_anything_changes() {
        use tokio::io::AsyncBufReadExt;
        let manager = std::sync::Arc::new(manager::Manager::default());
        *manager.status.write().await = vec![account("work", true)];
        let (_dir, socket, cancel, task) = serving(Some(manager)).await;
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        stream.write_all(b"subscribe\n").await.unwrap();
        let mut lines = tokio::io::BufReader::new(stream).lines();
        let first: events::Event =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let second: events::Event =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(matches!(&first, events::Event::Account { label, .. } if label == "work"));
        assert!(matches!(
            &second,
            events::Event::Mount { mounted: true, .. }
        ));
        let third: events::Event =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(
            third,
            events::Event::Ready,
            "priming must end with a marker"
        );
        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_subscription_outlives_the_exchange_timeout() {
        // The regression test for splitting the timeout. One deadline used to
        // wrap the whole connection, so a subscription would be cut at three
        // seconds no matter how healthy it was. Waiting past EXCHANGE_TIMEOUT
        // with the stream still open is the whole assertion.
        use tokio::io::AsyncBufReadExt;
        let manager = std::sync::Arc::new(manager::Manager::default());
        *manager.status.write().await = vec![account("work", true)];
        let (_dir, socket, cancel, task) = serving(Some(manager.clone())).await;
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        stream.write_all(b"subscribe\n").await.unwrap();
        let mut lines = tokio::io::BufReader::new(stream).lines();
        lines.next_line().await.unwrap().unwrap();
        lines.next_line().await.unwrap().unwrap();
        lines.next_line().await.unwrap().unwrap();
        tokio::time::sleep(EXCHANGE_TIMEOUT + Duration::from_millis(750)).await;
        // Still attached: a change now must still arrive.
        let events = events::diff(&[], &[account("later", false)]);
        for event in events {
            manager.publish_for_test(event);
        }
        let line = tokio::time::timeout(Duration::from_secs(3), lines.next_line())
            .await
            .expect("the subscription was cut at the exchange timeout")
            .unwrap()
            .expect("the stream closed instead of delivering");
        let event: events::Event = serde_json::from_str(&line).unwrap();
        assert_eq!(event.account_id(), Some("id-later"));
        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn the_client_subscription_yields_priming_then_changes() {
        let manager = std::sync::Arc::new(manager::Manager::default());
        *manager.status.write().await = vec![account("work", true)];
        let (_dir, socket, cancel, task) = serving(Some(manager.clone())).await;
        let mut subscription = Subscription::open(&socket).await.unwrap();
        // The line `open` consumed to tell a stream from a refusal is still
        // delivered; losing it would silently drop one account from a tray.
        let first = subscription.next().await.unwrap().unwrap();
        assert!(matches!(&first, events::Event::Account { label, .. } if label == "work"));
        let second = subscription.next().await.unwrap().unwrap();
        assert!(matches!(second, events::Event::Mount { mounted: true, .. }));
        let ready = subscription.next().await.unwrap().unwrap();
        assert_eq!(
            ready,
            events::Event::Ready,
            "priming must end with a marker"
        );
        for event in events::diff(&[], &[account("later", false)]) {
            manager.publish_for_test(event);
        }
        let third = subscription.next().await.unwrap().unwrap();
        assert_eq!(third.account_id(), Some("id-later"));
        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    /// A newer daemon may send events this client has never heard of, and that
    /// must not end the subscription.
    ///
    /// Without `Event::Unknown` the first new variant breaks every older
    /// subscriber: serde refuses the tag, `next` returns the parse error, and a
    /// tray reports the service unreachable because the daemon said something
    /// newer than it. That would make `transfer`, `pin` and every event after
    /// them a protocol break instead of an addition. Measured before the arm
    /// existed: `unknown variant \`transfer\``.
    ///
    /// Asserted on the client rather than through the daemon, because the daemon
    /// cannot yet send an event it does not have -- which is exactly the version
    /// skew being modelled.
    #[tokio::test]
    async fn an_event_from_a_newer_daemon_is_skipped_rather_than_ending_the_stream() {
        use tokio::io::AsyncWriteExt;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Consume the verb, then answer as a daemon two versions ahead.
            let mut verb = [0u8; 16];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut verb).await;
            for line in [
                r#"{"event":"ready"}"#,
                r#"{"event":"transfer","account_id":"id-work","progress":0.5}"#,
                r#"{"event":"something_else_entirely"}"#,
                r#"{"event":"account_removed","account_id":"id-work","label":"work"}"#,
            ] {
                stream.write_all(line.as_bytes()).await.unwrap();
                stream.write_all(b"\n").await.unwrap();
            }
            stream.flush().await.unwrap();
            // Hold the connection so the client sees a stream, not a close.
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let mut subscription = Subscription::open(&socket).await.unwrap();
        assert_eq!(
            subscription.next().await.unwrap().unwrap(),
            events::Event::Ready
        );
        // The two it cannot read are stepped over, and the one after them
        // arrives intact. Without the skip this is a parse error instead.
        let next = subscription
            .next()
            .await
            .expect("two unreadable events ended the stream")
            .expect("the stream closed instead of delivering");
        assert!(
            matches!(&next, events::Event::AccountRemoved { account_id, .. } if account_id == "id-work"),
            "expected the event after the unreadable ones, got {next:?}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_daemon_with_no_accounts_still_establishes_a_subscription() {
        // Found by running the tray, not by a test. With nothing to prime with
        // the daemon sent no events at all, so the client waited out its
        // deadline and reported "the service did not answer in time" against a
        // service that was answering perfectly. Ready is what distinguishes an
        // established subscription from silence.
        let manager = std::sync::Arc::new(manager::Manager::default());
        let (_dir, socket, cancel, task) = serving(Some(manager)).await;
        let mut subscription = Subscription::open(&socket).await.unwrap();
        assert_eq!(
            subscription.next().await.unwrap().unwrap(),
            events::Event::Ready
        );
        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_client_is_told_when_the_service_cannot_stream() {
        // A daemon managing no accounts answers the refusal shape. The client
        // must report that as "too old / cannot stream", not as a parse error on
        // some later line, because the two have different remedies.
        let (_dir, socket, cancel, task) = serving(None).await;
        let error = Subscription::open(&socket).await.unwrap_err().to_string();
        assert!(error.contains("manages no accounts"), "{error}");
        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn subscriptions_are_refused_with_a_reply_and_cannot_starve_status() {
        use tokio::io::AsyncBufReadExt;
        let manager = std::sync::Arc::new(manager::Manager::default());
        *manager.status.write().await = vec![account("work", true)];
        let (_dir, socket, cancel, task) = serving(Some(manager)).await;
        let mut held = Vec::new();
        for _ in 0..SUBSCRIPTION_SLOTS {
            let mut stream = UnixStream::connect(&socket).await.unwrap();
            stream.write_all(b"subscribe\n").await.unwrap();
            let mut lines = tokio::io::BufReader::new(stream).lines();
            // Drain priming through the Ready marker, so the subscription is
            // established before the next one is opened.
            lines.next_line().await.unwrap().unwrap();
            lines.next_line().await.unwrap().unwrap();
            lines.next_line().await.unwrap().unwrap();
            held.push(lines);
        }
        // One too many is told so, rather than dropped without a reply.
        let refused = ask(&socket, "subscribe\n").await;
        let parsed: serde_json::Value = serde_json::from_str(&refused).unwrap();
        assert!(
            parsed["refusal"]
                .as_str()
                .unwrap_or_default()
                .contains("too many"),
            "{parsed}"
        );
        // And the request half is untouched, which is the reason for two budgets.
        let reply = status(&socket).await.unwrap();
        assert_eq!(reply.protocol_version, STATUS_PROTOCOL_VERSION);
        cancel.cancel();
        task.await.unwrap().unwrap();
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
