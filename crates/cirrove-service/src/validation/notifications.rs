//! End-to-end notification proof: subscribe, save a current marker, create only
//! a unique fixture, and require its ID in the delta fetched because of a hint.
use super::*;
use cirrove_core::{
    Change, Checkpoint, MetadataProvider,
    notifications::{ChangeHintSender, NotificationState},
};

pub async fn onedrive_notifications(state: &Path, label: &str) -> Result<()> {
    let account = test_account(state, label)?;
    let _operation = accounts::account_operation(state, &account.id)?;
    let _owner = accounts::account_lock(&state.join("accounts").join(&account.id))?;
    let run = uuid::Uuid::new_v4();
    let directory = state.join("notification-checks").join(run.to_string());
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
    let name = format!("Cirrove-Notification-Validation-{run}");
    event(
        &mut log,
        serde_json::json!({"stage":"planned","folder":name,"scope":scope}),
    )?;
    for ancestor in directory.ancestors() {
        File::open(ancestor)?.sync_all()?;
    }
    println!(
        "Connecting Graph notifications; private evidence: {}",
        directory.display()
    );
    let graph = accounts::provider(&account)?;
    let cancel = CancellationToken::new();
    let _cancel_on_return = cancel.clone().drop_guard();
    let (hints, mut receiver) = ChangeHintSender::channel();
    let provider = graph.clone();
    let watched_scope = scope.clone();
    let token = cancel.clone();
    let mut listener =
        tokio::spawn(async move { provider.watch_changes(&watched_scope, hints, &token).await });
    tokio::select! {
        result=&mut listener => { result?.map_err(anyhow::Error::from)?; bail!("notification session ended before subscribing"); }
        result=tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if receiver.borrow_and_update().state == NotificationState::Connected { break; }
                receiver.changed().await.context("notification channel closed")?;
            }
            Ok::<(),anyhow::Error>(())
        }) => result.context("notification subscription timed out")??,
    }
    event(&mut log, serde_json::json!({"stage":"connected"}))?;
    println!("Socket.IO subscription connected. Establishing a change marker.");
    let mut cursor = graph.notification_checkpoint(&scope, &cancel).await?;
    let mut previous: Option<Node> = None;
    for sample in 0..3 {
        let mut generation = receiver.borrow_and_update().generation;
        let started = tokio::time::Instant::now();
        let current_name = if sample == 0 {
            name.clone()
        } else {
            format!("{name}-renamed-{sample}")
        };
        event(
            &mut log,
            serde_json::json!({"stage":"change_planned","sample":sample,"name":current_name}),
        )?;
        let folder = if let Some(before) = previous.take() {
            use cirrove_core::ReadProvider;
            use cirrove_core::mutation::{
                MutationIntent, MutationProvider, MutationReceipt, MutationRequest,
            };
            // Folder metadata can advance after creation. Establish the current
            // revision of this run's fixture before testing the next rename;
            // refuse to adopt an externally moved/renamed object.
            let current = graph.node(&scope, &before.id, &cancel).await?;
            if current.name != before.name || current.parent_id != before.parent_id {
                bail!("notification fixture changed outside this test; retained for review");
            }
            let receipt = graph
                .mutate(
                    &MutationRequest {
                        scope: scope.clone(),
                        intent: MutationIntent::Relocate {
                            before: current,
                            parent: account.root_id.clone(),
                            name: current_name.clone(),
                        },
                    },
                    &cancel,
                )
                .await?;
            match receipt {
                MutationReceipt::Upsert(node) => node,
                _ => bail!("fixture rename has no receipt"),
            }
        } else {
            graph
                .create_folder(&scope, &account.root_id, &current_name, &cancel)
                .await?
        };
        event(
            &mut log,
            serde_json::json!({"stage":"change_acknowledged","sample":sample,"node":folder,"elapsed_ms":started.elapsed().as_millis()}),
        )?;
        println!("Synthetic change acknowledged; waiting for its notification-triggered delta.");
        let deadline = started + Duration::from_secs(90);
        let mut hints_seen = 0;
        loop {
            tokio::select! {biased;
                _=tokio::time::sleep_until(deadline)=>bail!("no matching notification-triggered delta within deadline; fixture retained"),
                result=&mut listener=>{ result?.map_err(anyhow::Error::from)?; bail!("notification session ended during validation"); }
                result=async {
                    loop {
                        let current = receiver.borrow_and_update().generation;
                        if current != generation { generation = current; break; }
                        receiver.changed().await.context("notification channel closed")?;
                    }
                    Ok::<(),anyhow::Error>(())
                }=>result?,
            }
            hints_seen += 1;
            event(
                &mut log,
                serde_json::json!({"stage":"hint_received","sample":sample,"elapsed_ms":started.elapsed().as_millis()}),
            )?;
            let mut found = false;
            for page_number in 0..100 {
                let page = tokio::time::timeout_at(
                    deadline,
                    graph.changes(&scope, Some(&cursor), &cancel),
                )
                .await??;
                found |= page.changes.iter().any(|change|matches!(change, Change::Upsert(node) if node.id == folder.id && node.name == current_name));
                let complete = matches!(page.checkpoint, Checkpoint::Complete(_));
                cursor = page.checkpoint.cursor().clone();
                if complete {
                    break;
                }
                if page_number == 99 {
                    bail!("notification delta exceeded page budget");
                }
            }
            if found {
                event(
                    &mut log,
                    serde_json::json!({"stage":"passed","sample":sample,"elapsed_ms":started.elapsed().as_millis(),"hints":hints_seen}),
                )?;
                println!(
                    "Passed: Graph notification triggered a delta containing the new fixture. Folder retained."
                );
                break;
            }
        }
        previous = Some(folder);
    }
    cancel.cancel();
    let _ = listener.await?;
    Ok(())
}
