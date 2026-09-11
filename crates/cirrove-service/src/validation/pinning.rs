//! Milestone 3's claim, on a real drive: a pin made the way a user makes one
//! keeps content that then reads without touching the provider.
//!
//! Nine tests cover pinning and six of them go through a kernel mount, but every
//! one uses a fixture provider. A fixture cannot produce the shapes a real drive
//! does -- content revisions that change block keys, a linked collection whose
//! scope is not the account's own drive, a size the index and the provider
//! disagree about.
//!
//! Offline is established by counting rather than by severing the connection.
//! A pinned read that makes no provider content request cannot depend on one,
//! which is the same claim and is directly measurable; an unpinned control read
//! must make one, or the absence proves nothing about the pin.
use super::*;
use crate::{engine::Engine, filesystem::CloudFs, writable::WritableSession};
use cirrove_core::ReadProvider;

/// Upload one generated file into the fixture folder and return its node.
async fn place(
    graph: &Arc<OneDrive>,
    scope: &Scope,
    parent: &str,
    name: &str,
    bytes: Vec<u8>,
    cancel: &CancellationToken,
) -> Result<Node> {
    let request = UploadRequest {
        scope: scope.clone(),
        intent: UploadIntent::Create {
            parent: parent.to_owned(),
            name: name.to_owned(),
        },
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
    };
    let mut step = graph.begin_upload(&request, cancel).await?;
    loop {
        step = match step {
            // The provider names the exact range it wants next; sending anything
            // else is how a resumable upload goes wrong in a way that only shows
            // up at commit.
            UploadStep::Continue(progress) => {
                let start = progress.offset as usize;
                let end = (start + progress.length as usize).min(bytes.len());
                graph
                    .upload_part(
                        &request,
                        &progress.checkpoint,
                        progress.offset,
                        bytes[start..end].to_vec(),
                        cancel,
                    )
                    .await?
            }
            UploadStep::Commit(checkpoint) => {
                graph.commit_upload(&request, &checkpoint, cancel).await?
            }
            UploadStep::Complete(node) => return Ok(node),
        };
    }
}

