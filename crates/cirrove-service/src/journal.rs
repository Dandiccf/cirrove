//! Durable local upload snapshots. This module does not enable filesystem writes.
//!
//! Call on a blocking worker. Seal bytes and fsync their directory before committing
//! the pending row. Network work happens after a claim returns, outside this module.
//! An interrupted attempt requires remote verification, never unconditional replay.
mod ancestry;
mod barriers;
mod directories;
mod generations;
mod handoff;
mod mutations;
mod namespace;
mod preparation;
mod publication;
mod replacements;
mod unlinked;
mod working;
pub(crate) use ancestry::RetainedAncestors;
use barriers::WriteOrder;
use cirrove_core::{Node, NodeKind, Scope};
pub use generations::{UploadBase, WriteBase};
pub use mutations::{MutationRecord, MutationState};
pub(crate) use namespace::project_retained_namespace;
pub use namespace::{
    NamespaceCollision, NamespaceListing, NamespaceNames, NamespaceObject, project_namespace,
};
pub use preparation::UploadPreparation;
pub use publication::{NamespacePublication, NamespaceSnapshot};
pub use replacements::ReplacementRecord;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
pub use unlinked::UnlinkedFile;
use uuid::Uuid;
pub use working::{WorkingFile, WorkingSource};

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("local upload storage is unavailable")]
    Storage,
    #[error("upload journal is already owned by another process")]
    Busy,
    #[error("upload journal belongs to another account")]
    Account,
    #[error("invalid upload intent")]
    Intent,
    #[error("local pending-upload storage limit reached")]
    Quota,
    #[error("upload operation not found")]
    Missing,
    #[error("upload attempt is stale or out of order")]
    Stale,
    #[error("local upload snapshot failed integrity verification")]
    Corrupt,
    #[error("unsupported upload journal schema")]
    Schema,
}
impl From<std::io::Error> for JournalError {
    fn from(error: std::io::Error) -> Self {
        match error.raw_os_error() {
            Some(libc::ENOSPC | libc::EDQUOT) => Self::Quota,
            _ => Self::Storage,
        }
    }
}
impl From<rusqlite::Error> for JournalError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}
impl From<serde_json::Error> for JournalError {
    fn from(_: serde_json::Error) -> Self {
        Self::Corrupt
    }
}
pub type Result<T> = std::result::Result<T, JournalError>;

