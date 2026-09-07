//! Sparse durable local namespace. Objects, directory entries, remote bindings
//! and optional working bytes are separate; metadata edits never allocate bytes.
use super::*;
use cirrove_core::mutation::{MutationIntent, MutationReceipt, MutationRequest};
use rusqlite::Transaction;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamespaceNames {
    Sensitive,
    Insensitive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NamespaceObject {
    pub id: Uuid,
    pub scope: Scope,
    pub names: NamespaceNames,
    /// Stable local node identity and desired presentation; never replaced with
    /// a newly assigned cloud ID merely because an upload completed.
    pub node: Node,
    pub remote: Option<Node>,
    /// Historical metadata survives after another local object takes this cloud
    /// identity. Only the active owner can hide/project incoming provider items.
    #[serde(default = "owns_remote")]
    pub remote_owned: bool,
    pub remote_sequence: u64,
    pub working_file: Option<Uuid>,
    pub latest: Option<Uuid>,
    pub revision: u64,
    /// The local identity remains an alias, but remote metadata owns the entry.
    /// Only an idle, fully acknowledged object can enter this state.
    #[serde(default)]
    pub follows_remote: bool,
    /// No directory entry; identity and any open stream survive deletion.
    #[serde(default)]
    pub unlinked: bool,
}
fn owns_remote() -> bool {
    true
}
impl NamespaceObject {
    pub(crate) fn followed(&self, remote: Node) -> Result<Self> {
        if self.unlinked
            || !self.remote_owned
            || self.remote.as_ref().is_none_or(|r| r.id != remote.id)
            || remote.kind != self.node.kind
            || !matches!(remote.kind, NodeKind::File | NodeKind::Folder)
            || remote.target.is_some()
        {
            return Err(JournalError::Stale);
        }
        MutationRequest {
            scope: self.scope.clone(),
            intent: MutationIntent::Relocate {
                parent: remote.parent_id.clone().ok_or(JournalError::Intent)?,
                name: remote.name.clone(),
                before: remote.clone(),
            },
        }
        .validate()
        .map_err(|_| JournalError::Intent)?;
        let mut object = self.clone();
        object.node = remote.clone();
        object.node.id = self.node.id.clone();
        object.remote = Some(remote);
        object.working_file = None;
        object.latest = None;
        object.follows_remote = true;
        object.revision = self.revision.checked_add(1).ok_or(JournalError::Quota)?;
        Ok(object)
    }
}

#[derive(Clone, Debug)]
pub struct NamespaceCollision {
    pub local: Uuid,
    pub remote: Node,
}
#[derive(Clone, Debug)]
#[must_use]
pub struct NamespaceListing {
    pub nodes: Vec<Node>,
    /// Remote occupants are retained explicitly, not represented as duplicate
    /// Linux pathnames or silently mistaken for the pending local replacement.
    pub conflicts: Vec<NamespaceCollision>,
}

fn scope_key(scope: &Scope) -> Result<String> {
    Ok(serde_json::to_string(scope)?)
}
fn identity(scope: &Scope, item: &str) -> Result<String> {
    Ok(serde_json::to_string(&(scope, item))?)
}
pub(super) fn by_id(db: &Connection, id: Uuid) -> Result<NamespaceObject> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM namespace_objects WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(serde_json::from_str(&body.ok_or(JournalError::Missing)?)?)
}
pub(super) fn by_local(
    db: &Connection,
    scope: &Scope,
    item: &str,
) -> Result<Option<NamespaceObject>> {
    let key = identity(scope, item)?;
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM namespace_objects WHERE identity=?1",
            [key],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|b| Ok(serde_json::from_str(&b)?)).transpose()
}
pub(super) fn by_remote(
    db: &Connection,
    scope: &Scope,
    item: &str,
) -> Result<Option<NamespaceObject>> {
    let body: Option<String> = db
        .query_row(
            "SELECT o.body FROM namespace_remote r
            JOIN namespace_objects o ON o.id=r.object WHERE r.identity=?1",
            [identity(scope, item)?],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|b| Ok(serde_json::from_str(&b)?)).transpose()
}
pub(super) fn policy(db: &Connection, scope: &Scope) -> Result<NamespaceNames> {
    let value: Option<String> = db
        .query_row(
            "SELECT names FROM namespace_scopes WHERE scope=?1",
            [scope_key(scope)?],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(value) = value {
        return Ok(serde_json::from_str(&value)?);
    }
    let populated: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM namespace_objects WHERE scope=?1)",
        [scope_key(scope)?],
        |r| r.get(0),
    )?;
    if populated {
        return Err(JournalError::Corrupt);
    }
    Ok(NamespaceNames::Insensitive)
}
pub(super) fn ensure_legacy_policy(tx: &Transaction<'_>, scope: &Scope) -> Result<()> {
    // Preserve the original experimental working-file API's name semantics.
    // New adapters can select their policy before creating any local objects.
    tx.execute(
        "INSERT OR IGNORE INTO namespace_scopes VALUES(?1,?2)",
        params![
            scope_key(scope)?,
            serde_json::to_string(&NamespaceNames::Insensitive)?
        ],
    )?;
    Ok(())
}
pub(super) fn entry_slot(
    db: &Connection,
    scope: &Scope,
    parent: &str,
    name: &str,
) -> Result<String> {
    let name = match policy(db, scope)? {
        NamespaceNames::Sensitive => name.to_owned(),
        NamespaceNames::Insensitive => name.to_lowercase(),
    };
    Ok(serde_json::to_string(&(scope, parent, name))?)
}
pub(super) fn save(tx: &Transaction<'_>, object: &NamespaceObject) -> Result<()> {
    ancestry::check_new_slot(tx, object)?;
    if !object.remote_owned && !object.unlinked {
        return Err(JournalError::Corrupt);
    }

    if object.follows_remote
        && (object.unlinked
            || object.working_file.is_some()
            || object.latest.is_some()
            || object.remote.is_none())
    {
        return Err(JournalError::Corrupt);
    }
    if object.names != policy(tx, &object.scope)? {
        return Err(JournalError::Corrupt);
    }
    let parent = object
        .node
        .parent_id
        .as_deref()
        .ok_or(JournalError::Intent)?;
    let slot = entry_slot(tx, &object.scope, parent, &object.node.name)?;
    let occupied: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM namespace_entries WHERE slot=?1 AND object!=?2)",
        params![slot, object.id.to_string()],
        |r| r.get(0),
    )?;
    if occupied && !object.follows_remote && !object.unlinked {
        return Err(JournalError::Stale);
    }
    tx.execute(
        "INSERT INTO namespace_objects(id,identity,scope,working,body) VALUES(?1,?2,?3,?4,?5)
        ON CONFLICT(id) DO UPDATE SET working=excluded.working,body=excluded.body",
        params![
            object.id.to_string(),
            identity(&object.scope, &object.node.id)?,
            scope_key(&object.scope)?,
            object.working_file.map(|v| v.to_string()),
            serde_json::to_string(object)?
        ],
    )?;
    tx.execute(
        "DELETE FROM namespace_entries WHERE object=?1",
        [object.id.to_string()],
    )?;
    if !object.follows_remote && !object.unlinked {
        tx.execute(
            "INSERT INTO namespace_entries(slot,object,scope,parent) VALUES(?1,?2,?3,?4)",
            params![
                slot,
                object.id.to_string(),
                scope_key(&object.scope)?,
                parent
            ],
        )?;
    }
    tx.execute(
        "DELETE FROM namespace_remote WHERE object=?1",
        [object.id.to_string()],
    )?;
    if object.remote_owned
        && let Some(remote) = &object.remote
    {
        let key = identity(&object.scope, &remote.id)?;
        let other: Option<String> = tx
            .query_row(
                "SELECT object FROM namespace_remote WHERE identity=?1 AND object!=?2 LIMIT 1",
                params![&key, object.id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if other.is_some_and(|id| id != object.id.to_string()) {
            return Err(JournalError::Stale);
        }
        tx.execute(
            "INSERT OR IGNORE INTO namespace_remote VALUES(?1,?2)",
            params![key, object.id.to_string()],
        )?;
    }
    if let Some(latest) = object.latest {
        let other: Option<String> = tx
            .query_row(
                "SELECT object FROM namespace_operations WHERE operation=?1",
                [latest.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if other.is_some_and(|id| id != object.id.to_string()) {
            return Err(JournalError::Stale);
        }
        tx.execute(
            "INSERT OR IGNORE INTO namespace_operations VALUES(?1,?2)",
            params![latest.to_string(), object.id.to_string()],
        )?;
    }
    Ok(())
}

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    if version >= 7 {
        db.prepare("SELECT o.id,e.slot,r.identity,p.operation,s.names FROM namespace_objects o
            LEFT JOIN namespace_entries e ON e.object=o.id LEFT JOIN namespace_remote r ON r.object=o.id
            LEFT JOIN namespace_operations p ON p.object=o.id JOIN namespace_scopes s ON s.scope=o.scope LIMIT 0")?;
        return Ok(());
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE namespace_scopes(scope TEXT PRIMARY KEY,names TEXT NOT NULL);
        CREATE TABLE namespace_objects(id TEXT PRIMARY KEY,identity TEXT NOT NULL UNIQUE,scope TEXT NOT NULL,
            working TEXT UNIQUE,body TEXT NOT NULL);
        CREATE INDEX namespace_scope ON namespace_objects(scope,id);
        CREATE TABLE namespace_entries(slot TEXT PRIMARY KEY,object TEXT NOT NULL UNIQUE,scope TEXT NOT NULL,parent TEXT NOT NULL);
        CREATE INDEX namespace_parent ON namespace_entries(scope,parent);
        CREATE TABLE namespace_remote(identity TEXT PRIMARY KEY,object TEXT NOT NULL);
        CREATE TABLE namespace_operations(operation TEXT PRIMARY KEY,object TEXT NOT NULL);")?;
    {
        let mut query = tx.prepare("SELECT body FROM working_files ORDER BY id")?;
        let rows = query.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            let mut working: WorkingFile = serde_json::from_str(&row?)?;
            attach_working(&tx, &mut working)?;
        }
    }
    // Earlier completed generations may no longer be the head. Preserve their
    // remote IDs too, so a cloud listing cannot duplicate a local created object.
    {
        let mut query =
            tx.prepare("SELECT body FROM uploads WHERE state='uploaded' ORDER BY sequence")?;
        let rows = query.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            let upload: UploadRecord = serde_json::from_str(&row?)?;
            if let (Some(working), Some(remote)) = (upload.working_file, upload.remote) {
                bind_legacy_operation(&tx, working, upload.id)?;
                confirm(&tx, upload.id, upload.sequence, &remote)?;
            }
        }
    }
    {
        let mut query =
            tx.prepare("SELECT body FROM mutations WHERE state='applied' ORDER BY sequence")?;
        let rows = query.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            let mutation: MutationRecord = serde_json::from_str(&row?)?;
            if let (Some(working), Some(MutationReceipt::Upsert(remote))) =
                (mutation.working_file, mutation.receipt)
            {
                bind_legacy_operation(&tx, working, mutation.id)?;
                confirm(&tx, mutation.id, mutation.sequence, &remote)?;
            }
        }
    }
    tx.pragma_update(None, "user_version", 7)?;
    tx.commit()?;
    Ok(())
}
fn bind_legacy_operation(tx: &Transaction<'_>, working: Uuid, operation: Uuid) -> Result<()> {
    tx.execute("INSERT OR IGNORE INTO namespace_operations SELECT ?1,id FROM namespace_objects WHERE working=?2",
        params![operation.to_string(), working.to_string()])?;
    Ok(())
}

pub(super) fn prepare_attachment(
    db: &Connection,
    working: &mut WorkingFile,
) -> Result<NamespaceObject> {
    // Hydration identifies its provider source explicitly. A mutable cloud
    // binding must never be used to resolve an already local working stream.
    let existing = match &working.initial_remote {
        Some(remote) => by_remote(db, &working.scope, &remote.id)?,
        None => by_local(db, &working.scope, &working.node.id)?,
    };
    Ok(match existing {
        Some(mut object) => {
            if object.unlinked != working.unlinked || object.working_file.is_some() {
                return Err(JournalError::Stale);
            }
            if object.follows_remote {
                let source = working.initial_remote.as_ref().ok_or(JournalError::Stale)?;
                if object.latest.is_some()
                    || object.remote.as_ref().is_none_or(|r| r.id != source.id)
                    || source.kind != object.node.kind
                    || source.target.is_some()
                {
                    return Err(JournalError::Stale);
                }
                object.remote = Some(source.clone());
                object.node.name = source.name.clone();
                object.node.parent_id = source.parent_id.clone();
                directories::localize_parent(db, &object.scope, &mut object.node)?;
                object.follows_remote = false;
            }
            if object.node.kind != working.node.kind
                || !object
                    .remote
                    .as_ref()
                    .zip(working.initial_remote.as_ref())
                    .is_some_and(|(current, source)| {
                        current.size == source.size
                            && current.content_revision() == source.content_revision()
                    })
            {
                return Err(JournalError::Stale);
            }
            working.node.id = object.node.id.clone();
            working.node.name = object.node.name.clone();
            working.node.parent_id = object.node.parent_id.clone();
            working.latest = object.latest;
            working.initial_remote = object.remote.clone();
            object
        }
        None => {
            if working.initial_remote.is_some() {
                directories::localize_parent(db, &working.scope, &mut working.node)?;
            }
            let count: i64 =
                db.query_row("SELECT count(*) FROM namespace_objects", [], |r| r.get(0))?;
            if count >= 10_000 {
                return Err(JournalError::Quota);
            }
            NamespaceObject {
                id: working.id,
                scope: working.scope.clone(),
                names: policy(db, &working.scope)?,
                node: working.node.clone(),
                remote: working.initial_remote.clone(),
                remote_owned: true,
                remote_sequence: 0,
                working_file: None,
                latest: working.latest,
                revision: 0,
                follows_remote: false,
                unlinked: false,
            }
        }
    })
}
pub(super) fn attach_working(tx: &Transaction<'_>, working: &mut WorkingFile) -> Result<()> {
    ensure_legacy_policy(tx, &working.scope)?;
    let mut object = prepare_attachment(tx, working)?;
    object.node = working.node.clone();
    object.working_file = Some(working.id);
    object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
    save(tx, &object)
}
pub(super) fn update_working(tx: &Transaction<'_>, working: &WorkingFile) -> Result<()> {
    let id: String = tx.query_row(
        "SELECT id FROM namespace_objects WHERE working=?1",
        [working.id.to_string()],
        |r| r.get(0),
    )?;
    let mut object = by_id(tx, Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?)?;
    if object.scope != working.scope
        || object.node.id != working.node.id
        || object.unlinked != working.unlinked
    {
        return Err(JournalError::Stale);
    }
    object.node = working.node.clone();
    object.latest = working.latest;
    object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
    save(tx, &object)
}
pub(super) fn confirm(
    tx: &Transaction<'_>,
    operation: Uuid,
    sequence: u64,
    remote: &Node,
) -> Result<()> {
    let id: Option<String> = tx
        .query_row(
            "SELECT object FROM namespace_operations WHERE operation=?1",
            [operation.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else {
        return Ok(());
    };
    let mut object = by_id(tx, Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?)?;
    if sequence <= object.remote_sequence {
        return Ok(());
    }
    if object
        .remote
        .as_ref()
        .is_some_and(|old| old.id != remote.id)
        || remote.kind != object.node.kind
        || remote.target.is_some()
    {
        return Err(JournalError::Corrupt);
    }
    object.remote = Some(remote.clone());
    object.remote_sequence = sequence;
    if object.working_file.is_none() {
        object.node.etag = remote.etag.clone();
        object.node.content_version = remote.content_version.clone();
        object.node.size = remote.size;
        object.node.modified_unix = remote.modified_unix;
    }
    object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
    save(tx, &object)
}

impl UploadJournal {
    /// Select a collection's local name comparison before its first local edit.
    /// Switching a populated namespace's rules would change existing identities.
    pub fn configure_namespace(&mut self, scope: &Scope, names: NamespaceNames) -> Result<()> {
        if scope.account != self.account || scope.provider.is_empty() || scope.collection.is_empty()
        {
            return Err(JournalError::Account);
        }
        let existing: Option<String> = self
            .db
            .query_row(
                "SELECT names FROM namespace_scopes WHERE scope=?1",
                [scope_key(scope)?],
                |r| r.get(0),
            )
            .optional()?;
        let names = serde_json::to_string(&names)?;
        if existing.is_some_and(|old| old != names) {
            return Err(JournalError::Stale);
        }
        self.db.execute(
            "INSERT OR IGNORE INTO namespace_scopes VALUES(?1,?2)",
            params![scope_key(scope)?, names],
        )?;
        Ok(())
    }
    pub fn namespace_object(&self, id: Uuid) -> Result<NamespaceObject> {
        by_id(&self.db, id)
    }
    /// Resolve a stable local presentation identity. Provider aliases are never
    /// followed here: existing views and streams keep their own local object.
    pub fn namespace_by_local(&self, scope: &Scope, item: &str) -> Result<Option<NamespaceObject>> {
        by_local(&self.db, scope, item)
    }
    /// Resolve the current owner of a provider identity, for incoming metadata
    /// and hydration. Callers with a local view must use namespace_by_local.
    pub fn namespace_by_remote(
        &self,
        scope: &Scope,
        item: &str,
    ) -> Result<Option<NamespaceObject>> {
        by_remote(&self.db, scope, item)
    }
    /// Establish local identity for a file without reading or reserving its bytes.
    /// The caller obtains version-checked metadata, outside the journal lock.
    pub fn observe_namespace_file(&mut self, scope: Scope, node: Node) -> Result<NamespaceObject> {
        if scope.account != self.account {
            return Err(JournalError::Account);
        }
        if node.kind != NodeKind::File {
            return Err(JournalError::Intent);
        }
        MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::RemoveFile {
                before: node.clone(),
            },
        }
        .validate()
        .map_err(|_| JournalError::Intent)?;
        if let Some(mut object) = by_remote(&self.db, &scope, &node.id)? {
            if object.unlinked {
                return Err(JournalError::Stale);
            }
            if object.follows_remote {
                if object.working_file.is_some()
                    || object.latest.is_some()
                    || object.remote.as_ref().is_none_or(|r| r.id != node.id)
                {
                    return Err(JournalError::Stale);
                }
                let local = object.node.id.clone();
                object.node = node.clone();
                object.node.id = local;
                directories::localize_parent(&self.db, &scope, &mut object.node)?;
                object.remote = Some(node);
                object.follows_remote = false;
                object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
                let tx = self
                    .db
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                save(&tx, &object)?;
                tx.commit()?;
            }
            return Ok(object);
        }
        let count: i64 = self
            .db
            .query_row("SELECT count(*) FROM namespace_objects", [], |r| r.get(0))?;
        if count >= 10_000 {
            return Err(JournalError::Quota);
        }
        let mut object = NamespaceObject {
            id: Uuid::new_v4(),
            names: policy(&self.db, &scope)?,
            scope,
            node: node.clone(),
            remote: Some(node),
            remote_owned: true,
            remote_sequence: 0,
            working_file: None,
            latest: None,
            revision: 0,
            follows_remote: false,
            unlinked: false,
        };
        directories::localize_parent(&self.db, &object.scope, &mut object.node)?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_legacy_policy(&tx, &object.scope)?;
        save(&tx, &object)?;
        tx.commit()?;
        Ok(object)
    }
    /// A metadata-only relocation, or a relocation of the same object's working
    /// bytes. The caller checks the remote destination listing outside this lock;
    /// local entries and the eventual conditional provider call refuse collisions.
    pub fn relocate_namespace_file(
        &mut self,
        id: Uuid,
        revision: u64,
        parent: String,
        name: String,
    ) -> Result<MutationRecord> {
        let mut object = self.namespace_object(id)?;
        if object.revision != revision || object.follows_remote || object.unlinked {
            return Err(JournalError::Stale);
        }
        if object.node.kind != NodeKind::File {
            return Err(JournalError::Intent);
        }
        if let Some(working) = object.working_file {
            return self.relocate_working(working, parent, name);
        }
        let request = MutationRequest {
            scope: object.scope.clone(),
            intent: MutationIntent::Relocate {
                before: object.remote.clone().ok_or(JournalError::Corrupt)?,
                parent: parent.clone(),
                name: name.clone(),
            },
        };
        let base = if let Some(predecessor) = object.latest {
            self.validate_mutation_base(predecessor, &request)?;
            self.ensure_successor_free(predecessor)?;
            Some(WriteBase {
                predecessor,
                resolved: false,
            })
        } else {
            request.validate().map_err(|_| JournalError::Intent)?;
            None
        };
        object.node.parent_id = Some(parent);
        object.node.name = name;
        object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
        self.enqueue_namespace_mutation(request, base, object)
    }
    pub fn namespace_objects(&self) -> Result<Vec<NamespaceObject>> {
        let mut query = self
            .db
            .prepare("SELECT body FROM namespace_objects ORDER BY id")?;
        query
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }
    pub fn namespace_for_operation(&self, operation: Uuid) -> Result<Option<NamespaceObject>> {
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT o.body FROM namespace_operations p
            JOIN namespace_objects o ON o.id=p.object WHERE p.operation=?1",
                [operation.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        body.map(|b| Ok(serde_json::from_str(&b)?)).transpose()
    }
    pub fn namespace_overlay(
        &self,
        scope: &Scope,
        parent: &str,
        remote: Vec<Node>,
    ) -> Result<NamespaceListing> {
        let objects = self.namespace_objects()?;
        project_namespace(objects.iter(), scope, parent, remote)
    }
}

pub(super) fn commit_relocation(
    tx: &Transaction<'_>,
    mut object: NamespaceObject,
    mutation: &MutationRecord,
) -> Result<()> {
    let old = by_id(tx, object.id)?;
    if old.working_file.is_some()
        || old.latest != object.latest
        || old.scope != object.scope
        || old.node.id != object.node.id
        || old.revision.checked_add(1) != Some(object.revision)
        || mutation.base.as_ref().map(|b| b.predecessor) != old.latest
        || mutation.request.scope != old.scope
    {
        return Err(JournalError::Stale);
    }
    let MutationIntent::Relocate {
        before,
        parent,
        name,
    } = &mutation.request.intent
    else {
        return Err(JournalError::Intent);
    };
    if old.remote.as_ref() != Some(before)
        || object.node.parent_id.as_ref() != Some(parent)
        || object.node.name != *name
    {
        return Err(JournalError::Stale);
    }
    object.latest = Some(mutation.id);
    save(tx, &object)
}

/// The database view and the mount's in-memory view use the same projection.
/// No I/O is performed, so cached navigation need not wait for spool writes.
pub fn project_namespace<'a>(
    objects: impl IntoIterator<Item = &'a NamespaceObject>,
    scope: &Scope,
    parent: &str,
    remote: Vec<Node>,
) -> Result<NamespaceListing> {
    let objects = objects.into_iter().collect::<Vec<_>>();
    let retained = RetainedAncestors::new(objects.iter().copied());
    project_retained_namespace(objects, &retained, scope, parent, remote)
}

pub(crate) fn project_retained_namespace<'a>(
    objects: impl IntoIterator<Item = &'a NamespaceObject>,
    retained: &RetainedAncestors,
    scope: &Scope,
    parent: &str,
    remote: Vec<Node>,
) -> Result<NamespaceListing> {
    use std::collections::{HashMap, HashSet};
    let objects = objects
        .into_iter()
        .filter(|o| o.scope == *scope)
        .collect::<Vec<_>>();
    let names = objects
        .first()
        .map_or(NamespaceNames::Insensitive, |o| o.names);
    if objects.iter().any(|o| o.names != names) {
        return Err(JournalError::Corrupt);
    }
    let key = |name: &str| match names {
        NamespaceNames::Sensitive => name.to_owned(),
        NamespaceNames::Insensitive => name.to_lowercase(),
    };
    let mut hidden = HashSet::new();
    let mut local = HashMap::new();
    let mut aliases = HashMap::new();
    for object in &objects {
        if object.follows_remote && !retained.contains(object.id) {
            if !object.remote_owned {
                return Err(JournalError::Corrupt);
            }
            let remote = object.remote.as_ref().ok_or(JournalError::Corrupt)?;
            aliases.insert(remote.id.as_str(), object.node.id.as_str());
            continue;
        }
        if object.remote_owned
            && let Some(remote) = &object.remote
        {
            hidden.insert(remote.id.as_str());
        }
        if !object.unlinked && object.node.parent_id.as_deref() == Some(parent) {
            local.insert(key(&object.node.name), *object);
        }
    }
    let mut nodes = vec![];
    let mut conflicts = vec![];
    for mut node in remote {
        if hidden.contains(node.id.as_str()) {
            continue;
        }
        if let Some(object) = local.get(&key(&node.name)) {
            conflicts.push(NamespaceCollision {
                local: object.id,
                remote: node,
            });
        } else {
            if let Some(identity) = aliases.get(node.id.as_str()) {
                node.id = (*identity).to_owned();
            }
            node.parent_id = Some(parent.to_owned());
            nodes.push(node);
        }
    }
    let mut entries = local.values().map(|o| o.node.clone()).collect::<Vec<_>>();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    nodes.extend(entries);
    Ok(NamespaceListing { nodes, conflicts })
}
