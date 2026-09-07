//! A later save follows its predecessor's validated receipt, never its old ETag.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploadBase {
    pub predecessor: Uuid,
    pub resolved: bool,
}

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS upload_successor
        ON uploads(json_extract(body,'$.base.predecessor'))
        WHERE json_type(body,'$.base.predecessor')='text';",
    )?;
    tx.pragma_update(None, "user_version", version.max(4))?;
    tx.commit()?;
    Ok(())
}

impl UploadJournal {
    /// Seal a newer complete save while its predecessor may still be uploading.
    /// The chain is linear. A conflict retains and blocks its descendants; only a
    /// validated predecessor receipt can supply their remote identity and ETag.
    pub fn enqueue_after(&mut self, predecessor: Uuid, bytes: impl Read) -> Result<UploadRecord> {
        let previous = self.get(predecessor)?;
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM uploads WHERE json_extract(body,'$.base.predecessor')=?1)",
            [predecessor.to_string()],
            |r| r.get(0),
        )?;
        if exists {
            return Err(JournalError::Stale);
        }
        self.enqueue_generation(
            previous.scope,
            previous.intent,
            Some(UploadBase {
                predecessor,
                resolved: false,
            }),
            None,
            bytes,
        )
    }

    pub(super) fn resolve_ready_generations(&mut self) -> Result<bool> {
        // Bounded preparation. Unprepared rows remain ineligible until another
        // worker pass; independent ready uploads remain selectable.
        let ids = {
            let mut query = self.db.prepare(
                "SELECT u.id FROM uploads u JOIN uploads p
                ON p.id=json_extract(u.body,'$.base.predecessor')
                WHERE u.state='pending' AND p.state='uploaded'
                AND json_extract(u.body,'$.base.resolved')=0 ORDER BY u.sequence LIMIT 256",
            )?;
            query
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in ids {
            let mut record = self.get(Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?)?;
            let base = record.base.as_ref().ok_or(JournalError::Corrupt)?;
            let previous = self.get(base.predecessor)?;
            let remote = previous.remote.as_ref();
            let intent = remote.and_then(|node| {
                Some(UploadIntent::Replace {
                    item: node.id.clone(),
                    expected_etag: node.etag.clone()?,
                })
            });
            let valid = previous.state == UploadState::Uploaded
                && previous.scope == record.scope
                && previous.sequence < record.sequence
                && remote.is_some_and(|n| n.kind == NodeKind::File && n.target.is_none())
                && intent.as_ref().is_some_and(|i| i.validate().is_ok());
            if !valid {
                record.state = UploadState::Failed;
                self.save(&record)?;
                continue;
            }
            record.intent = intent.ok_or(JournalError::Corrupt)?;
            record.base.as_mut().ok_or(JournalError::Corrupt)?.resolved = true;
            let tx = self
                .db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // Retain the old name/identity exclusions and add the new remote
            // identity before ordinary eligibility checks can select this row.
            for key in mutations::upload_resources(&record.scope, &record.intent)? {
                tx.execute(
                    "INSERT OR IGNORE INTO write_resources(id,resource) VALUES(?1,?2)",
                    params![id, key],
                )?;
            }
            tx.execute(
                "UPDATE uploads SET resource=?2,body=?3 WHERE id=?1",
                params![
                    id,
                    resource(&record.intent, &record.scope)?,
                    serde_json::to_string(&record)?
                ],
            )?;
            tx.commit()?;
        }
        let waiting: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM uploads u JOIN uploads p
            ON p.id=json_extract(u.body,'$.base.predecessor')
            WHERE u.state='pending' AND p.state='uploaded'
            AND json_extract(u.body,'$.base.resolved')=0)",
            [],
            |r| r.get(0),
        )?;
        // Until every ready identity has its exclusions, another operation could
        // otherwise overtake an older save under its newly assigned remote ID.
        Ok(!waiting)
    }
}
