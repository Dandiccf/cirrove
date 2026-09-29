use super::*;
use crate::{journal::MutationState, mutations::MutationWorker};
use cirrove_core::{
    ReadProvider,
    mutation::{MutationIntent, MutationReceipt, MutationRequest},
};

/// Real namespace adapters and durable worker, confined to newly owned IDs.
pub async fn icloud_account_namespace(run: Uuid) -> Result<()> {
    let fixture = prepare(run, "namespace").await?;
    let mut known = vec![fixture.parent.clone()];
    let a = mutate(
        &fixture,
        &mut known,
        MutationIntent::CreateFolder {
            parent: fixture.parent.id.clone(),
            name: "Folder A".into(),
        },
    )
    .await?
    .context("folder receipt")?;
    let b = mutate(
        &fixture,
        &mut known,
        MutationIntent::CreateFolder {
            parent: fixture.parent.id.clone(),
            name: "Folder B".into(),
        },
    )
    .await?
    .context("folder receipt")?;
    let file = transfer(
        &fixture.state,
        &fixture.account,
        &fixture.scope,
        &fixture.snapshot,
        &fixture.parent,
        None,
        FIRST,
    )
    .await?;
    verify(
        &fixture.snapshot,
        &fixture.account,
        &fixture.parent,
        &file,
        FIRST,
    )
    .await?;
    known.push(file.clone());
    let renamed = mutate(
        &fixture,
        &mut known,
        MutationIntent::Relocate {
            before: file,
            parent: fixture.parent.id.clone(),
            name: "Renamed.txt".into(),
        },
    )
    .await?
    .context("file rename receipt")?;
    let moved = mutate(
        &fixture,
        &mut known,
        MutationIntent::Relocate {
            before: renamed.clone(),
            parent: a.id.clone(),
            name: renamed.name.clone(),
        },
    )
    .await?
    .context("file move receipt")?;
    let mut remote = ICloudReadSession::from_session_snapshot(
        &fixture.snapshot,
        &fixture.account.identity.username,
    )?;
    let digest = remote
        .hash_file_in_folder_for_revision(
            &a.id,
            &moved.id,
            moved.etag.as_deref().context("moved revision")?,
            moved.size,
        )
        .await?;
    ensure!(
        digest == hex::encode(Sha256::digest(FIRST)),
        "moved content mismatch"
    );
    mutate(
        &fixture,
        &mut known,
        MutationIntent::RemoveFile {
            before: moved.clone(),
        },
    )
    .await?;
    ensure!(
        remote.exact_item_in_trash(&moved.id).await?,
        "removed file is not recoverable"
    );
    let a = refresh(&fixture, &mut known, &a.id).await?;
    let renamed_folder = mutate(
        &fixture,
        &mut known,
        MutationIntent::Relocate {
            before: a,
            parent: fixture.parent.id.clone(),
            name: "Renamed Folder".into(),
        },
    )
    .await?
    .context("folder rename receipt")?;
    let moved_folder = mutate(
        &fixture,
        &mut known,
        MutationIntent::Relocate {
            before: renamed_folder.clone(),
            parent: b.id.clone(),
            name: renamed_folder.name.clone(),
        },
    )
    .await?
    .context("folder move receipt")?;
    mutate(
        &fixture,
        &mut known,
        MutationIntent::RemoveFolder {
            before: moved_folder,
        },
    )
    .await?;
    let b = refresh(&fixture, &mut known, &b.id).await?;
    mutate(
        &fixture,
        &mut known,
        MutationIntent::RemoveFolder { before: b },
    )
    .await?;
    ensure!(
        known.len() == 1 && known[0].id == fixture.parent.id,
        "unexpected retained namespace"
    );
    ensure!(
        remote.list_folder(&fixture.parent.id).await?.is_empty(),
        "fixture root is not empty after namespace operations"
    );
    let reopened = engine(
        &fixture.state,
        &fixture.account,
        &fixture.scope,
        &fixture.snapshot,
    )
    .await?;
    let context = WriteContext::open(&reopened, &fixture.state).await?;
    let rows = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .list_mutations(0, 16)?;
    ensure!(
        rows.len() == 9
            && rows
                .iter()
                .all(|row| row.state == MutationState::Applied && row.receipt.is_some()),
        "fresh journal did not retain all namespace receipts"
    );
    record(
        &fixture.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,"namespace_operations":9,"file_create":true,"independent_moved_digest":true,"recoverable_file_removal":true,"mounted":false}),
    )?;
    println!("Account router: nine namespace operations and independent observations verified");
    Ok(())
}

