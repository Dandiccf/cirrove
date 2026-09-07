//! Developer fixture for real Graph directory listings through an actual mount.
//! A fixture-only baseline suppresses delta/push, isolating activity revalidation.
use super::*;
use crate::{engine::Engine, filesystem::CloudFs};
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, ProviderError,
    ReadProvider,
    mutation::{MutationIntent, MutationProvider, MutationReceipt, MutationRequest},
};
use std::{ffi::OsString, sync::atomic::AtomicU64};

struct DirectoryOnly {
    graph: Arc<OneDrive>,
    scope: Scope,
    root: Node,
    pages: AtomicU64,
    baselines: AtomicU64,
}
#[async_trait::async_trait]
impl MetadataProvider for DirectoryOnly {
    fn provider_id(&self) -> &'static str {
        "onedrive"
    }
    async fn changes(
        &self,
        scope: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        if scope != &self.scope {
            return Err(ProviderError::Permission);
        }
        self.baselines.fetch_add(1, Ordering::SeqCst);
        Ok(ChangePage {
            changes: vec![Change::Upsert(self.root.clone())],
            checkpoint: Checkpoint::Complete(Cursor("fixture-baseline".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for DirectoryOnly {
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        if scope != &self.scope || id != self.root.id {
            return Err(ProviderError::Permission);
        }
        Ok(self.root.clone())
    }
    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        if scope != &self.scope || parent != self.root.id {
            return Err(ProviderError::Permission);
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.graph.children(scope, parent, cursor, cancel).await
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        // This check observes only folder names. It cannot validate content reads
        // or accidentally cause a file-manager thumbnail download.
        Err(ProviderError::Permission)
    }
}
async fn mounted_names(mount: &Path) -> Result<Vec<OsString>> {
    let mount = mount.to_owned();
    Ok(tokio::task::spawn_blocking(move || {
        std::fs::read_dir(mount)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<Vec<_>>>()
    })
    .await??)
}

pub async fn onedrive_freshness(state: &Path, label: &str) -> Result<()> {
    let account = test_account(state, label)?;
    let _operation = accounts::account_operation(state, &account.id)?;
    let _owner = accounts::account_lock(&state.join("accounts").join(&account.id))?;
    let run = uuid::Uuid::new_v4();
    let directory = state.join("freshness-checks").join(run.to_string());
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
    let name = format!("Cirrove-Freshness-Validation-{run}");
    event(
        &mut log,
        serde_json::json!({"stage":"planned","folder":name,"scope":scope,"mode":"directory-only; synthetic baseline; no Graph delta or push"}),
    )?;
    for ancestor in directory.ancestors() {
        File::open(ancestor)?.sync_all()?;
    }
    println!(
        "Creating one new fixture folder; private evidence: {}",
        directory.display()
    );
    let graph = accounts::provider(&account)?;
    let cancel = CancellationToken::new();
    let _cancel_on_return = cancel.clone().drop_guard();
    let root = graph
        .create_folder(&scope, &account.root_id, &name, &cancel)
        .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"created_root","node":root}),
    )?;
    let provider = Arc::new(DirectoryOnly {
        graph: graph.clone(),
        scope: scope.clone(),
        root: root.clone(),
        pages: AtomicU64::new(0),
        baselines: AtomicU64::new(0),
    });
    let mut config = account.clone();
    config.root_id = root.id.clone();
    config.mount_path = directory.join("mount");
    config.poll_seconds = 3600;
    crate::private_dir(&config.mount_path)?;
    let engine = Engine::new(config, provider.clone(), directory.join("engine")).await?;
    let mut session = None;
    let run_check = async {
        engine.start().await?;
        loop {
            if engine.health().await.iter().any(|h| h.state == "ready") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let fs = CloudFs::new(engine.clone())?;
        let mount = engine.account.mount_path.clone();
        session = Some(tokio::task::spawn_blocking(move || fs.mount(&mount)).await??);
        println!(
            "Temporary read-only mount ready; Graph delta and push are excluded from this check."
        );
        // Complete the first real listing before changing anything: subsequent
        // refreshes, rather than a lucky first request, must expose the changes.
        loop {
            let names = mounted_names(&engine.account.mount_path).await?;
            if provider.pages.load(Ordering::SeqCst) > 0
                && engine.directory_freshness().refreshing == 0
                && engine.directory_freshness().delayed == 0
            {
                anyhow::ensure!(
                    names.is_empty(),
                    "new fixture directory is not empty; retained for review"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let baselines = provider.baselines.load(Ordering::SeqCst);
        let mut previous: Option<Node> = None;
        for sample in 0..3 {
            let current_name = format!("Grüße-{sample}");
            let previous_name = previous.as_ref().map(|n| n.name.clone());
            event(
                &mut log,
                serde_json::json!({"stage":"change_planned","sample":sample,"name":current_name}),
            )?;
            let started = tokio::time::Instant::now();
            let folder = if let Some(before) = previous.take() {
                let current = graph.node(&scope, &before.id, &cancel).await?;
                anyhow::ensure!(
                    current.name == before.name && current.parent_id == before.parent_id,
                    "fixture changed outside this check; retained for review"
                );
                match graph
                    .mutate(
                        &MutationRequest {
                            scope: scope.clone(),
                            intent: MutationIntent::Relocate {
                                before: current,
                                parent: root.id.clone(),
                                name: current_name.clone(),
                            },
                        },
                        &cancel,
                    )
                    .await?
                {
                    MutationReceipt::Upsert(node) => node,
                    _ => bail!("fixture rename has no receipt"),
                }
            } else {
                graph
                    .create_folder(&scope, &root.id, &current_name, &cancel)
                    .await?
            };
            let acknowledged = tokio::time::Instant::now();
            let pages = provider.pages.load(Ordering::SeqCst);
            event(
                &mut log,
                serde_json::json!({"stage":"acknowledged","sample":sample,"node":folder,"elapsed_ms":started.elapsed().as_millis()}),
            )?;
            tokio::time::timeout(Duration::from_secs(45), async {
                loop {
                    let names = mounted_names(&engine.account.mount_path).await?;
                    if names.iter().any(|n| n == current_name.as_str())
                        && previous_name
                            .as_ref()
                            .is_none_or(|old| names.iter().all(|n| n != old.as_str()))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            .context("fixture change not visible through mount before deadline")??;
            anyhow::ensure!(
                provider.baselines.load(Ordering::SeqCst) == baselines,
                "an extra baseline interfered with directory-only validation"
            );
            event(
                &mut log,
                serde_json::json!({"stage":"passed","sample":sample,"after_ack_ms":acknowledged.elapsed().as_millis(),"total_ms":started.elapsed().as_millis(),"directory_pages":provider.pages.load(Ordering::SeqCst).saturating_sub(pages)}),
            )?;
            println!("Passed: generated folder change is visible through the actual mount.");
            previous = Some(folder);
        }
        Ok::<(), anyhow::Error>(())
    };
    let result = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(180),run_check) => result.context("freshness validation deadline reached").and_then(|r|r),
        _ = tokio::signal::ctrl_c() => Err(anyhow::anyhow!("freshness validation cancelled")),
    };
    cancel.cancel();
    engine.stop().await;
    if let Some(session) = session {
        tokio::task::spawn_blocking(move || session.umount_and_join()).await??;
    }
    event(
        &mut log,
        serde_json::json!({"stage":"finished","passed":result.is_ok(),"fixture_retained":true}),
    )?;
    result
}
