//! Genuine standalone removal acknowledgement, followed by retained-body corruption.
//! Root runs these controls; no provider, vault or account-service actions occur here.
use super::*;
use anyhow::Context;
use cirrove_core::mutation::MutationRequest;

fn applied_removal() -> anyhow::Result<(UploadJournal, MutationRecord)> {
    let root = tempfile::Builder::new()
        .prefix("native-trash-authority-")
        .tempdir()?
        .keep();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let mut journal = UploadJournal::open(&root.join("journal"), "owned", 1024 * 1024)?;
    let request = MutationRequest {
        scope: Scope {
            account: "owned".into(),
            provider: "icloud".into(),
            collection: "drive".into(),
        },
        intent: MutationIntent::TrashNativeDocument {
            before: Node {
                id: "FILE::com.apple.CloudDocs::owned-original".into(),
                parent_id: Some("FOLDER::com.apple.CloudDocs::owned-parent".into()),
                name: "Original.numbers".into(),
                kind: NodeKind::Folder,
                size: 17,
                modified_unix: 0,
                etag: Some("selected-original-revision".into()),
                content_version: None,
                target: None,
                package: true,
            },
        },
    };
    let queued = journal.enqueue_mutation(request)?;
    let claimed = journal.claim_mutation()?.context("native removal claim")?;
    assert_eq!(claimed.id, queued.id);
    journal.acknowledge_mutation(
        queued.id,
        claimed.attempt.context("native removal attempt")?,
        MutationReceipt::Removed {
            item: "FILE::com.apple.CloudDocs::owned-original".into(),
        },
    )?;
    let applied = journal.mutation(queued.id)?;
    assert_eq!(applied.state, MutationState::Applied);
    assert!(applied.attempt.is_none());
    assert!(applied.prepared_item.is_none());
    let due = journal
        .native_trash_publication_due(0)?
        .context("valid applied native removal publication")?;
    assert_eq!(serde_json::to_value(due)?, serde_json::to_value(&applied)?);
    Ok((journal, applied))
}

fn protected_rows(db: &Connection) -> anyhow::Result<Vec<Vec<String>>> {
    [
        "SELECT json_array(sequence,id,state,body) FROM mutations ORDER BY sequence",
        "SELECT json_array(sequence,id,complete) FROM write_queue ORDER BY sequence",
        "SELECT json_array(id,resource) FROM write_resources ORDER BY id,resource",
        "SELECT json_array(sequence,id,state,body) FROM uploads ORDER BY sequence",
        "SELECT json_array(operation,done,observed) FROM native_trash_metadata_publication ORDER BY operation",
    ]
    .into_iter()
    .map(|sql| {
        Ok(db
            .prepare(sql)?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .collect()
}

fn corrupt_applied_body_refused(
    journal: &UploadJournal,
    corrupted: &MutationRecord,
) -> anyhow::Result<()> {
    assert_eq!(
        journal.db.execute(
            "UPDATE mutations SET body=?2 WHERE id=?1",
            params![corrupted.id.to_string(), serde_json::to_string(corrupted)?],
        )?,
        1,
    );
    let protected = protected_rows(&journal.db)?;
    let before: (bool, i64, i64, Option<String>) = journal.db.query_row(
        "SELECT done,failures,retry_after,observed FROM native_trash_metadata_publication WHERE operation=?1",
        [corrupted.id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(before, (false, 0, 0, None));
    let changes = journal.db.total_changes();
    let result = journal.native_trash_publication_due(0);
    // Preserve the corrupted retained body, transfer/resource/queue rows and
    // publication identity/status. Only this job's bounded cooldown may change.
    assert_eq!(protected_rows(&journal.db)?, protected);
    let after: (bool, i64, i64, Option<String>) = journal.db.query_row(
        "SELECT done,failures,retry_after,observed FROM native_trash_metadata_publication WHERE operation=?1",
        [corrupted.id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(after.0, before.0);
    assert_eq!(after.3, before.3);
    if after != before {
        assert_eq!(after.1, 1);
        assert!((1..=60).contains(&after.2));
        assert_eq!(journal.db.total_changes() - changes, 1);
    } else {
        assert_eq!(journal.db.total_changes(), changes);
    }
    assert!(
        matches!(result, Err(JournalError::Corrupt)),
        "applied native removal with impossible acknowledgement authority must be refused",
    );
    Ok(())
}

#[test]
fn native_trash_due_rejects_applied_attempt_authority() -> anyhow::Result<()> {
    let (journal, mut applied) = applied_removal()?;
    applied.attempt = Some(Uuid::new_v4());
    corrupt_applied_body_refused(&journal, &applied)
}

#[test]
fn native_trash_due_rejects_applied_prepared_item_authority() -> anyhow::Result<()> {
    let (journal, mut applied) = applied_removal()?;
    applied.prepared_item = Some("FILE::com.apple.CloudDocs::foreign-item".into());
    corrupt_applied_body_refused(&journal, &applied)
}