fn guard(known: &[Node], root: &Node, intent: &MutationIntent) -> Result<()> {
    if let Some(before) = intent.before() {
        ensure!(
            before.id != root.id && known.iter().any(|node| node == before),
            "mutation source is not an exact run-owned receipt"
        );
    }
    let destination = match intent {
        MutationIntent::CreateFolder { parent, .. } | MutationIntent::Relocate { parent, .. } => {
            Some(parent)
        }
        _ => None,
    };
    if let Some(parent) = destination {
        ensure!(
            known
                .iter()
                .any(|node| &node.id == parent && node.kind == NodeKind::Folder),
            "mutation destination is outside the run-owned tree"
        );
    }
    Ok(())
}

async fn mutate(
    f: &Fixture,
    known: &mut Vec<Node>,
    intent: MutationIntent,
) -> Result<Option<Node>> {
    guard(known, &f.parent, &intent)?;
    let engine = engine(&f.state, &f.account, &f.scope, &f.snapshot).await?;
    let context = WriteContext::open(&engine, &f.state).await?;
    {
        let mut store = Store::open(context.metadata_db())?;
        for node in known.iter() {
            store.observe_node(&f.scope, node)?;
        }
    }
    let request = MutationRequest {
        scope: f.scope.clone(),
        intent,
    };
    let row = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .enqueue_mutation(request.clone())?;
    let provider = Arc::new(ICloudWriteProvider::new(&f.account, &context)?);
    let worker = MutationWorker::new(context.journal(), provider, CancellationToken::new());
    let result = worker.run_once().await?.context("mutation not selected")?;
    // Preserve bounded worker diagnostics before a failed arm stops. The journal
    // intentionally stores state, not the issue returned by this worker call.
    record(
        &f.run_dir.join(format!("mutation-result-{}.json", row.id)),
        &serde_json::json!({"operation":row.id,"state":format!("{:?}", result.state),
            "issue":result.issue}),
    )?;
    ensure!(
        result.id == row.id && result.state == MutationState::Applied,
        "namespace operation did not complete; retained for inspection, no replay"
    );
    let saved = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .mutation(row.id)?;
    let receipt = saved.receipt.context("mutation receipt missing")?;
    let read = ICloudDrive::on_demand_from_session_snapshot(
        f.scope.clone(),
        &f.account.identity.username,
        &f.snapshot,
    )?;
    let cancel = CancellationToken::new();
    let current = match &receipt {
        MutationReceipt::Upsert(node) => {
            let parent = node
                .parent_id
                .as_deref()
                .context("receipt parent missing")?;
            let page = read.children(&f.scope, parent, None, &cancel).await?;
            ensure!(
                page.nodes.iter().any(|seen| seen.id == node.id
                    && seen.name == node.name
                    && seen.kind == node.kind
                    && seen.size == node.size
                    && seen.etag == node.etag),
                "independent namespace receipt mismatch"
            );
            ensure!(
                page.nodes.iter().filter(|seen| seen.id == node.id).count() == 1,
                "duplicate exact identity in destination"
            );
            if let MutationIntent::Relocate { before, parent, .. } = &request.intent {
                ensure!(
                    node.id == before.id && node.parent_id.as_ref() == Some(parent),
                    "relocation changed identity or destination"
                );
                if before.parent_id.as_ref() != Some(parent) {
                    let old_page = read
                        .children(
                            &f.scope,
                            before
                                .parent_id
                                .as_deref()
                                .context("source parent missing")?,
                            None,
                            &cancel,
                        )
                        .await?;
                    ensure!(
                        old_page.next.is_none()
                            && old_page.nodes.iter().all(|seen| seen.id != before.id),
                        "moved identity remains in source parent"
                    );
                }
            }
            known.retain(|old| old.id != node.id);
            known.push(node.clone());
            Some(node.clone())
        }
        MutationReceipt::Removed { item } => {
            let before = request.intent.before().context("removed source missing")?;
            ensure!(item == &before.id, "removed identity mismatch");
            let page = read
                .children(
                    &f.scope,
                    before
                        .parent_id
                        .as_deref()
                        .context("removed parent missing")?,
                    None,
                    &cancel,
                )
                .await?;
            ensure!(
                page.next.is_none() && page.nodes.iter().all(|node| node.id != *item),
                "removed identity remains or listing incomplete"
            );
            known.retain(|old| old.id != *item);
            None
        }
    };
    record(
        &f.run_dir.join(format!("mutation-{}.json", row.sequence)),
        &receipt,
    )?;
    println!("Account router: namespace operation independently verified");
    Ok(current)
}

