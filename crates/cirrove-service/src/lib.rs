//! Linux user service and account coordination.
pub mod accounts;
mod activity;
pub use activity::DirectoryFreshness;
pub mod content;
pub mod diagnostics;
pub mod engine;
pub mod events;
pub mod filesystem;
pub mod icloud_writes;
pub mod jobs;
pub mod journal;
pub mod manager;
pub mod mutations;
pub mod native_abandon;
pub mod native_import;
pub mod native_trash;
pub mod recent;
mod recovery;
pub mod transfers;
pub mod validation;
pub mod writable;
use anyhow::{Context, Result, bail};
use cirrove_core::{CancellationToken, FeedMode, MetadataProvider, ProviderError, Scope};
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

/// The formats this executable writes. Reporting does not open local state,
/// establish package authenticity, or authorize migration/downgrade.
pub fn storage_format_attestation() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "product": "cirroved",
        "journal_schema": journal::JOURNAL_SCHEMA,
        "metadata_schema": cirrove_store::SCHEMA_VERSION,
    })
}

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
    /// True when this daemon's own executable has been replaced on disk since
    /// it started -- a package upgrade happened and this process is still the
    /// old version.
    ///
    /// Reported because the upgrade says nothing and neither did we. Measured
    /// on Fedora 44 on 2026-09-14: after `dnf upgrade` the daemon's pid was
    /// unchanged and `/proc/<pid>/exe` read `/usr/bin/cirroved (deleted)`, so
    /// the person went on running the version they had just replaced, with no
    /// indication anywhere. The visible symptom was mild and misleading -- the
    /// upgrade's new tray icon simply never appeared.
    ///
    /// `#[serde(default)]` for the usual reason: an older daemon's reply reads
    /// back as false, which is the only answer it can give.
    #[serde(default)]
    pub restart_required: bool,
}

/// Whether the running program's own executable has been replaced on disk.
///
/// A package manager unlinks the old file and writes a new one. Linux keeps the
/// running process on the old inode and marks the fact by appending
/// `" (deleted)"` to `/proc/self/exe`, which is the only signal there is.
pub fn binary_replaced_on_disk() -> bool {
    std::fs::read_link("/proc/self/exe")
        .map(|target| exe_link_says_replaced(&target.to_string_lossy()))
        .unwrap_or(false)
}

/// The decision on its own, so it can be tested without replacing a binary.
///
/// A real path could in principle end in those characters, which would read as
/// a pending restart forever. The consequence is a banner suggesting a restart
/// that is not needed, which is the harmless direction to be wrong in; the
/// alternative -- comparing inodes with the path -- is wrong in the other
/// direction whenever the daemon runs from a build directory.
pub fn exe_link_says_replaced(target: &str) -> bool {
    target.ends_with(" (deleted)")
}

/// A pin request carried over the control socket.
///
/// The command line used to write `Store::pin` directly, which is why
/// `Engine::materialise_pin` and `Engine::pin_folder` had no caller outside their
/// tests: a process that is not the daemon has no engine to reach. Routing the
/// request to the daemon puts the budget rule, the scope resolution and the
/// fetching in the one place that owns them.
/// A byte count in the largest unit that keeps it readable.
///
/// Fixed units do not survive the range this project actually reports: a cache
/// budget is gigabytes and a pinned spreadsheet is tens of kilobytes, and
/// printing both in MiB showed a real 66 KB pin as "0/0 MiB kept". One decimal
/// below 10 of a unit, none above, because "4.6 GB" and "907 MB" are both what
/// a person would say out loud.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, f64); 4] = [
        ("GB", 1024.0 * 1024.0 * 1024.0),
        ("MB", 1024.0 * 1024.0),
        ("KB", 1024.0),
        ("bytes", 1.0),
    ];
    for (unit, scale) in UNITS {
        if bytes as f64 >= scale {
            let value = bytes as f64 / scale;
            return if *unit == *"bytes" {
                format!("{bytes} bytes")
            } else if value < 10.0 {
                format!("{value:.1} {unit}")
            } else {
                format!("{value:.0} {unit}")
            };
        }
    }
    "0 bytes".to_owned()
}

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
    /// The job now fetching what the pin covers, when one was started.
    ///
    /// An accepted pin is a reservation, which is instant, and a fetch, which is
    /// not: the reply says the first happened and names the second so a caller
    /// can watch it, stop it, or wait for it. `None` from a daemon that fetched
    /// inside the request, and from an unpin, which has nothing to wait for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
}

/// Ask the daemon to stop a running job, or to forget one that already ended.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StopJobRequest {
    #[serde(default)]
    pub label: String,
    pub id: String,
}
/// What the daemon did about it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StopJobReply {
    /// True when a running job was asked to stop, or a finished record cleared.
    /// False means there was no such job, which is not an error: a client acting
    /// on a list it read a second ago races a job that ended in between.
    #[serde(default)]
    pub stopped: bool,
    /// The work was already over; the record was cleared instead.
    #[serde(default)]
    pub already_ended: bool,
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
    recent: Option<&recent::RecentChanges>,
) -> Result<u64> {
    let path = db_path.to_owned();
    let s = scope.clone();
    let mode = provider.feed_mode();
    let mut cursor = tokio::task::spawn_blocking(move || {
        let mut store = Store::open(path)?;
        if reset {
            store.begin(&s, true)
        } else if mode == FeedMode::FullSnapshot {
            store.begin_snapshot(&s)
        } else {
            store.begin(&s, false)
        }
    })
    .await??;
    // Whether this refresh continues from a saved cursor, decided once: a
    // baseline can run to several pages, and every page after the first
    // carries a cursor too. The first version looked at the page and recorded
    // a whole drive's second page as activity.
    let continuation = mode == FeedMode::Incremental && cursor.is_some() && !reset;
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
        // Activity, not baseline: a page continuing from a saved cursor is
        // what changed since; the first delta and a re-baseline list the
        // whole drive and would swamp a list meant to answer "what happened
        // while I was looking away".
        let record = recent.is_some() && continuation;
        let recorded = tokio::task::spawn_blocking(move || {
            let mut store = Store::open(path)?;
            let mut out = Vec::new();
            if record {
                for change in &page.changes {
                    out.push(match change {
                        cirrove_core::Change::Upsert(node) => recent::RemoteChange {
                            at_unix: recent::now_unix(),
                            id: node.id.clone(),
                            parent_id: node.parent_id.clone(),
                            name: node.name.clone(),
                            kind: if node.kind == cirrove_core::NodeKind::Folder {
                                "folder"
                            } else {
                                "file"
                            }
                            .into(),
                            size: node.size,
                            removed: false,
                        },
                        cirrove_core::Change::Delete { id } => {
                            // The store still holds the item until this page
                            // publishes, which is the last chance at its name.
                            let known = store.node(&s, id).ok().flatten();
                            recent::RemoteChange {
                                at_unix: recent::now_unix(),
                                id: id.clone(),
                                parent_id: known.as_ref().and_then(|n| n.parent_id.clone()),
                                name: known
                                    .as_ref()
                                    .map_or_else(|| id.clone(), |n| n.name.clone()),
                                kind: if matches!(
                                    known.as_ref().map(|n| &n.kind),
                                    Some(cirrove_core::NodeKind::Folder)
                                ) {
                                    "folder"
                                } else {
                                    "file"
                                }
                                .into(),
                                size: known.as_ref().map_or(0, |n| n.size),
                                removed: true,
                            }
                        }
                    });
                }
            }
            store.stage(&s, expected.as_ref(), &page)?;
            Ok::<_, anyhow::Error>(out)
        })
        .await??;
        if let Some(recent) = recent {
            for change in recorded {
                recent.record(change);
            }
        }
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

/// Ask the daemon to abandon the changes it gave up on for one account.
///
/// Discards, never retries. See `Manager::discard_stuck` for why.
/// Ask the daemon to try the stuck changes again, where that is sensible.
///
/// See [`RetryReply`] for why "where that is sensible" is the whole of it.
pub async fn retry_stuck(socket: &Path, label: &str) -> Result<RetryReply> {
    request(
        socket,
        "retry-stuck",
        Some(DiscardRequest {
            label: label.to_owned(),
        }),
        "Cirrove retry",
    )
    .await
}
pub async fn discard_stuck(socket: &Path, label: &str) -> Result<DiscardReply> {
    request(
        socket,
        "discard-stuck",
        Some(DiscardRequest {
            label: label.to_owned(),
        }),
        "Cirrove discard",
    )
    .await
}

/// What changed lately on one account: remote changes from the delta feed,
/// local saves from the upload journal, latest first, at most `limit` each.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecentRequest {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub limit: usize,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecentReply {
    #[serde(default)]
    pub remote: Vec<recent::RemoteChange>,
    #[serde(default)]
    pub local: Vec<recent::LocalChange>,
    /// The changes the daemon has given up on, named rather than counted.
    /// `#[serde(default)]` so an older daemon reads back as none, which is the
    /// list that daemon can produce.
    #[serde(default)]
    pub stuck: Vec<recent::StuckChange>,
    /// The saves the daemon has given up on, named rather than counted.
    /// `failed_uploads` counted these from the day it was written and nothing
    /// ever named them, so a person met a warning triangle and a number with no
    /// way to learn which file it meant. Kept apart from `stuck` because the
    /// actions differ: discarding a refused folder removal loses nothing, and
    /// discarding a failed save loses what the person wrote.
    #[serde(default)]
    pub failed: Vec<recent::StuckChange>,
    #[serde(default)]
    pub refusal: Option<String>,
}
pub async fn recent(socket: &Path, request: &RecentRequest) -> Result<RecentReply> {
    self::request(socket, "recent", Some(request), "Cirrove recent").await
}

