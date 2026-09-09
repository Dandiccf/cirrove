//! Developer fixture for milestone 4's claim that a mounted view follows remote
//! creates, edits, moves and deletions.
//!
//! Provider-level mutations are covered elsewhere. What is not is whether the
//! filesystem somebody is actually looking at follows them, which is a different
//! claim: the provider can accept a rename that the mount never reflects, and
//! nothing in a provider-level check would notice.
//!
//! No mechanism is isolated here. The box asks that the view is correct, not
//! which of the three refresh routes produced it, so the fixture watches the
//! mount the way a person would and reports how long each change took to land.
use super::*;
use crate::{engine::Engine, filesystem::CloudFs};
use cirrove_core::ReadProvider;
use cirrove_core::mutation::{MutationIntent, MutationProvider, MutationRequest};
use std::{collections::HashSet, ffi::OsString};

async fn names(mount: &Path) -> Result<HashSet<OsString>> {
    let mount = mount.to_owned();
    Ok(tokio::task::spawn_blocking(move || {
        std::fs::read_dir(mount)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<HashSet<_>>>()
    })
    .await??)
}
/// Wait for the mounted listing to satisfy `settled`, returning how long it took.
async fn until(
    mount: &Path,
    deadline: Duration,
    settled: impl Fn(&HashSet<OsString>) -> bool,
) -> Result<Duration> {
    let started = tokio::time::Instant::now();
    loop {
        if settled(&names(mount).await?) {
            return Ok(started.elapsed());
        }
        anyhow::ensure!(
            started.elapsed() < deadline,
            "the mounted view never followed the change within {deadline:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
pub async fn onedrive_remote_changes(state: &Path, label: &str) -> Result<()> {
    let account = test_account(state, label)?;
    let _operation = accounts::account_operation(state, &account.id)?;
    let _owner = accounts::account_lock(&state.join("accounts").join(&account.id))?;
    let run = uuid::Uuid::new_v4();
    let directory = state.join("remote-change-checks").join(run.to_string());
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
    let name = format!("Cirrove-RemoteChange-Validation-{run}");
    let cancel = CancellationToken::new();
    let _cancel_on_return = cancel.clone().drop_guard();
    let graph = accounts::provider(&account)?;
    let root = graph
        .create_folder(&scope, &account.root_id, &name, &cancel)
        .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"planned","folder":name,"private_evidence":directory}),
    )?;
    println!(
        "Watching a mounted view follow remote changes; private evidence: {}",
        directory.display()
    );

    let mut config = account.clone();
    config.root_id = root.id.clone();
    config.mount_path = directory.join("mount");
    crate::private_dir(&config.mount_path)?;
    let engine = Engine::new(config, graph.clone(), directory.join("engine")).await?;
    let mount = engine.account.mount_path.clone();
    let mut session = None;
    let request = |intent| MutationRequest {
        scope: scope.clone(),
        intent,
    };
    let result = async {
        engine.start().await?;
        while !engine.health().await.iter().any(|h| h.state == "ready") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let fs = CloudFs::new(engine.clone())?;
        let at = mount.clone();
        session = Some(tokio::task::spawn_blocking(move || fs.mount(&at)).await??);
        until(&mount, Duration::from_secs(60), |names| names.is_empty()).await?;

        // Create.
        let created = graph
            .create_folder(&scope, &root.id, "created", &cancel)
            .await?;
        let create_ms = until(&mount, Duration::from_secs(120), |names| {
            names.contains(OsString::from("created").as_os_str())
        })
        .await?;

        // Move, which here is a rename within the same parent. The precondition
        // has to come from a fresh read: the node returned by creation carries the
        // validator it had at creation, and a conditional mutation against a stale
        // one is refused as a conflict that never happened.
        let before = graph.node(&scope, &created.id, &cancel).await?;
        graph
            .mutate(
                &request(MutationIntent::Relocate {
                    before,
                    parent: root.id.clone(),
                    name: "renamed".into(),
                }),
                &cancel,
            )
            .await?;
        let move_ms = until(&mount, Duration::from_secs(120), |names| {
            names.contains(OsString::from("renamed").as_os_str())
                && !names.contains(OsString::from("created").as_os_str())
        })
        .await?;
        // Both halves matter. A view that shows the new name and keeps the old
        // one has followed nothing; it has added.

        // Delete. The renamed folder is empty, so this is an ordinary rmdir.
        let current = graph.node(&scope, &created.id, &cancel).await?;
        graph
            .mutate(
                &request(MutationIntent::RemoveFolder { before: current }),
                &cancel,
            )
            .await?;
        let delete_ms = until(&mount, Duration::from_secs(120), |names| {
            !names.contains(OsString::from("renamed").as_os_str())
        })
        .await?;

        let report = serde_json::json!({
            "stage": "finished", "passed": true,
            "create_visible_ms": create_ms.as_millis(),
            "move_visible_ms": move_ms.as_millis(),
            "delete_visible_ms": delete_ms.as_millis(),
        });
        event(&mut log, report.clone())?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        Ok::<(), anyhow::Error>(())
    }
    .await;
    cancel.cancel();
    engine.stop().await;
    if let Some(session) = session {
        tokio::task::spawn_blocking(move || session.umount_and_join()).await??;
    }
    result
}