async fn refresh(f: &Fixture, known: &mut [Node], id: &str) -> Result<Node> {
    let old = known
        .iter_mut()
        .find(|node| node.id == id)
        .context("refresh outside owned identities")?;
    let read = ICloudDrive::on_demand_from_session_snapshot(
        f.scope.clone(),
        &f.account.identity.username,
        &f.snapshot,
    )?;
    let page = read
        .children(
            &f.scope,
            old.parent_id.as_deref().context("owned parent missing")?,
            None,
            &CancellationToken::new(),
        )
        .await?;
    let current = page
        .nodes
        .into_iter()
        .find(|node| node.id == id)
        .context("owned identity missing")?;
    ensure!(
        current.parent_id == old.parent_id
            && current.name == old.name
            && current.kind == old.kind
            && !current.package
            && current.target.is_none(),
        "owned identity changed externally"
    );
    *old = current.clone();
    Ok(current)
}

/// Both naive orderings collide: the old name exists at the destination, and
/// the final name exists at the source. Only the original exact ID may change.
pub async fn icloud_account_combined(run: Uuid) -> Result<()> {
    let f = prepare(run, "combined").await?;
    let mut known = vec![f.parent.clone()];
    let mut destination = create_owned_folder(&f, &mut known, &f.parent.id, "Destination").await?;
    let source_blocker = create_owned_folder(&f, &mut known, &f.parent.id, "Combined.txt").await?;
    let original = transfer(
        &f.state,
        &f.account,
        &f.scope,
        &f.snapshot,
        &f.parent,
        None,
        FIRST,
    )
    .await?;
    known.push(original.clone());
    let blocker = transfer(
        &f.state,
        &f.account,
        &f.scope,
        &f.snapshot,
        &destination,
        None,
        SECOND,
    )
    .await?;
    known.push(blocker.clone());
    destination = refresh(&f, &mut known, &destination.id).await?;
    let moved = mutate(
        &f,
        &mut known,
        MutationIntent::Relocate {
            before: original.clone(),
            parent: destination.id.clone(),
            name: "Combined.txt".into(),
        },
    )
    .await?
    .context("combined file receipt")?;
    let mut remote =
        ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
    let digest = remote
        .hash_file_in_folder_for_revision(
            &destination.id,
            &moved.id,
            moved.etag.as_deref().context("combined file revision")?,
            moved.size,
        )
        .await?;
    ensure!(
        moved.id == original.id && digest == hex::encode(Sha256::digest(FIRST)),
        "combined file identity/content changed"
    );
    verify(&f.snapshot, &f.account, &destination, &blocker, SECOND).await?;
    refresh(&f, &mut known, &source_blocker.id).await?;
    record(&f.run_dir.join("combined-file-verified.json"), &moved)?;
    println!("Account router: combined file relocation passed both name collisions");

    let mut source_folder =
        create_owned_folder(&f, &mut known, &f.parent.id, "Folder Source").await?;
    let destination_blocker =
        create_owned_folder(&f, &mut known, &destination.id, "Folder Source").await?;
    let folder_source_blocker =
        create_owned_folder(&f, &mut known, &f.parent.id, "Folder Target").await?;
    let child = transfer(
        &f.state,
        &f.account,
        &f.scope,
        &f.snapshot,
        &source_folder,
        None,
        FIRST,
    )
    .await?;
    known.push(child.clone());
    source_folder = refresh(&f, &mut known, &source_folder.id).await?;
    destination = refresh(&f, &mut known, &destination.id).await?;
    let moved_folder = mutate(
        &f,
        &mut known,
        MutationIntent::Relocate {
            before: source_folder.clone(),
            parent: destination.id.clone(),
            name: "Folder Target".into(),
        },
    )
    .await?
    .context("combined folder receipt")?;
    ensure!(
        moved_folder.id == source_folder.id,
        "combined folder changed identity"
    );
    verify(&f.snapshot, &f.account, &moved_folder, &child, FIRST).await?;
    refresh(&f, &mut known, &destination_blocker.id).await?;
    refresh(&f, &mut known, &folder_source_blocker.id).await?;
    record(
        &f.run_dir.join("combined-folder-verified.json"),
        &moved_folder,
    )?;
    let reopened = engine(&f.state, &f.account, &f.scope, &f.snapshot).await?;
    let context = WriteContext::open(&reopened, &f.state).await?;
    let journal = context.journal();
    let journal = journal
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?;
    let mutations = journal.list_mutations(0, 16)?;
    let uploads = journal.list(0, 8)?;
    ensure!(
        mutations.len() == 7
            && mutations
                .iter()
                .all(|row| row.state == MutationState::Applied),
        "combined mutation receipts missing after reopen"
    );
    ensure!(
        uploads.len() == 3 && uploads.iter().all(|row| row.state == UploadState::Uploaded),
        "combined fixture uploads missing after reopen"
    );
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,"combined_file":true,"combined_populated_folder":true,"both_naive_orderings_blocked":true,"independent_digests":true,"reopened_journal":true,"mounted":false}),
    )?;
    println!("Account router: combined populated-folder relocation and journal reopening verified");
    Ok(())
}