/// What a file manager asks: the state of several paths in one exchange. It
/// asks about every file it shows, and a round trip per file would be the
/// slowest thing on the screen.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PathsRequest {
    /// Account label. Empty means the only account.
    #[serde(default)]
    pub label: String,
    /// Mount-relative paths, at most `PATHS_PER_REQUEST`.
    #[serde(default)]
    pub paths: Vec<String>,
}
/// A directory listing's worth. More is a client that should batch.
pub const PATHS_PER_REQUEST: usize = 200;
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathState {
    pub path: String,
    /// Whether indexed metadata supports a pin; false when unknown.
    #[serde(default)]
    pub can_pin: bool,
    #[serde(default)]
    pub item: String,
    /// "file" or "folder".
    #[serde(default)]
    pub kind: String,
    /// "direct" when the item itself is pinned, "inherited" when a folder above
    /// it is pinned recursively, absent when no pin covers it.
    #[serde(default)]
    pub pinned: Option<String>,
    #[serde(default)]
    pub size: u64,
    /// Bytes of this file's content on disk right now, so a badge can tell
    /// "kept" from "reserved and not fetched yet". Zero for a folder.
    #[serde(default)]
    pub resident: u64,
    /// Why there is no state: the path does not exist, or is not indexed and
    /// the provider could not be asked.
    #[serde(default)]
    pub refusal: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PathsReply {
    #[serde(default)]
    pub states: Vec<PathState>,
    #[serde(default)]
    pub refusal: Option<String>,
}
/// The state of several mount-relative paths of one account.
pub async fn paths(socket: &Path, request: &PathsRequest) -> Result<PathsReply> {
    self::request(socket, "paths", Some(request), "Cirrove paths").await
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiscardRequest {
    #[serde(default)]
    pub label: String,
}

/// What the daemon did about the stuck changes, and what is left.
///
/// `remaining` is read back after the discard rather than computed from the
/// count, because a discard can refuse one it listed -- an object that moved on
/// in between -- and a caller told "cleared" while the number stayed put would
/// have to find that out by asking again.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiscardReply {
    #[serde(default)]
    pub discarded: u64,
    #[serde(default)]
    pub remaining: u64,
    #[serde(default)]
    pub refusal: Option<String>,
}

/// What the daemon will try again, and what it will not.
///
/// The two numbers are different kinds of thing and the difference is the point.
/// A change that **failed** -- a quota, a permission, a connection that went
/// away -- is one the provider never decided about, and trying it again is
/// ordinary. A change in **conflict** is one the provider decided about: the
/// remote moved, and re-sending it would act on whatever is there now, which is
/// how a rename nobody made or a deletion of a version nobody saw happens. Those
/// are counted, not queued, and the person decides.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RetryReply {
    /// Changes queued to be tried again.
    #[serde(default)]
    pub queued: u64,
    /// Changes the cloud already decided about, which are not re-sent.
    #[serde(default)]
    pub conflicts: u64,
    #[serde(default)]
    pub refusal: Option<String>,
}

/// What happened to one path asked to be deleted permanently.
///
/// Every path gets an answer, including the refused ones. A person destroying
/// things without recovery is owed a line per thing, not a count.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PermanentDeletion {
    pub path: String,
    #[serde(default)]
    pub removed: bool,
    #[serde(default)]
    pub refusal: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PermanentDeleteRequest {
    #[serde(default)]
    pub label: String,
    pub paths: Vec<String>,
    /// Sent by a caller that has told the person this cannot be undone and has
    /// had them say yes. The daemon refuses without it, so a client cannot make
    /// this happen quietly by forgetting to ask.
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PermanentDeleteReply {
    #[serde(default)]
    pub deletions: Vec<PermanentDeletion>,
    #[serde(default)]
    pub refusal: Option<String>,
}

/// Remove these paths without the provider's recycle bin. See ADR 0008.
pub async fn delete_permanently(
    socket: &Path,
    label: &str,
    paths: Vec<String>,
) -> Result<PermanentDeleteReply> {
    request(
        socket,
        "delete-permanently",
        Some(PermanentDeleteRequest {
            label: label.to_owned(),
            paths,
            confirmed: true,
        }),
        "Cirrove delete-permanently",
    )
    .await
}

/// What keeping both copies did.
///
/// `considered` is every save the cloud refused; `kept` is how many now have a
/// copy queued beside the remote version. They differ when a save names an item
/// the index no longer knows -- the file it was replacing has since been moved
/// or removed -- and the difference is reported rather than hidden, because the
/// person is about to be told their work is safe.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeepBothReply {
    #[serde(default)]
    pub kept: u64,
    #[serde(default)]
    pub considered: u64,
    #[serde(default)]
    pub refusal: Option<String>,
}

