//! Actual public local-only discovery and read-only recovery; no Apple IO.
use super::*;
use crate::journal::{PackagePublicationStatus, UploadState};
use anyhow::{Context, ensure};
use cirrove_core::upload::{PackageSemanticIdentity, PackageUploadReceipt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use uuid::Uuid;
async fn raw(
    socket: &Path,
    body: serde_json::Value,
) -> anyhow::Result<crate::ListNativeImportsReply> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut peer = UnixStream::connect(socket).await?;
        peer.write_all(format!("list-native-imports {}\n", body).as_bytes())
            .await?;
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).await?;
        Ok(serde_json::from_slice(&bytes)?)
    })
    .await
    .context("bounded list exchange")?
}
async fn server(
    manager: Arc<Manager>,
    root: &Path,
    socket: &Path,
    cancel: CancellationToken,
) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<()>>> {
    let task = tokio::spawn(crate::serve_managed(
        root.join("socket.sqlite"),
        socket.to_owned(),
        cancel,
        Some(manager),
    ));
    tokio::time::timeout(Duration::from_secs(3), async {
        while !socket.exists() {
            ensure!(!task.is_finished(), "private server ended");
            tokio::task::yield_now().await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(task)
}
#[tokio::test]
async fn public_saved_import_discovery_survives_readonly_restart_without_jobs_or_replay()
-> anyhow::Result<()> {
    let Fixture {
        _temp,
        manager,
        engine,
        control,
        journal,
        provider,
        source: _,
    } = Fixture::with_journal("journal").await;
    let mut account = engine.account.clone();
    let state = _temp.path().join("state");
    let journal_path = engine.db.parent().unwrap().join("journal");
    let semantic = PackageSemanticIdentity {
        version: 1,
        sha256: "a".repeat(64),
        entries: 1,
        files: 1,
        expanded_bytes: 3,
    };
    let mut remote = node(
        "FILE::com.apple.CloudDocs::saved",
        Some(ROOT),
        "Saved.pages",
    );
    remote.package = true;
    remote.size = 3;
    let operation = {
        let mut j = journal.lock().unwrap();
        let row = j.enqueue_package_archive(
            engine.scope("drive"),
            UploadIntent::Create {
                parent: ROOT.into(),
                name: remote.name.clone(),
            },
            "Source.pages".into(),
            semantic.clone(),
            b"retained archive".as_slice(),
        )?;
        let claimed = j.claim_next()?.context("claim historical fixture")?;
        j.acknowledge_package(
            row.id,
            claimed.attempt.context("attempt")?,
            PackageUploadReceipt {
                remote: remote.clone(),
                semantic: semantic.clone(),
            },
        )?;
        let done = j.get(row.id)?;
        j.finish_package_publication(&done, PackagePublicationStatus::Present(remote.clone()), 0)?;
        j.enqueue_package_archive(
            engine.scope("drive"),
            UploadIntent::Create {
                parent: ROOT.into(),
                name: "Pending.pages".into(),
            },
            "Source.pages".into(),
            semantic,
            b"pending archive".as_slice(),
        )?;
        j.enqueue(
            engine.scope("drive"),
            UploadIntent::Create {
                parent: ROOT.into(),
                name: "ordinary.txt".into(),
            },
            b"ordinary".as_slice(),
        )?;
        row.id
    };
    let frozen = serde_json::to_value(journal.lock().unwrap().list(0, 100)?)?;
    let roots = provider.roots.load(Ordering::SeqCst);
    let reads = provider.reads.load(Ordering::SeqCst);
    let socket = _temp.path().join("list.sock");
    let cancel = CancellationToken::new();
    let task = server(manager.clone(), _temp.path(), &socket, cancel.clone()).await?;
    let outcome:anyhow::Result<()>=async {
        ensure!(crate::capabilities(&socket).await?.capabilities.get("list-native-imports")==Some(&1),"listing capability missing");
        let request=serde_json::json!({"label":account.label,"expected_account_id":account.id,"limit":1});
        for arm in ["wrong-account","missing-account","empty-label","wrong-label","limit-zero","limit-large","cursor-large","extra-archive","extra-retry","extra-operation"] {
            let mut body=request.clone();match arm {
                "wrong-account"=>body["expected_account_id"]=Uuid::new_v4().to_string().into(),
                "missing-account"=>{body.as_object_mut().unwrap().remove("expected_account_id");},
                "empty-label"=>body["label"]="".into(),"wrong-label"=>body["label"]="other".into(),
                "limit-zero"=>body["limit"]=0.into(),"limit-large"=>body["limit"]=101.into(),"cursor-large"=>body["after"]=u64::MAX.into(),
                "extra-archive"=>body["archive"]="/must/not/open.pages".into(),"extra-retry"=>body["retry"]=true.into(),_=>body["operation"]=operation.to_string().into(),
            }
            let refused=raw(&socket,body).await?;ensure!(refused.refusal.is_some()&&refused.operations.is_empty()&&refused.next.is_none(),"{arm} admitted");
        }
        let first=crate::list_native_imports(&socket,&crate::ListNativeImportsRequest {label:account.label.clone(),expected_account_id:account.id.clone(),after:None,limit:1}).await?;
        ensure!(first.refusal.is_none()&&first.operations.len()==1&&first.operations[0].operation==operation&&first.operations[0].completion_receipt_recorded&&first.operations[0].remote_item.as_deref()==Some(remote.id.as_str()),"wrong historical completed selection");
        let next=first.next.context("missing sequence cursor")?;
        let second=raw(&socket,serde_json::json!({"label":account.label,"expected_account_id":account.id,"after":next,"limit":1})).await?;
        ensure!(second.operations.len()==1&&second.operations[0].state==UploadState::Pending&&!second.operations[0].completion_receipt_recorded&&second.operations[0].remote_item.is_none()&&second.next.is_none(),"pending import or ordinary exclusion wrong");
        ensure!(engine.jobs.list().is_empty(),"listing started jobs");
        ensure!(provider.roots.load(Ordering::SeqCst)==roots&&provider.reads.load(Ordering::SeqCst)==reads,"listing requested metadata/content");
        ensure!(serde_json::to_value(journal.lock().unwrap().list(0,100)?)?==frozen,"listing modified journal rows");
        Ok(())
    }.await;
    cancel.cancel();
    task.await??;
    outcome?;
    engine.stop().await;
    drop(manager);
    drop(control);
    drop(journal);
    drop(engine);
    drop(provider);
    let before = std::fs::read(journal_path.join("uploads.db"))?;
    account.access = cirrove_auth::AccessMode::ReadOnly;
    // No provider call is allowed; observation is a separate explicit action.
    let provider = Arc::new(Provider {
        nodes: Mutex::new(vec![]),
        roots: AtomicUsize::new(0),
        pause: Mutex::new(None),
        reads: AtomicUsize::new(0),
    });
    let engine = Engine::new(account.clone(), provider.clone(), state).await?;
    let manager = Arc::new(Manager::default());
    manager
        .engines
        .write()
        .await
        .insert(account.id.clone(), engine.clone());
    manager.status.write().await.push(AccountStatus {
        account_id: account.id.clone(),
        label: account.label.clone(),
        enabled: true,
        mounted: false,
        ..Default::default()
    });
    let cancel = CancellationToken::new();
    let task = server(manager.clone(), _temp.path(), &socket, cancel.clone()).await?;
    let outcome: anyhow::Result<()> = async {
        let page = raw(
            &socket,
            serde_json::json!({"label":account.label,"expected_account_id":account.id,"limit":100}),
        )
        .await?;
        ensure!(
            page.refusal.is_none()
                && page.operations.len() == 2
                && page.operations[0].operation == operation
                && page.operations[0].completion_receipt_recorded,
            "read-only restart lost saved import"
        );
        ensure!(
            engine.jobs.list().is_empty() && manager.writers.read().await.is_empty(),
            "read-only discovery started workers/jobs"
        );
        ensure!(
            provider.roots.load(Ordering::SeqCst) == 0
                && provider.reads.load(Ordering::SeqCst) == 0,
            "read-only discovery requested cloud data"
        );
        let recovery = manager.recovery_control(engine.clone()).await?;
        // Existing generic recent history remains untouched too.
        ensure!(
            recovery.recent_local(100).await?.len() == 3,
            "history changed"
        );
        Ok(())
    }
    .await;
    cancel.cancel();
    task.await??;
    engine.stop().await;
    outcome?;
    ensure!(
        std::fs::read(journal_path.join("uploads.db"))? == before,
        "read-only discovery changed database bytes"
    );
    Ok(())
}
#[tokio::test]
async fn saved_import_listing_rechecks_account_owner_after_journal_wait() {
    let f = Fixture::new().await;
    f.manager
        .enqueue_native_package(f.engine.clone(), f.input(), CancellationToken::new())
        .await
        .unwrap();
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let journal = f.journal.clone();
    let holder = tokio::task::spawn_blocking(move || {
        let _held = journal.lock().unwrap();
        locked_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    locked_rx.await.unwrap();
    let mut listing =
        Box::pin(
            f.manager
                .list_native_imports(&f.engine, &f.engine.account.id, None, 100),
        );
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(listing.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    f.manager.engines.write().await.remove(&f.engine.account.id);
    release_tx.send(()).unwrap();
    assert!(
        listing.await.is_err(),
        "withdrawn account retained discovery authority"
    );
    holder.await.unwrap();
    assert_eq!(f.journal.lock().unwrap().list(0, 100).unwrap().len(), 1);
    assert!(f.engine.jobs.list().is_empty());
    f.engine.stop().await;
}
