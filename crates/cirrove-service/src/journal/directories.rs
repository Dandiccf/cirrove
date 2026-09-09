//! Local directories and fan-out destination dependencies, separate from a
//! file's linear content predecessor. No provider call may receive a local ID.
use super::*;
use cirrove_core::mutation::{MutationIntent, MutationRequest};
use rusqlite::Transaction;

/// A directory whose name is released and whose remote removal is queued.
pub struct RemovedDirectory {
    pub object: NamespaceObject,
    pub mutation: MutationRecord,
}

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    if version < 14 {
        let tx = db.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS write_destinations(
            operation TEXT PRIMARY KEY,parent TEXT NOT NULL,predecessor TEXT,
            resolved INTEGER NOT NULL DEFAULT 0 CHECK(resolved IN(0,1)));
            CREATE INDEX IF NOT EXISTS destination_ready ON write_destinations(resolved,predecessor);",
        )?;
        tx.pragma_update(None, "user_version", 14)?;
        tx.commit()?;
    }
    db.prepare("SELECT operation,parent,predecessor,resolved FROM write_destinations LIMIT 0")?;
    Ok(())
}

pub(super) fn bind(
    tx: &Transaction<'_>,
    operation: Uuid,
    sequence: u64,
    scope: &Scope,
    parent: &str,
) -> Result<()> {
    let Some(folder) = namespace::by_local(tx, scope, parent)? else {
        // An existing provider directory needs no unresolved local binding.
        return Ok(());
    };
    if folder.node.kind != NodeKind::Folder || folder.unlinked || !folder.remote_owned {
        return Err(JournalError::Intent);
    }
    if let Some(predecessor) = folder.latest {
        let previous: Option<i64> = tx
            .query_row(
                "SELECT sequence FROM write_queue WHERE id=?1",
                [predecessor.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if !previous.is_some_and(|s| s > 0 && (s as u64) < sequence) {
            return Err(JournalError::Stale);
        }
    } else if folder.remote.is_none() {
        return Err(JournalError::Corrupt);
    }
    tx.execute(
        "INSERT INTO write_destinations(operation,parent,predecessor) VALUES(?1,?2,?3)",
        params![
            operation.to_string(),
            folder.id.to_string(),
            folder.latest.map(|id| id.to_string())
        ],
    )?;
    Ok(())
}

pub(super) fn ready(db: &Connection, operation: Uuid) -> Result<bool> {
    Ok(!db.query_row(
        "SELECT EXISTS(SELECT 1 FROM write_destinations WHERE operation=?1 AND resolved=0)",
        [operation.to_string()],
        |r| r.get::<_, bool>(0),
    )?)
}

pub(super) fn local_parent(db: &Connection, scope: &Scope, parent: &str) -> Result<String> {
    Ok(namespace::by_remote(db, scope, parent)?
        .filter(|o| o.node.kind == NodeKind::Folder && !o.unlinked)
        .map_or_else(|| parent.to_owned(), |o| o.node.id))
}

pub(super) fn localize_parent(db: &Connection, scope: &Scope, node: &mut Node) -> Result<()> {
    if let Some(parent) = &node.parent_id {
        node.parent_id = Some(local_parent(db, scope, parent)?);
    }
    Ok(())
}

pub(super) fn commit_creation(
    tx: &Transaction<'_>,
    mut object: NamespaceObject,
    mutation: &MutationRecord,
) -> Result<()> {
    let MutationIntent::CreateFolder { parent, name } = &mutation.request.intent else {
        return Err(JournalError::Intent);
    };
    if object.node.kind != NodeKind::Folder
        || object.remote.is_some()
        || object.working_file.is_some()
        || object.latest.is_some()
        || object.node.parent_id.as_ref() != Some(parent)
        || &object.node.name != name
        || object.scope != mutation.request.scope
        || mutation.base.is_some()
    {
        return Err(JournalError::Stale);
    }
    namespace::ensure_legacy_policy(tx, &object.scope)?;
    object.latest = Some(mutation.id);
    namespace::save(tx, &object)
}

impl UploadJournal {
    /// Commit a local directory and its remote creation together. Children use
    /// this stable local identity before and after the provider assigns an ID.
    pub fn create_namespace_directory(
        &mut self,
        scope: Scope,
        parent: String,
        name: String,
    ) -> Result<NamespaceObject> {
        if scope.account != self.account {
            return Err(JournalError::Account);
        }
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: parent.clone(),
                name: name.clone(),
            },
        };
        request.validate().map_err(|_| JournalError::Intent)?;
        let count: i64 = self
            .db
            .query_row("SELECT count(*) FROM namespace_objects", [], |r| r.get(0))?;
        if count >= 10_000 {
            return Err(JournalError::Quota);
        }
        let id = Uuid::new_v4();
        let object = NamespaceObject {
            id,
            names: namespace::policy(&self.db, &scope)?,
            scope,
            node: Node {
                id: format!("local-directory-{id}"),
                parent_id: Some(parent),
                name,
                kind: NodeKind::Folder,
                size: 0,
                modified_unix: now_seconds(),
                etag: None,
                content_version: None,
                target: None,
            },
            remote: None,
            remote_owned: true,
            remote_sequence: 0,
            working_file: None,
            latest: None,
            revision: 0,
            follows_remote: false,
            unlinked: false,
        };
        self.enqueue_namespace_mutation(request, None, object)?;
        self.namespace_object(id)
    }

    /// Release a directory name and queue its conditional remote removal.
    ///
    /// Never recursive, the way POSIX `rmdir` is not. Emptiness against the
    /// provider is the adapter's job, immediately before the delete; what is
    /// checked here is the half the provider cannot see -- a locally created
    /// directory or file that lives only in this journal and would be orphaned by
    /// removing its parent. Callers are expected to have refused a non-empty
    /// directory already, so reaching either check means a race, not a user error.
    pub fn remove_namespace_directory(
        &mut self,
        id: Uuid,
        revision: u64,
    ) -> Result<RemovedDirectory> {
        let mut object = self.namespace_object(id)?;
        if object.scope.account != self.account {
            return Err(JournalError::Account);
        }
        if object.unlinked || object.follows_remote || object.revision != revision {
            return Err(JournalError::Stale);
        }
        if object.node.kind != NodeKind::Folder
            || object.node.target.is_some()
            || object.working_file.is_some()
        {
            return Err(JournalError::Intent);
        }
        if self.namespace_objects()?.iter().any(|child| {
            !child.unlinked
                && child.id != object.id
                && child.scope == object.scope
                && child.node.parent_id.as_deref() == Some(object.node.id.as_str())
        }) {
            return Err(JournalError::Intent);
        }
        let before = if object.latest.is_some() {
            object.node.clone()
        } else {
            object.remote.clone().ok_or(JournalError::Stale)?
        };
        let request = MutationRequest {
            scope: object.scope.clone(),
            intent: MutationIntent::RemoveFolder { before },
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
        let mut mutation = MutationRecord {
            id: Uuid::new_v4(),
            sequence: 0,
            request,
            state: MutationState::Pending,
            attempt: None,
            receipt: None,
            retry_at: 0,
            failed_attempts: 0,
            base,
            working_file: None,
            local_ready: true,
        };
        object.unlinked = true;
        object.latest = Some(mutation.id);
        object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        mutation.sequence = super::mutations::queue_insert(
            &tx,
            mutation.id,
            super::mutations::mutation_resources(&mutation.request)?,
        )?;
        super::generations::insert_dependency(
            &tx,
            mutation.id,
            mutation.sequence,
            mutation.base.as_ref(),
        )?;
        tx.execute(
            "INSERT INTO mutations VALUES(?1,?2,'pending',?3)",
            params![
                mutation.sequence as i64,
                mutation.id.to_string(),
                serde_json::to_string(&mutation)?
            ],
        )?;
        namespace::save(&tx, &object)?;
        tx.commit()?;
        #[cfg(feature = "test-support")]
        crate::journal::durable::record("directories::remove_namespace_directory");
        Ok(RemovedDirectory { object, mutation })
    }

    pub(crate) fn following_namespace(
        &self,
        object: &NamespaceObject,
        remote: Node,
    ) -> Result<NamespaceObject> {
        let mut followed = object.followed(remote)?;
        localize_parent(&self.db, &followed.scope, &mut followed.node)?;
        Ok(followed)
    }

    pub(crate) fn provider_parent(&self, scope: &Scope, parent: &str) -> Result<String> {
        match namespace::by_local(&self.db, scope, parent)? {
            Some(object) if object.node.kind == NodeKind::Folder && !object.unlinked => {
                object.remote.map(|n| n.id).ok_or(JournalError::Stale)
            }
            Some(_) => Err(JournalError::Intent),
            None => Ok(parent.to_owned()),
        }
    }

    pub(super) fn resolve_ready_destinations(&mut self) -> Result<bool> {
        let ready = "SELECT d.operation FROM write_destinations d
            JOIN write_queue q ON q.id=d.operation LEFT JOIN write_queue p ON p.id=d.predecessor
            WHERE d.resolved=0 AND q.complete=0 AND(d.predecessor IS NULL OR p.complete=1)";
        let ids = {
            let mut query = self
                .db
                .prepare(&format!("{ready} ORDER BY q.sequence LIMIT 256"))?;
            query
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in ids {
            self.resolve_destination(Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?)?;
        }
        Ok(!self
            .db
            .query_row(&format!("SELECT EXISTS({ready})"), [], |r| {
                r.get::<_, bool>(0)
            })?)
    }

    fn resolve_destination(&mut self, id: Uuid) -> Result<()> {
        let (parent, predecessor): (String, Option<String>) = self.db.query_row(
            "SELECT parent,predecessor FROM write_destinations WHERE operation=?1 AND resolved=0",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let folder =
            self.namespace_object(Uuid::parse_str(&parent).map_err(|_| JournalError::Corrupt)?)?;
        let mut operation = self.operation(id)?;
        let remote = match predecessor {
            Some(previous) => {
                let bound: bool = self.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM namespace_operations WHERE operation=?1 AND object=?2)",
                    params![previous, parent], |r| r.get(0),
                )?;
                let previous = self.operation(Uuid::parse_str(&previous).map_err(|_| JournalError::Corrupt)?)?;
                if bound && previous.scope() == operation.scope() && previous.sequence() < operation.sequence() {
                    previous.confirmed_node()
                } else {
                    None
                }
            },
            None => folder.remote.clone(),
        }
        .filter(|n| n.kind == NodeKind::Folder && n.target.is_none() && !n.id.is_empty());
        if folder.scope != *operation.scope() || folder.node.kind != NodeKind::Folder {
            return Err(JournalError::Corrupt);
        }
        let tx = self.db.transaction()?;
        match &mut operation {
            generations::Operation::Upload(r) => {
                let UploadIntent::Create { parent, .. } = &mut r.intent else {
                    return Err(JournalError::Corrupt);
                };
                if let Some(remote) = &remote {
                    *parent = remote.id.clone();
                } else {
                    r.state = UploadState::Failed;
                }
                for resource in mutations::upload_resources(&r.scope, &r.intent)? {
                    tx.execute(
                        "INSERT OR IGNORE INTO write_resources VALUES(?1,?2)",
                        params![id.to_string(), resource],
                    )?;
                }
                tx.execute(
                    "UPDATE uploads SET resource=?2,state=?3,body=?4 WHERE id=?1",
                    params![
                        id.to_string(),
                        resource(&r.intent, &r.scope)?,
                        serde_json::to_value(r.state)?.as_str(),
                        serde_json::to_string(r)?
                    ],
                )?;
            }
            generations::Operation::Mutation(r) => {
                let parent = match &mut r.request.intent {
                    MutationIntent::CreateFolder { parent, .. }
                    | MutationIntent::Relocate { parent, .. } => parent,
                    _ => return Err(JournalError::Corrupt),
                };
                if let Some(remote) = &remote {
                    *parent = remote.id.clone();
                } else {
                    r.state = MutationState::Failed;
                }
                for resource in mutations::mutation_resources(&r.request)? {
                    tx.execute(
                        "INSERT OR IGNORE INTO write_resources VALUES(?1,?2)",
                        params![id.to_string(), resource],
                    )?;
                }
                tx.execute(
                    "UPDATE mutations SET state=?2,body=?3 WHERE id=?1",
                    params![
                        id.to_string(),
                        serde_json::to_value(r.state)?.as_str(),
                        serde_json::to_string(r)?
                    ],
                )?;
            }
        }
        tx.execute(
            "UPDATE write_destinations SET resolved=1 WHERE operation=?1",
            [id.to_string()],
        )?;
        tx.commit()?;
        #[cfg(feature = "test-support")]
        crate::journal::durable::record("directories::resolve_destination");
        Ok(())
    }
}