/// Explicit replacement of one exact selected native PACKAGE revision.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "ReplaceNativePackageWire")]
pub struct ReplaceNativePackageRequest {
    pub label: String,
    pub expected_account_id: String,
    pub path: String,
    pub item_id: String,
    pub etag: String,
    pub archive: PathBuf,
    #[serde(default, skip_serializing_if = "package_source_is_wrapped")]
    pub source_layout: native_import::PackageSourceLayout,
    pub expected_root: Option<String>,
}
fn package_source_is_wrapped(layout: &native_import::PackageSourceLayout) -> bool {
    *layout == native_import::PackageSourceLayout::Wrapped
}
fn validate_package_source_shape(
    layout: native_import::PackageSourceLayout,
    root: &Option<String>,
) -> std::result::Result<(), &'static str> {
    match (layout, root) {
        (native_import::PackageSourceLayout::Wrapped, Some(_))
        | (
            native_import::PackageSourceLayout::FlatNumbers
            | native_import::PackageSourceLayout::FlatPages,
            None,
        ) => Ok(()),
        _ => Err("source layout and archive root disagree"),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceNativePackageWire {
    label: String,
    expected_account_id: String,
    path: String,
    item_id: String,
    etag: String,
    archive: PathBuf,
    #[serde(default)]
    source_layout: native_import::PackageSourceLayout,
    expected_root: Option<String>,
}
impl TryFrom<ReplaceNativePackageWire> for ReplaceNativePackageRequest {
    type Error = &'static str;
    fn try_from(wire: ReplaceNativePackageWire) -> std::result::Result<Self, Self::Error> {
        validate_package_source_shape(wire.source_layout, &wire.expected_root)?;
        Ok(Self {
            label: wire.label,
            expected_account_id: wire.expected_account_id,
            path: wire.path,
            item_id: wire.item_id,
            etag: wire.etag,
            archive: wire.archive,
            source_layout: wire.source_layout,
            expected_root: wire.expected_root,
        })
    }
}
/// Observation only; never submits a replacement or changes its archive.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchNativeReplacementRequest {
    pub label: String,
    pub expected_account_id: String,
    pub operation: uuid::Uuid,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeReplacementReply {
    #[serde(default)]
    pub job: Option<jobs::Job>,
    #[serde(default)]
    pub refusal: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListNativeReplacementsRequest {
    pub label: String,
    pub expected_account_id: String,
    #[serde(default)]
    pub after: Option<u64>,
    pub limit: u32,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ListNativeReplacementsReply {
    #[serde(default)]
    pub operations: Vec<journal::NativeReplacementSelection>,
    #[serde(default)]
    pub next: Option<u64>,
    #[serde(default)]
    pub refusal: Option<String>,
}
pub async fn replace_native_package(
    socket: &Path,
    body: &ReplaceNativePackageRequest,
) -> Result<NativeReplacementReply> {
    request(
        socket,
        "replace-native-package",
        Some(body),
        "Cirrove explicit native replacement",
    )
    .await
}
pub async fn watch_native_replacement(
    socket: &Path,
    body: &WatchNativeReplacementRequest,
) -> Result<NativeReplacementReply> {
    request(
        socket,
        "watch-native-replacement",
        Some(body),
        "Cirrove native replacement observation",
    )
    .await
}
pub async fn list_native_replacements(
    socket: &Path,
    body: &ListNativeReplacementsRequest,
) -> Result<ListNativeReplacementsReply> {
    request(
        socket,
        "list-native-replacements",
        Some(body),
        "Cirrove retained native replacements",
    )
    .await
}

/// Discover retained native Trash operations after a lost reply or daemon restart.
/// This bounded read never enqueues, wakes a worker or retries a mutation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListNativeTrashRequest {
    pub label: String,
    pub expected_account_id: String,
    #[serde(default)]
    pub after: Option<u64>,
    /// One bounded page; valid range is 1..=100.
    pub limit: u32,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ListNativeTrashReply {
    #[serde(default)]
    pub operations: Vec<native_trash::NativeTrashSelection>,
    #[serde(default)]
    pub next: Option<u64>,
    #[serde(default)]
    pub refusal: Option<String>,
}
pub async fn list_native_trash(
    socket: &Path,
    body: &ListNativeTrashRequest,
) -> Result<ListNativeTrashReply> {
    request(
        socket,
        "list-native-trash",
        Some(body),
        "Cirrove retained native Trash operations",
    )
    .await
}

/// Explicit, exact-revision removal into provider recovery; never permanent delete.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrashNativeDocumentRequest {
    pub label: String,
    pub expected_account_id: String,
    pub path: String,
    pub item_id: String,
    pub etag: String,
}
/// Observer only: cannot enqueue or replay a removal. Completion means recorded
/// historical removal/publication evidence, not current state after an external restore.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchNativeTrashRequest {
    pub label: String,
    pub expected_account_id: String,
    pub operation: uuid::Uuid,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeTrashReply {
    #[serde(default)]
    pub job: Option<jobs::Job>,
    #[serde(default)]
    pub refusal: Option<String>,
}
pub async fn trash_native_document(
    socket: &Path,
    body: &TrashNativeDocumentRequest,
) -> Result<NativeTrashReply> {
    request(
        socket,
        "trash-native-document",
        Some(body),
        "Cirrove native document Trash",
    )
    .await
}
pub async fn watch_native_trash(
    socket: &Path,
    body: &WatchNativeTrashRequest,
) -> Result<NativeTrashReply> {
    request(
        socket,
        "watch-native-trash",
        Some(body),
        "Cirrove native Trash observation",
    )
    .await
}

/// Discover saved explicit imports without capture, upload, retry or provider IO.
/// Completion evidence is historical; current availability requires explicit watch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListNativeImportsRequest {
    pub label: String,
    pub expected_account_id: String,
    #[serde(default)]
    pub after: Option<u64>,
    pub limit: u32,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ListNativeImportsReply {
    #[serde(default)]
    pub operations: Vec<journal::NativeImportSelection>,
    #[serde(default)]
    pub next: Option<u64>,
    #[serde(default)]
    pub refusal: Option<String>,
}
pub async fn list_native_imports(
    socket: &Path,
    body: &ListNativeImportsRequest,
) -> Result<ListNativeImportsReply> {
    request(
        socket,
        "list-native-imports",
        Some(body),
        "Cirrove saved native imports",
    )
    .await
}

/// Attach an observer to an existing durable native import, never enqueue again.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchNativeImportRequest {
    pub label: String,
    pub expected_account_id: String,
    pub operation: uuid::Uuid,
}
pub async fn watch_native_import(
    socket: &Path,
    body: &WatchNativeImportRequest,
) -> Result<ImportNativePackageReply> {
    request(
        socket,
        "watch-native-import",
        Some(body),
        "Cirrove native import observation",
    )
    .await
}

/// Explicit native archive import; representation is verified by the daemon.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "ImportNativePackageWire")]
pub struct ImportNativePackageRequest {
    pub label: String,
    /// Optional consent binding for callers that selected an existing account.
    /// None preserves deliberate CLI label lookup; Some must match before a job starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<String>,
    pub archive: PathBuf,
    #[serde(default, skip_serializing_if = "package_source_is_wrapped")]
    pub source_layout: native_import::PackageSourceLayout,
    pub expected_root: Option<String>,
    /// Visible relative destination directory inside the selected mount.
    pub parent: String,
    pub name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportNativePackageWire {
    label: String,
    #[serde(default)]
    expected_account_id: Option<String>,
    archive: PathBuf,
    #[serde(default)]
    source_layout: native_import::PackageSourceLayout,
    expected_root: Option<String>,
    parent: String,
    name: String,
}
impl TryFrom<ImportNativePackageWire> for ImportNativePackageRequest {
    type Error = &'static str;
    fn try_from(wire: ImportNativePackageWire) -> std::result::Result<Self, Self::Error> {
        validate_package_source_shape(wire.source_layout, &wire.expected_root)?;
        Ok(Self {
            label: wire.label,
            expected_account_id: wire.expected_account_id,
            archive: wire.archive,
            source_layout: wire.source_layout,
            expected_root: wire.expected_root,
            parent: wire.parent,
            name: wire.name,
        })
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ImportNativePackageReply {
    pub job: Option<jobs::Job>,
    pub refusal: Option<String>,
}
pub async fn import_native_package(
    socket: &Path,
    body: &ImportNativePackageRequest,
) -> Result<ImportNativePackageReply> {
    request(
        socket,
        "import-native-package",
        Some(body),
        "Cirrove native import",
    )
    .await
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportSaveRequest {
    pub label: String,
    pub operation: uuid::Uuid,
    pub destination: PathBuf,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExportSaveReply {
    #[serde(default)]
    pub job: Option<jobs::Job>,
    #[serde(default)]
    pub refusal: Option<String>,
}
pub async fn export_save(
    socket: &Path,
    request_body: &ExportSaveRequest,
) -> Result<ExportSaveReply> {
    request(
        socket,
        "export-save",
        Some(request_body),
        "Cirrove local export",
    )
    .await
}

/// Metadata-only active working-file recovery, never a cloud refresh.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryWorkingRequest {
    pub label: String,
    pub after: Option<uuid::Uuid>,
    pub limit: u32,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecoveryWorkingReply {
    pub files: Vec<journal::WorkingRecovery>,
    pub next: Option<uuid::Uuid>,
    pub refusal: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportWorkingRequest {
    pub label: String,
    pub file: uuid::Uuid,
    pub generation: u64,
    pub destination: PathBuf,
}
impl ExportWorkingRequest {
    /// Match an exact service receipt, never merely a terminal job status.
    pub fn confirmed_receipt<'a>(
        &self,
        initial: &jobs::Job,
        completed: &'a jobs::Job,
    ) -> Option<&'a journal::WorkingExportReceipt> {
        let receipt = completed.working_export.as_ref()?;
        (initial.kind == jobs::JobKind::ExportLocal
            && completed.kind == jobs::JobKind::ExportLocal
            && completed.id == initial.id
            && completed.state == jobs::JobState::Succeeded
            && completed.export.is_none()
            && receipt.source.file == self.file
            && receipt.source.generation == self.generation
            && receipt.source.size == initial.bytes_total
            && receipt.destination == self.destination
            && receipt.sha256.len() == 64
            && receipt.sha256.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(receipt)
    }
}
pub async fn recovery_working(
    socket: &Path,
    body: &RecoveryWorkingRequest,
) -> Result<RecoveryWorkingReply> {
    request(
        socket,
        "recovery-working",
        Some(body),
        "Cirrove working recovery",
    )
    .await
}
pub async fn export_working(socket: &Path, body: &ExportWorkingRequest) -> Result<ExportSaveReply> {
    request(
        socket,
        "export-working",
        Some(body),
        "Cirrove working export",
    )
    .await
}

/// Put the person's version of every refused save beside the cloud's, under a
/// new name, instead of making them choose which one to lose.
pub async fn keep_both(socket: &Path, label: &str) -> Result<KeepBothReply> {
    request(
        socket,
        "keep-both",
        Some(DiscardRequest {
            label: label.to_owned(),
        }),
        "Cirrove keep-both",
    )
    .await
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

/// Stop a running job, or clear the record of one that ended. See [`jobs`].
pub async fn stop_job(socket: &Path, body: &StopJobRequest) -> Result<StopJobReply> {
    request(socket, "stop-job", Some(body), "Cirrove stop").await
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
    let reply = match verb {
        "pin" => manager.apply_pin_request(&request).await,
        _ => manager.apply_unpin_request(&request).await,
    };
    match reply {
        Ok(reply) => Ok(reply),
        Err(error) => Ok(PinReply {
            refusal: Some(error.to_string()),
            ..Default::default()
        }),
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
            capabilities: [
                ("events".to_string(), events::EVENT_PROTOCOL_VERSION),
                // So a client can ask rather than guess. An older daemon omits
                // the key and its refusal of the verb is the same answer.
                ("discard-stuck".to_string(), 1),
                ("paths".to_string(), 1),
                ("paths-cached".to_string(), 1),
                ("recent".to_string(), 1),
                ("retry-stuck".to_string(), 1),
                ("keep-both".to_string(), 1),
                ("export-save".to_string(), 1),
                ("import-native-package".to_string(), 1),
                ("watch-native-import".to_string(), 1),
                ("list-native-imports".to_string(), 1),
                ("trash-native-document".to_string(), 1),
                ("watch-native-trash".to_string(), 1),
                ("list-native-trash".to_string(), 1),
                ("replace-native-package".to_string(), 1),
                ("watch-native-replacement".to_string(), 1),
                ("list-native-replacements".to_string(), 1),
                ("abandon-native-stage".to_string(), 1),
                ("native-stage-abandonment".to_string(), 1),
                ("import-native-package-account-binding".to_string(), 1),
                ("delete-permanently".to_string(), 1),
                ("stop-job".to_string(), 1),
            ]
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
                        if verb=="discard-stuck" {
                            let reply=match (serde_json::from_str::<DiscardRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.discard_stuck(&r.label).await {
                                    Ok((discarded,remaining))=>DiscardReply{discarded,remaining,refusal:None},
                                    Err(error)=>DiscardReply{refusal:Some(error.to_string()),..Default::default()},
                                },
                                (Ok(_),None)=>DiscardReply{refusal:Some("this service manages no accounts".into()),..Default::default()},
                                (Err(_),_)=>DiscardReply{refusal:Some("malformed request body".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="retry-stuck" {
                            let reply=match (serde_json::from_str::<DiscardRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.retry_stuck(&r.label).await {
                                    Ok((queued,conflicts))=>RetryReply{queued,conflicts,refusal:None},
                                    Err(error)=>RetryReply{refusal:Some(error.to_string()),..Default::default()},
                                },
                                (Ok(_),None)=>RetryReply{refusal:Some("this service manages no accounts".into()),..Default::default()},
                                (Err(_),_)=>RetryReply{refusal:Some("malformed request body".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="delete-permanently" {
                            let reply=match (serde_json::from_str::<PermanentDeleteRequest>(body),&manager) {
                                // Before anything else, including whether this
                                // daemon has the account: consent is a condition
                                // of the request, not a property of a drive, and
                                // a client author who forgot it should be told
                                // that rather than something about accounts.
                                (Ok(r),_) if !r.confirmed=>PermanentDeleteReply{refusal:Some("permanent deletion needs the person to have been asked; nothing was removed".into()),..Default::default()},
                                (Ok(r),_) if r.paths.len()>PATHS_PER_REQUEST=>PermanentDeleteReply{refusal:Some(format!("at most {PATHS_PER_REQUEST} paths per request")),..Default::default()},
                                (Ok(r),Some(m))=>match m.delete_permanently(&r.label,&r.paths,r.confirmed).await {
                                    Ok(deletions)=>PermanentDeleteReply{deletions,refusal:None},
                                    Err(error)=>PermanentDeleteReply{refusal:Some(error.to_string()),..Default::default()},
                                },
                                (Ok(_),None)=>PermanentDeleteReply{refusal:Some("this service manages no accounts".into()),..Default::default()},
                                (Err(_),_)=>PermanentDeleteReply{refusal:Some("malformed request body".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="recovery-working" {
                            let reply=match (serde_json::from_str::<RecoveryWorkingRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.recovery_working(&r).await {
                                    Ok((files,next))=>RecoveryWorkingReply{files,next,refusal:None},
                                    Err(error)=>RecoveryWorkingReply{refusal:Some(error.to_string()),..Default::default()},
                                },
                                _=>RecoveryWorkingReply{refusal:Some("working recovery request or account service is unavailable".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="export-working" {
                            let reply=match (serde_json::from_str::<ExportWorkingRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.export_working(&r).await {
                                    Ok(job)=>ExportSaveReply{job:Some(job),refusal:None},
                                    Err(error)=>ExportSaveReply{refusal:Some(error.to_string()),..Default::default()},
                                },
                                _=>ExportSaveReply{refusal:Some("working export request or account service is unavailable".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if matches!(verb, "abandon-native-stage" | "native-stage-abandonment") {
                            return write_reply(&mut stream, &native_abandon::handle(verb, body, manager.as_ref()).await).await;
                        }
                        if verb=="replace-native-package" {
                            let reply=match (serde_json::from_str::<ReplaceNativePackageRequest>(body),&manager){
                                (Ok(r),Some(m)) if !r.label.is_empty()=>match m.engine(&r.label).await {
                                    Ok(engine) if engine.account.id==r.expected_account_id && uuid::Uuid::parse_str(&r.expected_account_id).is_ok()=>match m.start_native_replacement(engine,crate::native_import::NativeReplaceInput{selected:crate::native_trash::NativeTrashInput{expected_account_id:r.expected_account_id,path:r.path,item_id:r.item_id,etag:r.etag},source:r.archive,source_layout:r.source_layout,expected_root:r.expected_root}).await {
                                        Ok(job)=>NativeReplacementReply{job:Some(job),refusal:None},Err(_)=>NativeReplacementReply{job:None,refusal:Some("native replacement admission could not be started".into())},
                                    },_=>NativeReplacementReply{job:None,refusal:Some("selected replacement account changed or is unavailable".into())},
                                },_=>NativeReplacementReply{job:None,refusal:Some("replacement requires an exact account and available service".into())},
                            };return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="watch-native-replacement" {
                            let reply=match (serde_json::from_str::<WatchNativeReplacementRequest>(body),&manager){
                                (Ok(r),Some(m)) if !r.label.is_empty()=>match m.engine(&r.label).await {
                                    Ok(engine) if engine.account.id==r.expected_account_id && uuid::Uuid::parse_str(&r.expected_account_id).is_ok()=>match m.watch_native_replacement(engine,&r.expected_account_id,r.operation).await {
                                        Ok(job)=>NativeReplacementReply{job:Some(job),refusal:None},Err(_)=>NativeReplacementReply{job:None,refusal:Some("saved replacement observation could not attach".into())},
                                    },_=>NativeReplacementReply{job:None,refusal:Some("selected replacement account changed or is unavailable".into())},
                                },_=>NativeReplacementReply{job:None,refusal:Some("replacement observation request or service unavailable".into())},
                            };return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="list-native-replacements" {
                            let reply=match (serde_json::from_str::<ListNativeReplacementsRequest>(body),&manager){
                                (Ok(r),Some(m)) if !r.label.is_empty() && (1..=100).contains(&r.limit) && r.after.is_none_or(|n|n<=i64::MAX as u64)=>match m.engine(&r.label).await {
                                    Ok(engine) if engine.account.id==r.expected_account_id && uuid::Uuid::parse_str(&r.expected_account_id).is_ok()=>match m.list_native_replacements(&engine,&r.expected_account_id,r.after,r.limit).await {
                                        Ok(page)=>ListNativeReplacementsReply{operations:page.operations,next:page.next,refusal:None},Err(_)=>ListNativeReplacementsReply{refusal:Some("retained replacements unavailable for selected account".into()),..Default::default()},
                                    },_=>ListNativeReplacementsReply{refusal:Some("selected replacement account changed or is unavailable".into()),..Default::default()},
                                },_=>ListNativeReplacementsReply{refusal:Some("replacement listing requires an exact account and bounded page".into()),..Default::default()},
                            };return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="list-native-trash" {
                            let reply=match (serde_json::from_str::<ListNativeTrashRequest>(body),&manager) {
                                (Ok(r),Some(m)) if (1..=100).contains(&r.limit) && r.after.is_none_or(|n|n<=i64::MAX as u64)=>match m.engine(&r.label).await {
                                    Ok(engine) if engine.account.id==r.expected_account_id && uuid::Uuid::parse_str(&r.expected_account_id).is_ok()=>match m.list_native_trash(&engine,&r.expected_account_id,r.after,r.limit).await {
                                        Ok(list)=>ListNativeTrashReply{operations:list.operations,next:list.next,refusal:None},
                                        Err(_)=>ListNativeTrashReply{refusal:Some("retained native Trash operations are unavailable for the selected account".into()),..Default::default()},
                                    },
                                    _=>ListNativeTrashReply{refusal:Some("selected native Trash account is unavailable or changed".into()),..Default::default()},
                                },
                                _=>ListNativeTrashReply{refusal:Some("native Trash listing requires an exact account and a page limit between 1 and 100".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="trash-native-document" {
                            let reply=match (serde_json::from_str::<TrashNativeDocumentRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.engine(&r.label).await {
                                    Ok(engine) if engine.account.id==r.expected_account_id && uuid::Uuid::parse_str(&r.expected_account_id).is_ok()=>match engine.start_native_trash(m.clone(),crate::native_trash::NativeTrashInput{expected_account_id:r.expected_account_id,path:r.path,item_id:r.item_id,etag:r.etag}) {
                                        Ok(job)=>NativeTrashReply{job:Some(job),refusal:None},
                                        Err(_)=>NativeTrashReply{job:None,refusal:Some("native Trash admission could not be started".into())},
                                    },
                                    _=>NativeTrashReply{job:None,refusal:Some("selected native Trash account is unavailable or changed".into())},
                                },
                                _=>NativeTrashReply{job:None,refusal:Some("native Trash request or account service is unavailable".into())},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="watch-native-trash" {
                            let reply=match (serde_json::from_str::<WatchNativeTrashRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.engine(&r.label).await {
                                    Ok(engine) if engine.account.id==r.expected_account_id && uuid::Uuid::parse_str(&r.expected_account_id).is_ok()=>match engine.start_native_trash_watch(m.clone(),r.expected_account_id,r.operation) {
                                        Ok(job)=>NativeTrashReply{job:Some(job),refusal:None},
                                        Err(_)=>NativeTrashReply{job:None,refusal:Some("native Trash observation could not be started".into())},
                                    },
                                    _=>NativeTrashReply{job:None,refusal:Some("selected native Trash account is unavailable or changed".into())},
                                },
                                _=>NativeTrashReply{job:None,refusal:Some("native Trash watch request or account service is unavailable".into())},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb == "list-native-imports" {
                            let reply = match (serde_json::from_str::<ListNativeImportsRequest>(body), &manager) {
                                (Ok(r), Some(m)) if !r.label.is_empty()
                                    && (1..=100).contains(&r.limit)
                                    && r.after.is_none_or(|n| n <= i64::MAX as u64) => match m.engine(&r.label).await {
                                        Ok(engine) if engine.account.id == r.expected_account_id
                                            && uuid::Uuid::parse_str(&r.expected_account_id).is_ok() => match m.list_native_imports(&engine, &r.expected_account_id, r.after, r.limit).await {
                                                Ok(page) => ListNativeImportsReply { operations: page.operations, next: page.next, refusal: None },
                                                Err(_) => ListNativeImportsReply { refusal: Some("saved native imports unavailable for selected account".into()), ..Default::default() },
                                            },
                                        _ => ListNativeImportsReply { refusal: Some("selected import account changed or is unavailable".into()), ..Default::default() },
                                    },
                                _ => ListNativeImportsReply { refusal: Some("import listing requires an exact account and bounded page".into()), ..Default::default() },
                            };
                            return write_reply(&mut stream, &reply).await;
                        }
                        if verb=="watch-native-import" {
                            let reply=match (serde_json::from_str::<WatchNativeImportRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.watch_native_import(&r).await {
                                    Ok(job)=>ImportNativePackageReply {job:Some(job),refusal:None},
                                    Err(_)=>ImportNativePackageReply {job:None,refusal:Some("saved native import or selected writable connection is unavailable".into())},
                                },
                                _=>ImportNativePackageReply {job:None,refusal:Some("native import watch request or account service is unavailable".into())},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="import-native-package" {
                            let reply = match (serde_json::from_str::<ImportNativePackageRequest>(body), &manager) {
                                (Ok(r), Some(m)) => match m.engine(&r.label).await {
                                    Ok(engine) if r.expected_account_id.as_ref().is_some_and(|expected| expected != &engine.account.id) => ImportNativePackageReply { job: None, refusal: Some("native import account changed; select the connection again".into()) },
                                    Ok(engine) => match engine.start_native_import(m.clone(), crate::native_import::NativeImportInput {
                                        source:r.archive, source_layout:r.source_layout, expected_root:r.expected_root, parent:r.parent, name:r.name,
                                    }) {
                                        Ok(job) => ImportNativePackageReply {job:Some(job), refusal:None},
                                        Err(_) => ImportNativePackageReply {job:None, refusal:Some("native import could not be started".into())},
                                    },
                                    Err(_) => ImportNativePackageReply {job:None, refusal:Some("choose an active writable iCloud connection".into())},
                                },
                                _ => ImportNativePackageReply {job:None, refusal:Some("native import request or account service is unavailable".into())},
                            };
                            return write_reply(&mut stream, &reply).await;
                        }
                        if verb=="export-save" {
                            let reply=match (serde_json::from_str::<ExportSaveRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.export_save(&r).await {
                                    Ok(job)=>ExportSaveReply{job:Some(job),refusal:None},
                                    Err(error)=>ExportSaveReply{refusal:Some(error.to_string()),..Default::default()},
                                },
                                _=>ExportSaveReply{refusal:Some("export request or account service is unavailable".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="keep-both" {
                            let reply=match (serde_json::from_str::<DiscardRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.keep_both(&r.label).await {
                                    Ok((kept,considered))=>KeepBothReply{kept,considered,refusal:None},
                                    Err(error)=>KeepBothReply{refusal:Some(error.to_string()),..Default::default()},
                                },
                                (Ok(_),None)=>KeepBothReply{refusal:Some("this service manages no accounts".into()),..Default::default()},
                                (Err(_),_)=>KeepBothReply{refusal:Some("malformed request body".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="recent" {
                            let reply=match (serde_json::from_str::<RecentRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.recent(&r.label,if r.limit==0 {20} else {r.limit.min(200)}).await {
                                    Ok(reply)=>reply,
                                    Err(error)=>RecentReply{refusal:Some(error.to_string()),..Default::default()},
                                },
                                (Ok(_),None)=>RecentReply{refusal:Some("this service manages no accounts".into()),..Default::default()},
                                (Err(_),_)=>RecentReply{refusal:Some("malformed request body".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="paths" || verb=="paths-cached" {
                            let reply=match (serde_json::from_str::<PathsRequest>(body),&manager) {
                                (Ok(r),_) if r.paths.len()>PATHS_PER_REQUEST=>PathsReply{refusal:Some(format!("at most {PATHS_PER_REQUEST} paths per request")),..Default::default()},
                                (Ok(r),Some(m))=>match m.path_states_mode(&r.label,&r.paths,verb=="paths-cached").await {
                                    Ok(states)=>PathsReply{states,refusal:None},
                                    Err(error)=>PathsReply{refusal:Some(error.to_string()),..Default::default()},
                                },
                                (Ok(_),None)=>PathsReply{refusal:Some("this service manages no accounts".into()),..Default::default()},
                                (Err(_),_)=>PathsReply{refusal:Some("malformed request body".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb=="stop-job" {
                            let reply=match (serde_json::from_str::<StopJobRequest>(body),&manager) {
                                (Ok(r),Some(m))=>match m.stop_job(&r.label,&r.id).await {
                                    Ok(outcome)=>StopJobReply{
                                        stopped:!matches!(outcome,jobs::Stopped::Unknown),
                                        already_ended:matches!(outcome,jobs::Stopped::Dismissed),
                                        refusal:None,
                                    },
                                    Err(error)=>StopJobReply{refusal:Some(error.to_string()),..Default::default()},
                                },
                                (Ok(_),None)=>StopJobReply{refusal:Some("this service manages no accounts".into()),..Default::default()},
                                (Err(_),_)=>StopJobReply{refusal:Some("malformed request body".into()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        if verb!="status" {
                            // An error here used to end the exchange with no
                            // reply, which a client reads as a parse failure on
                            // nothing. The error is the reply.
                            let reply=match handle_control(verb,body,&manager).await {
                                Ok(reply)=>reply,
                                Err(error)=>PinReply{refusal:Some(error.to_string()),..Default::default()},
                            };
                            return write_reply(&mut stream,&reply).await;
                        }
                        let (mut feeds,mut items)=tokio::task::spawn_blocking(move || Store::open(path)?.counts()).await??;
                        let mut accounts=match &manager {Some(m)=>m.status.read().await.clone(),None=>vec![]};
                        // Progress is read here rather than taken from the
                        // five-second status loop: a bar that moved five seconds
                        // ago is a spinner with extra steps.
                        if let Some(m)=&manager {
                            let mut running=m.jobs().await;
                            for account in &mut accounts {
                                if let Some(jobs)=running.remove(&account.account_id) {account.jobs=jobs;}
                            }
                        }
                        if manager.is_some() {feeds=accounts.iter().map(|a|a.indexed_feeds).sum();items=accounts.iter().map(|a|a.indexed_items).sum();}
                        let active_mounts=accounts.iter().filter(|a|a.mounted).count() as u64;
                        let reply=Status{protocol_version:STATUS_PROTOCOL_VERSION,version:env!("CARGO_PKG_VERSION").into(),milestone:"writable-preview".into(),indexed_feeds:feeds,indexed_items:items,active_mounts,accounts,allocator_trims:crate::filesystem::allocator_trims(),free_arena_bytes:cirrove_allocator::free_arena_bytes(),retained_bytes:cirrove_allocator::retained_bytes(),restart_required:crate::binary_replaced_on_disk()};
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
    async fn native_trash_capabilities_and_missing_manager_refuse_without_starting_jobs() {
        let (_dir, socket, cancel, task) = serving(None).await;
        let caps = capabilities(&socket).await.unwrap();
        assert_eq!(caps.capabilities.get("trash-native-document"), Some(&1));
        assert_eq!(caps.capabilities.get("watch-native-trash"), Some(&1));
        let account = uuid::Uuid::new_v4().to_string();
        let reply = trash_native_document(
            &socket,
            &TrashNativeDocumentRequest {
                label: "Cloud".into(),
                expected_account_id: account.clone(),
                path: "Own.pages".into(),
                item_id: "FILE::com.apple.CloudDocs::own".into(),
                etag: "E1".into(),
            },
        )
        .await
        .unwrap();
        assert!(reply.job.is_none() && reply.refusal.is_some());
        let reply = watch_native_trash(
            &socket,
            &WatchNativeTrashRequest {
                label: "Cloud".into(),
                expected_account_id: account,
                operation: uuid::Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
        assert!(reply.job.is_none() && reply.refusal.is_some());
        cancel.cancel();
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn native_trash_list_protocol_is_bounded_and_read_only() {
        let (_dir, socket, cancel, task) = serving(None).await;
        assert_eq!(
            capabilities(&socket)
                .await
                .unwrap()
                .capabilities
                .get("list-native-trash"),
            Some(&1)
        );
        let body = serde_json::json!({"label":"Cloud","expected_account_id":uuid::Uuid::new_v4().to_string(),"after":0,"limit":100});
        for field in ["path", "item_id", "etag", "retry", "permanent", "operation"] {
            let mut extra = body.clone();
            extra[field] = serde_json::json!("forbidden");
            assert!(serde_json::from_value::<ListNativeTrashRequest>(extra).is_err());
        }
        for limit in [0, 1, 100, 101] {
            let mut request: ListNativeTrashRequest = serde_json::from_value(body.clone()).unwrap();
            request.limit = limit;
            let reply = list_native_trash(&socket, &request).await.unwrap();
            assert!(reply.operations.is_empty() && reply.next.is_none() && reply.refusal.is_some());
        }
        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[test]
    fn native_trash_protocol_requires_binding_and_watch_has_no_mutation_arguments() {
        let input = serde_json::json!({"label":"Cloud","expected_account_id":uuid::Uuid::new_v4().to_string(),"path":"Own.pages","item_id":"FILE::com.apple.CloudDocs::own","etag":"E1"});
        assert!(serde_json::from_value::<TrashNativeDocumentRequest>(input.clone()).is_ok());
        for field in ["expected_account_id", "path", "item_id", "etag"] {
            let mut missing = input.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<TrashNativeDocumentRequest>(missing).is_err());
        }
        let watch = serde_json::json!({"label":"Cloud","expected_account_id":uuid::Uuid::new_v4().to_string(),"operation":uuid::Uuid::new_v4()});
        for field in ["path", "item_id", "etag", "retry", "permanent", "archive"] {
            let mut extra = watch.clone();
            extra[field] = serde_json::json!("forbidden");
            assert!(serde_json::from_value::<WatchNativeTrashRequest>(extra).is_err());
        }
        let mut extra = input;
        extra["permanent"] = serde_json::json!(true);
        assert!(serde_json::from_value::<TrashNativeDocumentRequest>(extra).is_err());
    }
    #[tokio::test]
    async fn native_trash_client_transmits_unusual_paths_as_one_structured_request() {
        use tokio::io::AsyncBufReadExt;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let path = "Folder/Own \"quoted\"; $()\nline.pages".to_owned();
        let expected = path.clone();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let body = line.strip_prefix("trash-native-document ").unwrap();
            let parsed: TrashNativeDocumentRequest = serde_json::from_str(body).unwrap();
            assert_eq!(parsed.path, expected);
            assert_eq!(line.bytes().filter(|b| *b == b'\n').count(), 1);
            reader
                .get_mut()
                .write_all(b"{\"job\":null,\"refusal\":\"synthetic refusal\"}\n")
                .await
                .unwrap();
        });
        let reply = trash_native_document(
            &socket,
            &TrashNativeDocumentRequest {
                label: "Cloud".into(),
                expected_account_id: uuid::Uuid::new_v4().to_string(),
                path,
                item_id: "FILE::com.apple.CloudDocs::own".into(),
                etag: "E1".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(reply.refusal.as_deref(), Some("synthetic refusal"));
        task.await.unwrap();
    }

    /// The daemon cannot see a dialogue, so it will not act on the assumption
    /// that one happened. A client that forgot to ask would otherwise destroy
    /// files on its own say-so, and this is the one operation with no way back.
    #[tokio::test]
    async fn permanent_deletion_refuses_a_request_that_did_not_ask_the_person() {
        let (_dir, socket, cancel, task) = serving(None).await;
        let body = serde_json::json!({
            "label": "",
            "paths": ["Note.txt"],
            "confirmed": false,
        });
        let reply = ask(&socket, &format!("delete-permanently {body}\n")).await;
        let parsed: PermanentDeleteReply = serde_json::from_str(&reply).unwrap();
        assert!(parsed.deletions.is_empty(), "nothing may be removed");
        let refusal = parsed.refusal.unwrap_or_default();
        assert!(
            refusal.contains("asked"),
            "the refusal must say what is missing, so a client author can fix it \
             rather than guess: {refusal}"
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
                &CancellationToken::new(),
                None,
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

    struct SnapshotProvider(std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl MetadataProvider for SnapshotProvider {
        fn provider_id(&self) -> &'static str {
            "snapshot-fixture"
        }
        fn feed_mode(&self) -> FeedMode {
            FeedMode::FullSnapshot
        }
        async fn changes(
            &self,
            _scope: &Scope,
            cursor: Option<&cirrove_core::Cursor>,
            _cancel: &CancellationToken,
        ) -> std::result::Result<cirrove_core::ChangePage, ProviderError> {
            if cursor.is_some() {
                return Err(ProviderError::Protocol("completed snapshot was reused"));
            }
            let round = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let id = format!("round-{round}");
            Ok(cirrove_core::ChangePage {
                changes: vec![cirrove_core::Change::Upsert(cirrove_core::Node {
                    id: id.clone(),
                    parent_id: None,
                    name: id,
                    kind: cirrove_core::NodeKind::File,
                    size: 1,
                    modified_unix: 0,
                    etag: None,
                    content_version: None,
                    target: None,
                    package: false,
                })],
                checkpoint: cirrove_core::Checkpoint::Complete(cirrove_core::Cursor(format!(
                    "done-{round}"
                ))),
            })
        }
    }
    #[tokio::test]
    async fn coordinator_starts_a_new_snapshot_after_a_completed_round() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        let scope = Scope {
            account: "a".into(),
            provider: "snapshot-fixture".into(),
            collection: "d".into(),
        };
        let provider = SnapshotProvider(std::sync::atomic::AtomicUsize::new(0));
        for round in 0..2 {
            refresh(
                &provider,
                &scope,
                &path,
                false,
                &CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
            let nodes = Store::open(&path).unwrap().nodes(&scope).unwrap();
            assert_eq!(nodes.len(), 1);
            assert_eq!(nodes[0].id, format!("round-{round}"));
        }
    }
    #[test]
    fn native_source_layout_preserves_legacy_wire_and_rejects_ambiguous_roots() {
        let import = serde_json::json!({"label":"Owned","archive":"/var/tmp/Owned.zip","expected_root":"Owned.numbers","parent":"","name":"Copy.numbers"});
        let replace = serde_json::json!({"label":"Owned","expected_account_id":uuid::Uuid::new_v4(),"path":"Owned.numbers","item_id":"FILE::com.apple.CloudDocs::owned","etag":"v1","archive":"/var/tmp/Owned.zip","expected_root":"Owned.numbers"});
        for (is_replace, legacy) in [(false, import), (true, replace)] {
            let decode =
                |value: serde_json::Value| -> Result<serde_json::Value, serde_json::Error> {
                    if is_replace {
                        serde_json::from_value::<ReplaceNativePackageRequest>(value)
                            .and_then(serde_json::to_value)
                    } else {
                        serde_json::from_value::<ImportNativePackageRequest>(value)
                            .and_then(serde_json::to_value)
                    }
                };
            assert_eq!(decode(legacy.clone()).unwrap(), legacy);
            for null_root in [false, true] {
                let mut value = legacy.clone();
                if null_root {
                    value["expected_root"] = serde_json::Value::Null;
                } else {
                    value.as_object_mut().unwrap().remove("expected_root");
                }
                assert!(
                    decode(value.clone()).is_err(),
                    "wrapped root must be present and nonnull"
                );
                value["source_layout"] = serde_json::json!("flat_numbers");
                let flat = decode(value).unwrap();
                assert_eq!(flat["source_layout"], "flat_numbers");
                assert!(flat["expected_root"].is_null());
            }
            for layout in ["flat_numbers", "guessed", "flat-numbers"] {
                let mut value = legacy.clone();
                value["source_layout"] = serde_json::json!(layout);
                assert!(
                    decode(value).is_err(),
                    "unknown layout or nonnull flat root"
                );
            }
            let mut value = legacy.clone();
            value["source_layout"] = serde_json::json!("wrapped");
            assert_eq!(decode(value).unwrap(), legacy);
        }
    }
    #[test]
    fn native_replacement_protocol_requires_exact_selection_and_observers_reject_write_fields() {
        let input = serde_json::json!({"label":"Owned","expected_account_id":uuid::Uuid::new_v4(),"path":"Owned.pages","item_id":"FILE::com.apple.CloudDocs::owned","etag":"v1","archive":"/var/tmp/Owned.zip","expected_root":"Owned.pages"});
        assert!(serde_json::from_value::<ReplaceNativePackageRequest>(input.clone()).is_ok());
        for field in [
            "expected_account_id",
            "path",
            "item_id",
            "etag",
            "archive",
            "expected_root",
        ] {
            let mut v = input.clone();
            v.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<ReplaceNativePackageRequest>(v).is_err(),
                "{field}"
            );
        }
        let watch = serde_json::json!({"label":"Owned","expected_account_id":uuid::Uuid::new_v4(),"operation":uuid::Uuid::new_v4()});
        let list = serde_json::json!({"label":"Owned","expected_account_id":uuid::Uuid::new_v4(),"limit":100});
        for key in [
            "archive",
            "expected_root",
            "source_layout",
            "item_id",
            "etag",
            "retry",
        ] {
            let mut w = watch.clone();
            w[key] = serde_json::json!("forbidden");
            assert!(serde_json::from_value::<WatchNativeReplacementRequest>(w).is_err());
            let mut l = list.clone();
            l[key] = serde_json::json!("forbidden");
            assert!(serde_json::from_value::<ListNativeReplacementsRequest>(l).is_err());
        }
    }
    #[tokio::test]
    async fn native_replacement_public_routes_refuse_missing_manager_and_invalid_pages() {
        let (_dir, socket, cancel, task) = serving(None).await;
        let caps = capabilities(&socket).await.unwrap();
        for name in [
            "replace-native-package",
            "watch-native-replacement",
            "list-native-replacements",
        ] {
            assert_eq!(caps.capabilities.get(name), Some(&1));
        }
        let account = uuid::Uuid::new_v4().to_string();
        let reply = replace_native_package(
            &socket,
            &ReplaceNativePackageRequest {
                label: "Owned".into(),
                expected_account_id: account.clone(),
                path: "Owned.pages".into(),
                item_id: "FILE::com.apple.CloudDocs::owned".into(),
                etag: "v1".into(),
                archive: "/var/tmp/Owned.zip".into(),
                source_layout: native_import::PackageSourceLayout::Wrapped,
                expected_root: Some("Owned.pages".into()),
            },
        )
        .await
        .unwrap();
        assert!(reply.job.is_none() && reply.refusal.is_some());
        let reply = watch_native_replacement(
            &socket,
            &WatchNativeReplacementRequest {
                label: "Owned".into(),
                expected_account_id: account.clone(),
                operation: uuid::Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
        assert!(reply.job.is_none() && reply.refusal.is_some());
        for (limit, after) in [(0, None), (101, None), (100, Some(u64::MAX)), (100, None)] {
            let reply = list_native_replacements(
                &socket,
                &ListNativeReplacementsRequest {
                    label: "Owned".into(),
                    expected_account_id: account.clone(),
                    limit,
                    after,
                },
            )
            .await
            .unwrap();
            assert!(reply.refusal.is_some() && reply.operations.is_empty() && reply.next.is_none());
        }
        cancel.cancel();
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn native_replacement_client_preserves_structured_archive_and_selection() {
        use tokio::io::AsyncBufReadExt;
        for source_layout in [
            native_import::PackageSourceLayout::Wrapped,
            native_import::PackageSourceLayout::FlatNumbers,
        ] {
            let dir = tempfile::tempdir().unwrap().keep().join("private");
            private_dir(&dir).unwrap();
            let socket = dir.join("control.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let input = ReplaceNativePackageRequest {
                label: "Owned".into(),
                expected_account_id: uuid::Uuid::new_v4().to_string(),
                path: "Folder/quote \"; $(literal).pages".into(),
                item_id: "FILE::com.apple.CloudDocs::owned".into(),
                etag: "e-tag".into(),
                archive: PathBuf::from("/var/tmp/source \"; $(literal)\n.zip"),
                source_layout,
                expected_root: (source_layout == native_import::PackageSourceLayout::Wrapped)
                    .then(|| "Source.pages".into()),
            };
            let expected = serde_json::to_value(&input).unwrap();
            if source_layout == native_import::PackageSourceLayout::Wrapped {
                assert!(expected.get("source_layout").is_none());
            } else {
                assert_eq!(expected["source_layout"], "flat_numbers");
                assert!(expected["expected_root"].is_null());
            }
            let task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let request: ReplaceNativePackageRequest =
                    serde_json::from_str(line.strip_prefix("replace-native-package ").unwrap())
                        .unwrap();
                assert_eq!(serde_json::to_value(request).unwrap(), expected);
                assert_eq!(line.bytes().filter(|b| *b == b'\n').count(), 1);
                reader
                    .get_mut()
                    .write_all(b"{\"job\":null,\"refusal\":\"synthetic\"}\n")
                    .await
                    .unwrap();
            });
            assert_eq!(
                replace_native_package(&socket, &input)
                    .await
                    .unwrap()
                    .refusal
                    .as_deref(),
                Some("synthetic")
            );
            task.await.unwrap();
        }
    }
}

#[cfg(test)]
mod a_replaced_binary {
    use super::exe_link_says_replaced;

    #[test]
    fn the_kernels_marker_is_what_says_an_upgrade_happened_underneath_us() {
        // This exact string is what /proc/<pid>/exe read on Fedora 44 after
        // dnf upgrade, with the daemon still serving from the old inode.
        assert!(exe_link_says_replaced("/usr/bin/cirroved (deleted)"));
    }

    #[test]
    fn an_ordinary_running_binary_is_not_a_pending_restart() {
        assert!(!exe_link_says_replaced("/usr/bin/cirroved"));
        assert!(!exe_link_says_replaced(
            "/home/someone/Work/cirrove/target/release/cirroved"
        ));
    }

    #[test]
    fn the_marker_counts_only_at_the_end_where_the_kernel_puts_it() {
        // A directory that merely contains the word must not read as an
        // upgrade: the kernel appends the marker, it does not embed it.
        assert!(!exe_link_says_replaced("/opt/ (deleted)/cirroved"));
        assert!(!exe_link_says_replaced("/usr/bin/cirroved (deleted) "));
    }
}

#[cfg(test)]
mod readable_sizes {
    use super::human_bytes;

    #[test]
    fn a_size_is_shown_in_a_unit_that_does_not_round_it_away() {
        // The case that prompted this: a 66 KB pin printed as "0 MiB".
        assert_eq!(human_bytes(67_826), "66 KB");
        assert_eq!(human_bytes(0), "0 bytes");
        assert_eq!(human_bytes(1), "1 bytes");
        assert_eq!(human_bytes(1023), "1023 bytes");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_bytes(66 * 1024 * 1024), "66 MB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn nothing_a_daemon_can_report_is_shown_as_zero_unless_it_is_zero() {
        for bytes in [1_u64, 999, 1024, 100_000, 1_048_576, 1 << 30, u64::MAX] {
            let shown = human_bytes(bytes);
            assert!(
                !shown.starts_with("0 ") && !shown.starts_with("0."),
                "{bytes} bytes reads as {shown}, which says there is nothing there"
            );
        }
    }
}