pub use cirrove_core::upload::UploadIntent;
fn resource(intent: &UploadIntent, scope: &Scope) -> Result<String> {
    Ok(match intent {
        UploadIntent::Create { parent, name } => {
            serde_json::to_string(&(scope, "create", parent, name))?
        }
        UploadIntent::Replace { item, .. } => serde_json::to_string(&(scope, "replace", item))?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadState {
    /// A durable namespace operation whose unchanged cloud source is being
    /// captured. This state has no uploadable payload or remote attempt.
    Preparing,
    Pending,
    Uploading,
    VerifyRequired,
    Verifying,
    Uploaded,
    Conflict,
    Failed,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct UploadRecord {
    pub id: Uuid,
    pub sequence: u64,
    pub scope: Scope,
    pub intent: UploadIntent,
    pub state: UploadState,
    pub size: u64,
    pub sha256: String,
    /// An attempt token fences delayed results after restart or retry.
    pub attempt: Option<Uuid>,
    pub remote: Option<Node>,
    /// A preceding save or namespace operation supplies the confirmed base version.
    #[serde(default)]
    pub base: Option<UploadBase>,
    #[serde(default)]
    pub working_file: Option<Uuid>,
    /// Reference to an opaque checkpoint stored in the credential vault.
    #[serde(default)]
    pub session_key: Option<Uuid>,
    /// Contiguous prefix acknowledged by the remote session, not a committed file.
    #[serde(default)]
    pub transferred_bytes: u64,
    #[serde(default)]
    pub retry_at: u64,
    #[serde(default)]
    pub failed_attempts: u32,
}
impl std::fmt::Debug for UploadRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadRecord")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

enum GenerationCommit {
    Working(working::WorkingCommit),
    Replacement(Box<replacements::ReplacementCommit>),
}
impl GenerationCommit {
    fn working_id(&self) -> Option<Uuid> {
        match self {
            Self::Working(w) => Some(w.id),
            Self::Replacement(r) => r.working_id(),
        }
    }
}

pub struct UploadJournal {
    db: Connection,
    objects: PathBuf,
    working: PathBuf,
    account: String,
    quota: u64,
    _owner: File,
}
fn owned_private(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != std::fs::metadata("/proc/self")?.uid()
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(JournalError::Storage);
    }
    Ok(())
}
fn private_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    owned_private(&file)?;
    Ok(file)
}
impl UploadJournal {
    /// One owner per account journal. This is a separate database from the
    /// evictable metadata/cache index: pending edits are never cache entries.
    pub fn open(root: &Path, account: &str, quota: u64) -> Result<Self> {
        if account.is_empty() || quota == 0 {
            return Err(JournalError::Intent);
        }
        crate::private_dir(root).map_err(|_| JournalError::Storage)?;
        let owner = private_file(&root.join("owner.lock"))?;
        fs2::FileExt::try_lock_exclusive(&owner).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                JournalError::Busy
            } else {
                JournalError::Storage
            }
        })?;
        let objects = root.join("objects");
        crate::private_dir(&objects).map_err(|_| JournalError::Storage)?;
        let working = root.join("working");
        crate::private_dir(&working).map_err(|_| JournalError::Storage)?;
        let database = root.join("uploads.db");
        private_file(&database)?;
        let mut db = Connection::open(database)?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 14 {
            return Err(JournalError::Schema);
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1), account TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS uploads (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
                resource TEXT NOT NULL, state TEXT NOT NULL, body TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS upload_order ON uploads(resource,sequence);
            CREATE INDEX IF NOT EXISTS upload_state ON uploads(state,sequence);
            ")?;
        db.execute("INSERT OR IGNORE INTO identity VALUES(1,?1)", [account])?;
        let stored: String = db.query_row("SELECT account FROM identity", [], |r| r.get(0))?;
        if stored != account {
            return Err(JournalError::Account);
        }
        mutations::migrate_queue(&mut db, version)?;
        generations::migrate(&mut db, version)?;
        working::migrate(&mut db, version)?;
        generations::migrate_dependencies(&mut db, version)?;
        namespace::migrate(&mut db, version)?;
        handoff::migrate(&mut db, version)?;
        unlinked::migrate(&mut db, version)?;
        barriers::migrate(&mut db, version)?;
        replacements::migrate(&mut db, version)?;
        publication::migrate(&mut db, version)?;
        preparation::migrate(&mut db, version)?;
        directories::migrate(&mut db, version)?;
        // Never infer that a transfer failed just because its process died.
        db.execute(
            "UPDATE uploads SET state='verify_required',
            body=json_set(body,'$.state','verify_required','$.attempt',NULL)
            WHERE state IN ('uploading','verifying')",
            [],
        )?;
        File::open(&objects)?.sync_all()?;
        File::open(&working)?.sync_all()?;
        File::open(root)?.sync_all()?;
        // Persist newly created ancestor entries too; syncing only the immediate
        // parent is insufficient when a whole account directory was just made.
        for parent in root.ancestors().skip(1) {
            File::open(parent)?.sync_all()?;
        }
        let mut journal = Self {
            db,
            objects,
            working,
            account: account.into(),
            quota,
            _owner: owner,
        };
        journal.recover_preparation_files()?;
        journal.recover_working()?;
        journal.recover_unlinked_readers()?;
        journal.recover_replacement_readers()?;
        journal.collect_retired_working(1000)?;
        Ok(journal)
    }
    /// Counts every spool file, including interrupted, unreferenced publications.
    /// Such bytes are retained for inspection and cannot silently bypass quota.
    pub fn retained_bytes(&self) -> Result<u64> {
        self.retained_usage().map(|(bytes, _)| bytes)
    }
    fn retained_usage(&self) -> Result<(u64, usize)> {
        let mut total = 0u64;
        let mut count = 0;
        for (index, entry) in std::fs::read_dir(&self.objects)?
            .chain(std::fs::read_dir(&self.working)?)
            .enumerate()
        {
            // Empty files must not bypass the local queue's resource bounds.
            if index >= 10_000 {
                return Err(JournalError::Quota);
            }
            let meta = std::fs::symlink_metadata(entry?.path())?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(JournalError::Corrupt);
            }
            total = total.checked_add(meta.len()).ok_or(JournalError::Quota)?;
            count += 1;
        }
        Ok((total, count))
    }
    /// Returns only after the complete immutable snapshot and journal row are
    /// durable. Reader errors, disk-full and quota failures never enqueue a row.
    /// The caller must hold its edit-generation guard while providing the source;
    /// copying from a concurrently modified file does not define a snapshot.
    pub fn enqueue(
        &mut self,
        scope: Scope,
        intent: UploadIntent,
        bytes: impl Read,
    ) -> Result<UploadRecord> {
        self.enqueue_generation(scope, intent, WriteOrder::default(), None, bytes)
    }
    fn enqueue_generation(
        &mut self,
        scope: Scope,
        intent: UploadIntent,
        order: WriteOrder,
        working: Option<GenerationCommit>,
        mut bytes: impl Read,
    ) -> Result<UploadRecord> {
        if scope.account != self.account {
            return Err(JournalError::Account);
        }
        if scope.provider.is_empty() || scope.collection.is_empty() {
            return Err(JournalError::Intent);
        }
        intent.validate().map_err(|_| JournalError::Intent)?;
        if let Some(base) = &order.base {
            self.ensure_successor_free(base.predecessor)?;
        }
        barriers::validate(&self.db, &scope, &order.prerequisites)?;
        let (retained, files) = self.retained_usage()?;
        if files >= 10_000 {
            return Err(JournalError::Quota);
        }
        let available = self.quota.saturating_sub(retained);
        let mut temporary = tempfile::NamedTempFile::new_in(&self.objects)?;
        let mut size = 0u64;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 128 * 1024];
        loop {
            let count = bytes.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            size = size.checked_add(count as u64).ok_or(JournalError::Quota)?;
            if size > available {
                return Err(JournalError::Quota);
            }
            temporary.write_all(&buffer[..count])?;
            hash.update(&buffer[..count]);
        }
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o400))?;
        temporary.as_file().sync_all()?;
        let mut record = UploadRecord {
            id: Uuid::new_v4(),
            sequence: 0,
            scope,
            intent,
            state: UploadState::Pending,
            size,
            sha256: format!("{:x}", hash.finalize()),
            attempt: None,
            remote: None,
            base: order.base,
            working_file: working.as_ref().and_then(GenerationCommit::working_id),
            session_key: None,
            transferred_bytes: 0,
            retry_at: 0,
            failed_attempts: 0,
        };
        temporary
            .persist_noclobber(self.objects.join(record.id.to_string()))
            .map_err(|_| JournalError::Storage)?;
        File::open(&self.objects)?.sync_all()?;
        // Failures after publication retain an orphan; never delete possibly
        // acknowledged bytes in an error/recovery path.
        let tx = self.db.transaction()?;
        record.sequence = mutations::queue_insert(
            &tx,
            record.id,
            mutations::upload_resources(&record.scope, &record.intent)?,
        )?;
        if record.base.is_none()
            && let UploadIntent::Create { parent, .. } = &record.intent
        {
            directories::bind(&tx, record.id, record.sequence, &record.scope, parent)?;
        }
        generations::insert_dependency(&tx, record.id, record.sequence, record.base.as_ref())?;
        barriers::insert(
            &tx,
            record.id,
            record.sequence,
            &record.scope,
            &order.prerequisites,
        )?;
        tx.execute(
            "INSERT INTO uploads(id,resource,state,body,sequence) VALUES(?1,?2,'pending',?3,?4)",
            params![
                record.id.to_string(),
                resource(&record.intent, &record.scope)?,
                serde_json::to_string(&record)?,
                record.sequence as i64
            ],
        )?;
        tx.execute(
            "UPDATE uploads SET body=?2 WHERE id=?1",
            params![record.id.to_string(), serde_json::to_string(&record)?],
        )?;
        if let Some(commit) = &working {
            match commit {
                GenerationCommit::Working(commit) => {
                    working::commit_generation(&tx, commit, &record)?
                }
                GenerationCommit::Replacement(commit) => {
                    replacements::commit(&tx, commit, &record)?
                }
            }
        }
        tx.commit()?;
        Ok(record)
    }
    pub fn get(&self, id: Uuid) -> Result<UploadRecord> {
        let body: String = self
            .db
            .query_row(
                "SELECT body FROM uploads WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(JournalError::Missing)?;
        Ok(serde_json::from_str(&body)?)
    }
    pub fn list(&self, after: u64, limit: u32) -> Result<Vec<UploadRecord>> {
        let Ok(after) = i64::try_from(after) else {
            return Ok(Vec::new());
        };
        let mut query = self
            .db
            .prepare("SELECT body FROM uploads WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
        let rows = query.query_map(params![after, limit.clamp(1, 1000)], |r| {
            r.get::<_, String>(0)
        })?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    /// Verify a sealed snapshot using bounded memory, then return an owned,
    /// read-only file descriptor positioned at byte zero for the transfer worker.
    pub fn payload(&self, id: Uuid) -> Result<File> {
        let record = self.get(id)?;
        if record.state == UploadState::Preparing || record.sha256.len() != 64 {
            return Err(JournalError::Stale);
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.objects.join(id.to_string()))
            .map_err(|_| JournalError::Corrupt)?;
        owned_private(&file)?;
        if file.metadata()?.len() != record.size {
            return Err(JournalError::Corrupt);
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 128 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        if format!("{:x}", hash.finalize()) != record.sha256 {
            return Err(JournalError::Corrupt);
        }
        file.seek(SeekFrom::Start(0))?;
        Ok(file)
    }
    /// One unresolved edit blocks later edits of that identity, but independent
    /// files may progress. Network callers must drop their journal lock on return.
    pub fn claim_next(&mut self) -> Result<Option<UploadRecord>> {
        if !self.resolve_ready_generations()? {
            return Ok(None);
        }
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT u.body FROM uploads u WHERE u.state='pending'
                AND NOT EXISTS(SELECT 1 FROM write_destinations d WHERE d.operation=u.id AND d.resolved=0)
                AND (json_extract(u.body,'$.base') IS NULL OR json_extract(u.body,'$.base.resolved')=1)
                AND NOT EXISTS (SELECT 1 FROM file_replacements r WHERE r.id=u.id AND json_extract(r.body,'$.local_ready')=0)
                AND NOT EXISTS (SELECT 1 FROM write_prerequisites b LEFT JOIN write_queue p ON p.id=b.predecessor
                    WHERE b.operation=u.id AND coalesce(p.complete,0)!=1)
                AND COALESCE(json_extract(u.body,'$.retry_at'),0)<=?1 AND NOT EXISTS (
                SELECT 1 FROM write_queue previous
                JOIN write_resources a ON a.id=previous.id
                JOIN write_resources b ON b.resource=a.resource AND b.id=u.id
                WHERE previous.sequence<u.sequence AND previous.complete=0)
             ORDER BY u.sequence LIMIT 1",
                [now_seconds() as i64],
                |r| r.get(0),
            )
            .optional()?;
        let Some(body) = body else {
            return Ok(None);
        };
        let mut record: UploadRecord = serde_json::from_str(&body)?;
        if let Err(error) = self.payload(record.id) {
            record.state = UploadState::Failed;
            self.save(&record)?;
            return Err(error);
        }
        record.state = UploadState::Uploading;
        record.attempt = Some(Uuid::new_v4());
        self.save(&record)?;
        Ok(Some(record))
    }
    fn save(&mut self, record: &UploadRecord) -> Result<()> {
        let state = serde_json::to_value(record.state)?;
        let state = state.as_str().ok_or(JournalError::Corrupt)?;
        let tx = self.db.transaction()?;
        let changed = tx.execute(
            "UPDATE uploads SET state=?2,body=?3 WHERE id=?1",
            params![record.id.to_string(), state, serde_json::to_string(record)?],
        )?;
        if changed != 1 {
            return Err(JournalError::Missing);
        }
        mutations::queue_complete(&tx, record.id, record.state == UploadState::Uploaded)?;
        if record.state == UploadState::Uploaded
            && let Some(remote) = &record.remote
            && !replacements::confirm(&tx, record.id, record.sequence, remote)?
        {
            namespace::confirm(&tx, record.id, record.sequence, remote)?;
        }
        tx.commit()?;
        Ok(())
    }
    fn active_attempt(&self, id: Uuid, attempt: Uuid) -> Result<UploadRecord> {
        let record = self.get(id)?;
        if !matches!(
            record.state,
            UploadState::Uploading | UploadState::Verifying
        ) || record.attempt != Some(attempt)
        {
            return Err(JournalError::Stale);
        }
        Ok(record)
    }
    pub fn claim_next_verification(&mut self) -> Result<Option<UploadRecord>> {
        if !self.resolve_ready_generations()? {
            return Ok(None);
        }
        let id: Option<String> = self
            .db
            .query_row(
                "SELECT u.id FROM uploads u WHERE u.state='verify_required'
                AND NOT EXISTS(SELECT 1 FROM write_destinations d WHERE d.operation=u.id AND d.resolved=0)
                AND (json_extract(u.body,'$.base') IS NULL OR json_extract(u.body,'$.base.resolved')=1)
                AND NOT EXISTS (SELECT 1 FROM file_replacements r WHERE r.id=u.id AND json_extract(r.body,'$.local_ready')=0)
                AND NOT EXISTS (SELECT 1 FROM write_prerequisites b LEFT JOIN write_queue p ON p.id=b.predecessor
                    WHERE b.operation=u.id AND coalesce(p.complete,0)!=1)
                AND COALESCE(json_extract(u.body,'$.retry_at'),0)<=?1 AND NOT EXISTS (
                SELECT 1 FROM write_queue previous
                JOIN write_resources a ON a.id=previous.id
                JOIN write_resources b ON b.resource=a.resource AND b.id=u.id
                WHERE previous.sequence<u.sequence AND previous.complete=0)
                ORDER BY u.sequence LIMIT 1",
                [now_seconds() as i64],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| {
            self.claim_verification(Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?)
        })
        .transpose()
    }
    /// Persist only a credential reference and verified range progress. The
    /// checkpoint itself must already be saved to the credential vault.
    pub fn record_session(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        key: Uuid,
        transferred: u64,
    ) -> Result<()> {
        let mut record = self.active_attempt(id, attempt)?;
        if transferred > record.size {
            return Err(JournalError::Stale);
        }
        record.session_key = Some(key);
        record.transferred_bytes = transferred;
        record.state = UploadState::Uploading;
        self.save(&record)
    }
    /// Use VerifyRequired for an ambiguous network result. Conflict and Failed
    /// retain bytes and never silently retry or turn into upload success.
    pub fn stop_attempt(&mut self, id: Uuid, attempt: Uuid, state: UploadState) -> Result<()> {
        self.defer_attempt(id, attempt, state, std::time::Duration::ZERO)
    }
    pub fn defer_attempt(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        state: UploadState,
        delay: std::time::Duration,
    ) -> Result<()> {
        if !matches!(
            state,
            UploadState::VerifyRequired | UploadState::Conflict | UploadState::Failed
        ) {
            return Err(JournalError::Stale);
        }
        let mut record = self.active_attempt(id, attempt)?;
        record.state = state;
        record.attempt = None;
        record.retry_at = now_seconds()
            .saturating_add(delay.as_secs())
            .min(i64::MAX as u64);
        record.failed_attempts = record.failed_attempts.saturating_add(1);
        self.save(&record)
    }
    /// The provider worker must prove the old attempt did not commit before
    /// calling this. A conflict requires a new user-approved intent, not replay.
    pub fn retry_verified_uncommitted(&mut self, id: Uuid) -> Result<()> {
        let mut record = self.get(id)?;
        if !matches!(
            record.state,
            UploadState::VerifyRequired | UploadState::Failed
        ) {
            return Err(JournalError::Stale);
        }
        self.payload(id)?;
        record.state = UploadState::Pending;
        record.attempt = None;
        record.retry_at = 0;
        record.session_key = None;
        record.transferred_bytes = 0;
        self.save(&record)
    }
    /// A user retry or reauthentication requests verification, never a blind
    /// replay. Conflicts require an explicit resolution intent.
    pub fn request_retry(&mut self, id: Uuid) -> Result<()> {
        let mut record = self.get(id)?;
        if record.base.as_ref().is_some_and(|base| !base.resolved) {
            return Err(JournalError::Stale);
        }
        if !matches!(
            record.state,
            UploadState::Failed | UploadState::VerifyRequired
        ) {
            return Err(JournalError::Stale);
        }
        record.state = UploadState::VerifyRequired;
        record.retry_at = 0;
        self.save(&record)
    }
    /// Reserve an uncertain operation for read-only remote reconciliation. This
    /// does not authorize another upload. A matching remote result can be
    /// acknowledged with this new attempt token; interruption requires recheck.
    pub fn claim_verification(&mut self, id: Uuid) -> Result<UploadRecord> {
        let mut record = self.get(id)?;
        if record.state != UploadState::VerifyRequired
            || record.base.as_ref().is_some_and(|base| !base.resolved)
            || !barriers::satisfied(&self.db, id)?
            || !replacements::locally_ready(&self.db, id)?
            || !directories::ready(&self.db, id)?
        {
            return Err(JournalError::Stale);
        }
        record.state = UploadState::Verifying;
        record.attempt = Some(Uuid::new_v4());
        self.save(&record)?;
        Ok(record)
    }
    /// Called only after a successful remote commit or verified remote receipt.
    /// An uncertain receipt after restart needs a new fenced verification attempt.
    pub fn acknowledge(&mut self, id: Uuid, attempt: Uuid, remote: Node) -> Result<()> {
        let mut record = self.active_attempt(id, attempt)?;
        let identity_matches = match &record.intent {
            UploadIntent::Create { parent, name } => {
                &remote.name == name && remote.parent_id.as_deref() == Some(parent.as_str())
            }
            UploadIntent::Replace { item, .. } => &remote.id == item,
        };
        if !identity_matches
            || remote.id.is_empty()
            || remote.kind != NodeKind::File
            || remote.target.is_some()
            || remote.size != record.size
            || remote.content_revision().is_none()
        {
            return Err(JournalError::Corrupt);
        }
        record.remote = Some(remote);
        record.state = UploadState::Uploaded;
        record.transferred_bytes = record.size;
        record.attempt = None;
        self.save(&record)
    }
    /// Remove only an explicitly acknowledged payload; keep the durable receipt.
    /// Pending, failed, conflicted and uncertain edits have no deletion API here.
    pub fn prune_uploaded_payload(&mut self, id: Uuid) -> Result<()> {
        if self.get(id)?.state != UploadState::Uploaded {
            return Err(JournalError::Stale);
        }
        match std::fs::remove_file(self.objects.join(id.to_string())) {
            Ok(()) => File::open(&self.objects)?.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(JournalError::Storage),
        }
        Ok(())
    }
}
fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64)
}