async fn create_owned_folder(
    f: &Fixture,
    known: &mut Vec<Node>,
    parent: &str,
    name: &str,
) -> Result<Node> {
    mutate(
        f,
        known,
        MutationIntent::CreateFolder {
            parent: parent.into(),
            name: name.into(),
        },
    )
    .await?
    .context("owned folder creation receipt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_guard_rejects_foreign_ids_destinations_and_fixture_root_removal() {
        let root = Node {
            id: "root".into(),
            parent_id: Some(ROOT_ID.into()),
            name: "fixture".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("revision".into()),
            content_version: None,
            target: None,
            package: false,
        };
        let file = Node {
            id: "file".into(),
            parent_id: Some(root.id.clone()),
            name: "owned.txt".into(),
            kind: NodeKind::File,
            ..root.clone()
        };
        let known = [root.clone(), file.clone()];
        assert!(
            guard(
                &known,
                &root,
                &MutationIntent::Relocate {
                    before: file.clone(),
                    parent: root.id.clone(),
                    name: "new.txt".into()
                }
            )
            .is_ok()
        );
        assert!(
            guard(
                &known,
                &root,
                &MutationIntent::RemoveFolder {
                    before: root.clone()
                }
            )
            .is_err()
        );
        assert!(
            guard(
                &known,
                &root,
                &MutationIntent::RemoveFile {
                    before: Node {
                        id: "foreign".into(),
                        ..file.clone()
                    }
                }
            )
            .is_err()
        );
        assert!(
            guard(
                &known,
                &root,
                &MutationIntent::Relocate {
                    before: file,
                    parent: "foreign".into(),
                    name: "new.txt".into()
                }
            )
            .is_err()
        );
        assert!(
            guard(
                &known,
                &root,
                &MutationIntent::CreateFolder {
                    parent: "foreign".into(),
                    name: "new".into()
                }
            )
            .is_err()
        );
    }
}