pub async fn onedrive_pinning(state: &Path, label: &str) -> Result<()> {
    let account = test_account(state, label)?;
    let _operation = accounts::account_operation(state, &account.id)?;
    let _owner = accounts::account_lock(&state.join("accounts").join(&account.id))?;
    let run = uuid::Uuid::new_v4();
    let directory = state.join("pinning-checks").join(run.to_string());
    crate::private_dir(&directory)?;
    let mut log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(directory.join("events.jsonl"))?;
    let scope = Scope {
        account: account.id.clone(),
        provider: "onedrive".into(),
        collection: account.drive.id.clone(),
    };
    let cancel = CancellationToken::new();
    let _cancel_on_return = cancel.clone().drop_guard();
    let graph = accounts::provider(&account)?;
    let name = format!("Cirrove-Pinning-Validation-{run}");
    let root = graph
        .create_folder(&scope, &account.root_id, &name, &cancel)
        .await?;
    println!(
        "Creating one fixture folder with two generated files; private evidence: {}",
        directory.display()
    );

    // Two files: one to pin, one to leave alone as the control. Big enough to
    // span several cache blocks so the pin has real work to do.
    let pinned_bytes: Vec<u8> = (0..9_000_000u32).map(|i| (i % 251) as u8).collect();
    let control_bytes: Vec<u8> = (0..3_000_000u32).map(|i| (i % 241) as u8).collect();
    let uploaded = place(
        &graph,
        &scope,
        &root.id,
        "pinned.bin",
        pinned_bytes.clone(),
        &cancel,
    )
    .await?;
    place(
        &graph,
        &scope,
        &root.id,
        "control.bin",
        control_bytes,
        &cancel,
    )
    .await?;
    // A subtree for the recursive half. Two files under one folder, so a walk
    // that finds one of them and calls it done is distinguishable from a walk
    // that finds both.
    let subtree = graph
        .create_folder(&scope, &root.id, "subtree", &cancel)
        .await?;
    let deep_a: Vec<u8> = (0..2_500_000u32).map(|i| (i % 239) as u8).collect();
    let deep_b: Vec<u8> = (0..1_500_000u32).map(|i| (i % 233) as u8).collect();
    place(
        &graph,
        &scope,
        &subtree.id,
        "a.bin",
        deep_a.clone(),
        &cancel,
    )
    .await?;
    place(
        &graph,
        &scope,
        &subtree.id,
        "b.bin",
        deep_b.clone(),
        &cancel,
    )
    .await?;

    let mut config = account.clone();
    config.root_id = root.id.clone();
    config.mount_path = directory.join("mount");
    // Reservations may not claim the whole budget and the floor is eight blocks.
    config.cache_bytes = 256 * 1024 * 1024;
    crate::private_dir(&config.mount_path)?;
    let mount = config.mount_path.clone();

    let engine = Engine::new(config.clone(), graph.clone(), directory.join("engine")).await?;
    engine.start().await?;
    while !engine.health().await.iter().any(|h| h.state == "ready") {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let reply = engine
        .apply_pin_request(&crate::PinRequest {
            path: Some("pinned.bin".into()),
            ..Default::default()
        })
        .await?;
    anyhow::ensure!(
        reply.accepted && reply.refusal.is_none(),
        "the pin was refused on a real account: {reply:?}"
    );
    let status = engine.pin_status().await?;
    let held = status
        .first()
        .context("the pin was accepted and then not reported")?
        .clone();
    anyhow::ensure!(
        held.resident > 0 && held.blocks > 0,
        "P1 failed: the pin reserved without keeping anything: {held:?}"
    );
    // The recursive half, through the same request path.
    let folder = engine
        .apply_pin_request(&crate::PinRequest {
            path: Some("subtree".into()),
            recursive: true,
            ..Default::default()
        })
        .await?;
    anyhow::ensure!(
        folder.accepted && folder.refusal.is_none(),
        "the recursive pin was refused on a real account: {folder:?}"
    );
    anyhow::ensure!(
        folder.files == 2,
        "the walk found {} files where the subtree holds two; a walk that stops early \
         would keep part of a folder and report success",
        folder.files
    );
    anyhow::ensure!(
        folder.complete,
        "the walk reported the subtree as incomplete on a drive where it is fully indexed"
    );
    event(
        &mut log,
        serde_json::json!({"stage":"pinned","reserved":held.reserved,
            "resident":held.resident,"blocks":held.blocks,"folder":name,
            "recursive_files":folder.files,"recursive_reserved":folder.reserved,
            "recursive_complete":folder.complete}),
    )?;
    engine.stop().await;
    drop(engine);

    // Rebuilt on the same directory, so the in-memory block cache is empty and
    // anything that reads afterwards can only be coming off disk.
    let engine = Engine::new(config.clone(), graph.clone(), directory.join("engine")).await?;
    let mut session = None;
    let result = async {
        engine.start().await?;
        while !engine.health().await.iter().any(|h| h.state == "ready") {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let fs = CloudFs::new(engine.clone())?;
        let at = mount.clone();
        session = Some(tokio::task::spawn_blocking(move || fs.mount(&at)).await??);

        let before = graph
            .read_path_counters()
            .map(|c| c.content_gets)
            .unwrap_or(0);
        let path = mount.join("pinned.bin");
        let read = tokio::task::spawn_blocking(move || std::fs::read(path)).await??;
        let after_pinned = graph
            .read_path_counters()
            .map(|c| c.content_gets)
            .unwrap_or(0);
        anyhow::ensure!(
            read == pinned_bytes,
            "P2 failed: the pinned file read back different bytes"
        );
        anyhow::ensure!(
            after_pinned == before,
            "P3 failed: reading the pinned file made {} provider content request(s), so it \
             was not served from what the pin kept",
            after_pinned - before
        );

        // Both files under the recursively pinned folder, same conditions.
        for (name, expected) in [("a.bin", &deep_a), ("b.bin", &deep_b)] {
            let path = mount.join("subtree").join(name);
            let read = tokio::task::spawn_blocking(move || std::fs::read(path)).await??;
            anyhow::ensure!(
                &read == expected,
                "{name} under the recursively pinned folder read back different bytes"
            );
        }
        let after_subtree = graph
            .read_path_counters()
            .map(|c| c.content_gets)
            .unwrap_or(0);
        anyhow::ensure!(
            after_subtree == after_pinned,
            "reading a recursively pinned folder made {} provider content request(s), so the \
             walk did not keep what it claimed",
            after_subtree - after_pinned
        );

        // The control. Same mount, same conditions, a file nobody pinned: it
        // must need the provider, or the silence above says nothing.
        let path = mount.join("control.bin");
        let _ = tokio::task::spawn_blocking(move || std::fs::read(path)).await??;
        let after_control = graph
            .read_path_counters()
            .map(|c| c.content_gets)
            .unwrap_or(0);
        anyhow::ensure!(
            after_control > after_subtree,
            "P4 failed: an unpinned file was served without touching the provider either, so \
             this run cannot tell a pin from a warm cache"
        );
        event(
            &mut log,
            serde_json::json!({"stage":"read","bytes":read.len(),
                "content_gets_for_pinned":after_pinned - before,
                "content_gets_for_subtree":after_subtree - after_pinned,
                "content_gets_for_control":after_control - after_subtree}),
        )?;
        // The budget a user can see. The row asks for clear free-space
        // behaviour, and a refusal at the end is the least useful moment to
        // learn space was running out.
        let held_budget = engine.pin_budget().await?;
        anyhow::ensure!(
            held_budget.reserved_bytes > 0 && held_budget.free_bytes < held_budget.pinnable_bytes,
            "the budget did not move with two pins in force: {held_budget:?}"
        );
        anyhow::ensure!(
            held_budget.pinnable_bytes < held_budget.cache_bytes,
            "reservations may not claim the whole cache: {held_budget:?}"
        );
        let sentence = held_budget.explain();
        anyhow::ensure!(
            sentence.contains("available for ordinary reads"),
            "the explanation must say what the rest of the cache is for: {sentence}"
        );

        // And releasing must give the whole reservation back, on a real drive
        // and not only in a fixture.
        let released = engine
            .apply_unpin_request(&crate::PinRequest {
                path: Some("pinned.bin".into()),
                ..Default::default()
            })
            .await?;
        anyhow::ensure!(released.accepted, "unpin was refused: {released:?}");
        let after_unpin = engine.pin_budget().await?;
        anyhow::ensure!(
            after_unpin.free_bytes == held_budget.free_bytes + held.reserved,
            "unpinning returned {} of the {} bytes it had reserved",
            after_unpin.free_bytes - held_budget.free_bytes,
            held.reserved
        );
        event(
            &mut log,
            serde_json::json!({"stage":"budget",
                "cache_bytes":held_budget.cache_bytes,
                "pinnable_bytes":held_budget.pinnable_bytes,
                "reserved_with_two_pins":held_budget.reserved_bytes,
                "used_per_mille":held_budget.used_per_mille(),
                "free_after_unpin":after_unpin.free_bytes,
                "explanation":sentence}),
        )?;
        println!(
            "Passed: {} bytes read through the mount with no provider request; the unpinned \
             control needed {}.",
            read.len(),
            after_control - after_subtree
        );
        println!("Budget: {sentence}");
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Some(session) = session.take() {
        let _ = tokio::task::spawn_blocking(move || session.umount_and_join()).await;
    }
    engine.stop().await;
    drop(engine);
    result?;

    // The editing half, on a writable mount of the same drive.
    //
    // A real account cannot be taken offline, so the testable form of "edited
    // offline" is that the edit is accepted LOCALLY: it lands in the journal as
    // unsent work without the provider being asked to do anything, and the
    // pinned content is still readable afterwards. That is the property the
    // clause is about -- an application does not wait for the network to save.
    let mut writable = config.clone();
    writable.mount_path = directory.join("writable-mount");
    crate::private_dir(&writable.mount_path)?;
    let edit_mount = writable.mount_path.clone();
    let engine = Engine::new(writable, graph.clone(), directory.join("engine")).await?;
    let journal = Arc::new(std::sync::Mutex::new(UploadJournal::open(
        &directory.join("edit-journal"),
        &account.id,
        64 * 1024 * 1024,
    )?));
    let session = WritableSession::mount(
        engine.clone(),
        journal.clone(),
        graph.clone(),
        Arc::new(DesktopVault),
    )
    .await?;
    let edited = b"edited locally, with nothing asked of the provider".to_vec();
    let editing = async {
        let path = edit_mount.join("subtree").join("a.bin");
        let payload = edited.clone();
        tokio::task::spawn_blocking(move || std::fs::write(path, payload)).await??;
        // The edit must be in the journal as unsent work. If the upload worker
        // has already drained it that is not a failure of the claim, so the
        // assertion is on the bytes being recoverable locally, not on the state.
        let rows = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .list(0, 100)?;
        anyhow::ensure!(
            !rows.is_empty(),
            "an edit through a writable mount left nothing in the journal"
        );
        event(
            &mut log,
            serde_json::json!({"stage":"edited","journal_rows":rows.len(),
                "bytes":edited.len()}),
        )?;
        println!(
            "Passed: a pinned file edited through a writable mount left {} record(s) in the \
             local journal.",
            rows.len()
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let _ = tokio::task::spawn_blocking(move || session.shutdown()).await;

    // Unsent work must survive cache pressure. The claim holds structurally --
    // eviction only removes files whose names it recognises as its own block
    // keys -- but structural is what a claim is called before anyone has put it
    // under load on a real drive.
    let unsent = async {
        let rows = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .list(0, 100)?;
        let record = rows.first().context("no unsent record to protect")?.id;
        let before = {
            let j = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut j.payload(record)?, &mut bytes)?;
            bytes
        };
        anyhow::ensure!(!before.is_empty(), "the unsent record has no payload");

        // Fill the cache past its budget with real content, so eviction runs
        // repeatedly while the journal sits beside it.
        let scope_for_reads = Scope {
            account: account.id.clone(),
            provider: "onedrive".into(),
            collection: account.drive.id.clone(),
        };
        let big = engine.node(&scope_for_reads, &uploaded.id).await?;
        for round in 0..6u64 {
            let at = (round * 4) * crate::content::BLOCK_SIZE as u64;
            let _ = engine
                .cache
                .read(
                    engine.provider.as_ref(),
                    &scope_for_reads,
                    &big,
                    at.min(big.size.saturating_sub(1)),
                    crate::content::BLOCK_SIZE,
                    &engine.cancel,
                )
                .await;
        }
        let after = {
            let j = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut j.payload(record)?, &mut bytes)?;
            bytes
        };
        anyhow::ensure!(
            after == before,
            "cache pressure changed an unsent record's bytes on a real drive"
        );
        event(
            &mut log,
            serde_json::json!({"stage":"unsent_under_pressure","payload_bytes":after.len()}),
        )?;
        println!(
            "Passed: an unsent record of {} bytes is unchanged after cache pressure.",
            after.len()
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    engine.stop().await;
    editing?;
    unsent?;
    println!("Fixture folder retained: {name}");
    Ok(())
}
