//! Durable local upload snapshots. This module does not enable filesystem writes.
//!
//! Call on a blocking worker. Seal bytes and fsync their directory before committing
//! the pending row. Network work happens after a claim returns, outside this module.
//! An interrupted attempt requires remote verification, never unconditional replay.
use cirrove_core::{Node, NodeKind, Scope};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;

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
    fn from(_: std::io::Error) -> Self {
        Self::Storage
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

/// Creates must fail on a name collision; replacing requires the originally seen
/// metadata ETag. Content-only tags cannot protect a concurrent rename or move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UploadIntent {
    Create { parent: String, name: String },
    Replace { item: String, expected_etag: String },
}
impl UploadIntent {
    fn validate(&self) -> Result<()> {
        let valid = |s: &str| !s.is_empty() && s.len() <= 4096 && !s.contains('\0');
        let okay = match self {
            Self::Create { parent, name } => {
                valid(parent)
                    && valid(name)
                    && !matches!(name.as_str(), "." | "..")
                    && !name.contains('/')
            }
            Self::Replace {
                item,
                expected_etag,
            } => valid(item) && valid(expected_etag) && !expected_etag.contains(['\r', '\n']),
        };
        if okay {
            Ok(())
        } else {
            Err(JournalError::Intent)
        }
    }
    fn resource(&self, scope: &Scope) -> Result<String> {
        // Used only to order edits of the same identity. The provider enforces
        // its own case/normalization and name-collision rules for new objects.
        Ok(match self {
            Self::Create { parent, name } => {
                serde_json::to_string(&(scope, "create", parent, name))?
            }
            Self::Replace { item, .. } => serde_json::to_string(&(scope, "replace", item))?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadState {
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
}
impl std::fmt::Debug for UploadRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadRecord")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

pub struct UploadJournal {
    db: Connection,
    objects: PathBuf,
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
        let database = root.join("uploads.db");
        private_file(&database)?;
        let db = Connection::open(database)?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 1 {
            return Err(JournalError::Schema);
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1), account TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS uploads (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
                resource TEXT NOT NULL, state TEXT NOT NULL, body TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS upload_order ON uploads(resource,sequence);
            CREATE INDEX IF NOT EXISTS upload_state ON uploads(state,sequence);
            PRAGMA user_version=1;")?;
        db.execute("INSERT OR IGNORE INTO identity VALUES(1,?1)", [account])?;
        let stored: String = db.query_row("SELECT account FROM identity", [], |r| r.get(0))?;
        if stored != account {
            return Err(JournalError::Account);
        }
        // Never infer that a transfer failed just because its process died.
        db.execute(
            "UPDATE uploads SET state='verify_required',
            body=json_set(body,'$.state','verify_required','$.attempt',NULL)
            WHERE state IN ('uploading','verifying')",
            [],
        )?;
        File::open(&objects)?.sync_all()?;
        File::open(root)?.sync_all()?;
        // Persist newly created ancestor entries too; syncing only the immediate
        // parent is insufficient when a whole account directory was just made.
        for parent in root.ancestors().skip(1) {
            File::open(parent)?.sync_all()?;
        }
        Ok(Self {
            db,
            objects,
            account: account.into(),
            quota,
            _owner: owner,
        })
    }
    /// Counts every spool file, including interrupted, unreferenced publications.
    /// Such bytes are retained for inspection and cannot silently bypass quota.
    pub fn retained_bytes(&self) -> Result<u64> {
        self.retained_usage().map(|(bytes, _)| bytes)
    }
    fn retained_usage(&self) -> Result<(u64, usize)> {
        let mut total = 0u64;
        let mut count = 0;
        for (index, entry) in std::fs::read_dir(&self.objects)?.enumerate() {
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
        mut bytes: impl Read,
    ) -> Result<UploadRecord> {
        if scope.account != self.account {
            return Err(JournalError::Account);
        }
        if scope.provider.is_empty() || scope.collection.is_empty() {
            return Err(JournalError::Intent);
        }
        intent.validate()?;
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
        };
        temporary
            .persist_noclobber(self.objects.join(record.id.to_string()))
            .map_err(|_| JournalError::Storage)?;
        File::open(&self.objects)?.sync_all()?;
        // Failures after publication retain an orphan; never delete possibly
        // acknowledged bytes in an error/recovery path.
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO uploads(id,resource,state,body) VALUES(?1,?2,'pending',?3)",
            params![
                record.id.to_string(),
                record.intent.resource(&record.scope)?,
                serde_json::to_string(&record)?
            ],
        )?;
        record.sequence = tx.last_insert_rowid() as u64;
        tx.execute(
            "UPDATE uploads SET body=?2 WHERE id=?1",
            params![record.id.to_string(), serde_json::to_string(&record)?],
        )?;
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
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT u.body FROM uploads u WHERE u.state='pending' AND NOT EXISTS (
                SELECT 1 FROM uploads previous WHERE previous.resource=u.resource
                AND previous.sequence<u.sequence AND previous.state!='uploaded')
             ORDER BY u.sequence LIMIT 1",
                [],
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
        let changed = self.db.execute(
            "UPDATE uploads SET state=?2,body=?3 WHERE id=?1",
            params![record.id.to_string(), state, serde_json::to_string(record)?],
        )?;
        if changed != 1 {
            return Err(JournalError::Missing);
        }
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
    /// Use VerifyRequired for an ambiguous network result. Conflict and Failed
    /// retain bytes and never silently retry or turn into upload success.
    pub fn stop_attempt(&mut self, id: Uuid, attempt: Uuid, state: UploadState) -> Result<()> {
        if !matches!(
            state,
            UploadState::VerifyRequired | UploadState::Conflict | UploadState::Failed
        ) {
            return Err(JournalError::Stale);
        }
        let mut record = self.active_attempt(id, attempt)?;
        record.state = state;
        record.attempt = None;
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
        self.save(&record)
    }
    /// Reserve an uncertain operation for read-only remote reconciliation. This
    /// does not authorize another upload. A matching remote result can be
    /// acknowledged with this new attempt token; interruption requires recheck.
    pub fn claim_verification(&mut self, id: Uuid) -> Result<UploadRecord> {
        let mut record = self.get(id)?;
        if record.state != UploadState::VerifyRequired {
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
