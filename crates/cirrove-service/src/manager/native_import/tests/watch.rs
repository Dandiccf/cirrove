//! Raw public requests make the absent-watch negative control a runtime refusal.
use super::*;
use crate::{
    jobs::JobState,
    journal::{PackagePublicationStatus, UploadJournal},
};
use anyhow::{Context, ensure};
use cirrove_core::upload::{PackageSemanticIdentity, PackageUploadReceipt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

struct Observation {
    node: Mutex<Node>,
    reads: AtomicUsize,
    blocked: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl MetadataProvider for Observation {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        panic!("watch must not start a delta worker")
    }
}
#[async_trait::async_trait]
impl ReadProvider for Observation {
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        cancel: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.blocked.load(Ordering::SeqCst) {
            self.entered.notify_one();
            cancel.cancelled().await;
            return Err(ProviderError::Cancelled);
        }
        let node = self.node.lock().unwrap().clone();
        if node.id == id {
            Ok(node)
        } else {
            Err(ProviderError::NotFound)
        }
    }
    async fn children(
        &self,
        _: &Scope,
        parent: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        let n = self.node.lock().unwrap().clone();
        Ok(DirectoryPage {
            nodes: if n.parent_id.as_deref() == Some(parent) {
                vec![n]
            } else {
                Vec::new()
            },
            next: None,
        })
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        panic!("watch must never download document content")
    }
}
async fn raw(
    socket: &Path,
    body: serde_json::Value,
) -> anyhow::Result<crate::ImportNativePackageReply> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut peer = UnixStream::connect(socket).await?;
        peer.write_all(format!("watch-native-import {}\n", body).as_bytes())
            .await?;
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).await?;
        Ok(serde_json::from_slice(&bytes)?)
    })
    .await
    .context("watch exchange timeout")?
}
async fn terminal(socket: &Path, id: &str) -> anyhow::Result<crate::jobs::Job> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = crate::status(socket).await?;
            if let Some(job) = status
                .accounts
                .into_iter()
                .flat_map(|a| a.jobs)
                .find(|j| j.id == id)
                && !job.running()
            {
                return Ok(job);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("watch job timeout")?
}
#[tokio::test]
async fn public_watch_reopens_existing_package_after_restart_without_upload_or_replay()
-> anyhow::Result<()> {
    let Fixture {
        _temp,
        manager,
        engine,
        control,
        journal,
        provider,
        source: _,
    } = Fixture::new().await;
    let account = engine.account.clone();
    let state = _temp.path().join("state");
    let journal_path = engine.db.parent().unwrap().join("uploads");
    let semantic = PackageSemanticIdentity {
        version: 1,
        sha256: "a".repeat(64),
        entries: 1,
        files: 1,
        expanded_bytes: 17,
    };
    let mut remote = node(
        "FILE::com.apple.CloudDocs::retained",
        Some(ROOT),
        "Retained.pages",
    );
    remote.package = true;
    remote.size = 17;
    remote.content_version = remote.etag.clone();
    let mut observed = remote.clone();
    observed.content_version = None;
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
            b"owned synthetic archive".as_slice(),
        )?;
        let claimed = j.claim_next()?.context("claim fixture")?;
        j.acknowledge_package(
            row.id,
            claimed.attempt.context("attempt")?,
            PackageUploadReceipt {
                remote: remote.clone(),
                semantic,
            },
        )?;
        let done = j.get(row.id)?;
        j.finish_package_publication(
            &done,
            PackagePublicationStatus::Present(observed.clone()),
            0,
        )?;
        // A non-package record must never become an import observer.
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
    // Release every old account/journal owner: the next engine starts with no jobs.
    engine.stop().await;
    drop(manager);
    drop(control);
    drop(journal);
    drop(engine);
    drop(provider);
    let remote = Arc::new(Observation {
        node: Mutex::new(observed),
        reads: AtomicUsize::new(0),
        blocked: std::sync::atomic::AtomicBool::new(false),
        entered: tokio::sync::Notify::new(),
    });
    let engine = Engine::new(account.clone(), remote.clone(), state).await?;
    ensure!(
        engine.jobs.list().is_empty(),
        "restart retained ephemeral jobs"
    );
    let journal = Arc::new(Mutex::new(UploadJournal::open(
        &journal_path,
        &account.id,
        16 * 1024 * 1024,
    )?));
    let fs = CloudFs::new_experimental_writable(engine.clone(), journal.clone()).await?;
    let control = fs.write_control()?;
    let manager = Arc::new(Manager::default());
    manager
        .engines
        .write()
        .await
        .insert(account.id.clone(), engine.clone());
    manager
        .writers
        .write()
        .await
        .insert(account.id.clone(), control.clone());
    manager.status.write().await.push(AccountStatus {
        account_id: account.id.clone(),
        label: account.label.clone(),
        mount_path: account.mount_path.clone(),
        enabled: true,
        mounted: true,
        ..Default::default()
    });
    let before = serde_json::to_value(journal.lock().unwrap().list(0, 100)?)?;
    let ordinary = journal.lock().unwrap().list(0, 100)?[1].id;
    let runtime = tempfile::tempdir_in("/var/tmp")?;
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))?;
    let socket = runtime.path().join("watch.sock");
    let cancel = CancellationToken::new();
    let server = {
        let m = manager.clone();
        let s = socket.clone();
        let c = cancel.clone();
        let db = runtime.path().join("status.db");
        tokio::spawn(async move { crate::serve_managed(db, s, c, Some(m)).await })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while !socket.exists() {
            ensure!(
                !server.is_finished(),
                "watch fixture server exited before binding private socket"
            );
            tokio::task::yield_now().await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("watch fixture socket startup timed out")??;
    let outcome:anyhow::Result<()>=async {
        let request=serde_json::json!({"label":account.label,"expected_account_id":account.id,"operation":operation});
        for arm in ["wrong-account","missing","ordinary","missing-account","extra-source"] {
            let mut body=request.clone();match arm {
                "wrong-account"=>body["expected_account_id"]=uuid::Uuid::new_v4().to_string().into(),
                "missing"=>body["operation"]=uuid::Uuid::new_v4().to_string().into(),
                "ordinary"=>body["operation"]=ordinary.to_string().into(),
                "missing-account"=>{body.as_object_mut().unwrap().remove("expected_account_id");},
                _=>body["archive"]="/must/not/open.pages".into(),
            }
            ensure!(raw(&socket,body).await?.job.is_none(),"{arm} became watch");
        }
        ensure!(engine.jobs.list().is_empty(),"rejected watch created jobs");
        let started=raw(&socket,request.clone()).await?.job.context("retained import watch was not accepted")?;
        ensure!(started.native_import.as_ref().is_some_and(|p|p.operation==operation),"initial watch not operation bound");
        let done=terminal(&socket,&started.id).await?;
        ensure!(done.state==JobState::Succeeded,"retained import did not reach corrected public success");
        let receipt=done.native_import.context("receipt")?;
        ensure!(receipt.operation==operation&&receipt.remote.as_ref().is_some_and(|n|n.id=="FILE::com.apple.CloudDocs::retained"&&n.content_version.is_none()),"wrong public receipt");
        ensure!(remote.reads.load(Ordering::SeqCst)>0,"watch trusted stale publication instead of current exact metadata");
        // Existing outbox remains Present(v1); the cloud observation has moved on.
        remote.node.lock().unwrap().etag=Some("v2".into());
        let changed=raw(&socket,request.clone()).await?.job.context("second watch")?;
        ensure!(terminal(&socket,&changed.id).await?.state==JobState::Failed,"stale durable publication reported current success");
        remote.blocked.store(true,Ordering::SeqCst);
        let blocked=raw(&socket,request).await?.job.context("stoppable watch")?;
        tokio::time::timeout(Duration::from_secs(3),remote.entered.notified()).await?;
        crate::stop_job(&socket,&crate::StopJobRequest{label:account.label.clone(),id:blocked.id.clone()}).await?;
        ensure!(terminal(&socket,&blocked.id).await?.state==JobState::Stopped,"stop did not stop only observer");
        ensure!(serde_json::to_value(journal.lock().unwrap().list(0,100)?)?==before,"watch changed journal state/attempts or queued another upload");
        Ok(())
    }.await;
    cancel.cancel();
    server.await??;
    engine.stop().await;
    outcome
}
