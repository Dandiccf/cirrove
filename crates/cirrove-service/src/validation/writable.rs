//! Actual application writes through FUSE, restricted to a new run-owned folder.
use super::*;
use crate::{engine::Engine, writable::WritableSession};
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, NodeKind,
    ProviderError, ReadProvider,
};
use std::collections::HashSet;

struct FixtureGraph {
    graph: Arc<OneDrive>,
    scope: Scope,
    root: Node,
    owned: Mutex<HashSet<String>>,
}
impl FixtureGraph {
    fn item_allowed(&self, scope: &Scope, id: &str) -> bool {
        scope == &self.scope
            && (id == self.root.id || self.owned.lock().is_ok_and(|owned| owned.contains(id)))
    }
    fn request_allowed(&self, request: &UploadRequest) -> cirrove_core::upload::Result<()> {
        let allowed = request.scope == self.scope
            && match &request.intent {
                UploadIntent::Create { parent, .. } => parent == &self.root.id,
                UploadIntent::Replace { item, .. } => {
                    item != &self.root.id && self.item_allowed(&request.scope, item)
                }
            };
        if !allowed {
            return Err(ProviderError::Permission.into());
        }
        request.validate()
    }
    fn receipt(&self, request: &UploadRequest, node: &Node) -> cirrove_core::upload::Result<()> {
        let matches = match &request.intent {
            UploadIntent::Create { name, .. } => name == &node.name,
            UploadIntent::Replace { item, .. } => item == &node.id,
        };
        if !matches
            || node.kind != NodeKind::File
            || node.target.is_some()
            || node.parent_id.as_ref() != Some(&self.root.id)
        {
            return Err(UploadError::Uncertain);
        }
        self.owned
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .insert(node.id.clone());
        Ok(())
    }
    fn step(
        &self,
        request: &UploadRequest,
        step: UploadStep,
    ) -> cirrove_core::upload::Result<UploadStep> {
        if let UploadStep::Complete(node) = &step {
            self.receipt(request, node)?;
        }
        Ok(step)
    }
}
#[async_trait::async_trait]
impl MetadataProvider for FixtureGraph {
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
        // Index only this run's root. Full-account delta and notifications are
        // outside this mounted-write check and have their own validators.
        Ok(ChangePage {
            changes: vec![Change::Upsert(self.root.clone())],
            checkpoint: Checkpoint::Complete(Cursor("writable-fixture-baseline".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for FixtureGraph {
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        cancel: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        if !self.item_allowed(scope, id) {
            return Err(ProviderError::Permission);
        }
        if id == self.root.id {
            return Ok(self.root.clone());
        }
        let node = self.graph.node(scope, id, cancel).await?;
        if node.parent_id.as_ref() != Some(&self.root.id) || node.target.is_some() {
            return Err(ProviderError::Permission);
        }
        Ok(node)
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
        let mut page = self.graph.children(scope, parent, cursor, cancel).await?;
        page.nodes.retain(|node| self.item_allowed(scope, &node.id));
        Ok(page)
    }
    async fn read_range(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        if !self.item_allowed(scope, &node.id) || node.id == self.root.id {
            return Err(ProviderError::Permission);
        }
        self.graph
            .read_range(scope, node, offset, length, cancel)
            .await
    }
}
#[async_trait::async_trait]
impl UploadProvider for FixtureGraph {
    async fn begin_upload(
        &self,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.request_allowed(r)?;
        self.step(r, self.graph.begin_upload(r, c).await?)
    }
    async fn inspect_upload(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.request_allowed(r)?;
        self.step(r, self.graph.inspect_upload(r, s, c).await?)
    }
    async fn upload_part(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        o: u64,
        b: Vec<u8>,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.request_allowed(r)?;
        self.step(r, self.graph.upload_part(r, s, o, b, c).await?)
    }
    async fn commit_upload(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.request_allowed(r)?;
        self.step(r, self.graph.commit_upload(r, s, c).await?)
    }
    async fn reconcile_upload(
        &self,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        self.request_allowed(r)?;
        let result = self.graph.reconcile_upload(r, c).await?;
        if let Reconciliation::Committed(node) = &result {
            self.receipt(r, node)?;
        }
        Ok(result)
    }
}
const APP_WRITES: &str = r#"
import hashlib,json,os,sys,time
path=sys.argv[1]
records=[]
with open(path,'x+b',buffering=0) as file:
    for generation,size in ((1,131071),(2,262177)):
        data=bytes((i*13+generation*17)%251 for i in range(size))
        start=time.monotonic()
        file.seek(0)
        view=memoryview(data)
        while view:
            count=file.write(view)
            if not count: raise OSError('short write')
            view=view[count:]
        file.truncate(size)
        os.fsync(file.fileno())
        records.append(dict(generation=generation,size=size,sha256=hashlib.sha256(data).hexdigest(),local_save_ms=(time.monotonic()-start)*1000))
with open(path,'rb') as file:
    assert file.read()==data
print(json.dumps(records))
"#;

pub async fn onedrive_writable(state: &Path, label: &str) -> Result<()> {
    let account = test_account(state, label)?;
    let _operation = accounts::account_operation(state, &account.id)?;
    let _owner = accounts::account_lock(&state.join("accounts").join(&account.id))?;
    let run = uuid::Uuid::new_v4();
    let directory = state.join("mounted-write-checks").join(run.to_string());
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
    let name = format!("Cirrove-Mounted-Write-Validation-{run}");
    event(
        &mut log,
        serde_json::json!({"stage":"planned","folder":name,"scope":scope,"mode":"run-owned folder; fixture baseline; automatic real uploads; separate application process"}),
    )?;
    for ancestor in directory.ancestors() {
        File::open(ancestor)?.sync_all()?;
    }
    println!(
        "Creating a dedicated writable fixture; private evidence: {}",
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
    let provider = Arc::new(FixtureGraph {
        graph,
        scope,
        root: root.clone(),
        owned: Mutex::new(HashSet::new()),
    });
    let mut config = account.clone();
    config.root_id = root.id;
    config.mount_path = directory.join("mount");
    config.poll_seconds = 3600;
    crate::private_dir(&config.mount_path)?;
    let engine = Engine::new(config, provider.clone(), directory.join("engine")).await?;
    let journal = Arc::new(Mutex::new(UploadJournal::open(
        &directory.join("journal"),
        &account.id,
        64 * 1024 * 1024,
    )?));
    let session = WritableSession::mount(
        engine.clone(),
        journal,
        provider.clone(),
        Arc::new(DesktopVault),
    )
    .await?;
    let check = async {
        let mut app = tokio::process::Command::new("python3");
        app.args(["-c", APP_WRITES])
            .arg(engine.account.mount_path.join("Grüße & Kärnten.txt"))
            .kill_on_drop(true);
        let output = app
            .output()
            .await
            .context("launching the separate write-check application")?;
        anyhow::ensure!(
            output.status.success(),
            "mounted application write failed; fixture and journal retained"
        );
        let saves: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
        anyhow::ensure!(saves.len() == 2, "unexpected application save report");
        event(
            &mut log,
            serde_json::json!({"stage":"local_saves","saves":saves}),
        )?;
        println!("Both application saves completed locally; waiting for cloud acknowledgement.");
        let uploads = loop {
            let rows = session.uploads(0, 100).await?;
            anyhow::ensure!(
                !rows
                    .iter()
                    .any(|r| matches!(r.state, UploadState::Failed | UploadState::Conflict)),
                "mounted upload requires review; local bytes and journal retained"
            );
            if rows.len() == 2 && rows.iter().all(|r| r.state == UploadState::Uploaded) {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        for (record, save) in uploads.iter().zip(&saves) {
            anyhow::ensure!(
                Some(record.size) == save["size"].as_u64()
                    && Some(record.sha256.as_str()) == save["sha256"].as_str(),
                "sealed generation differs from the application bytes"
            );
        }
        let latest = uploads.last().context("missing upload generation")?;
        verify(provider.as_ref(), latest, &cancel).await?;
        event(
            &mut log,
            serde_json::json!({"stage":"readback_verified","generations":uploads,"saves":saves}),
        )?;
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(180), check) => result
            .map_err(|_| anyhow::anyhow!("mounted write check timed out; fixture and local journal retained"))
            .and_then(|r| r),
        _ = tokio::signal::ctrl_c() => Err(anyhow::anyhow!(
            "mounted write check interrupted; fixture and local journal retained"
        )),
    };
    let shutdown = session.shutdown().await;
    event(
        &mut log,
        serde_json::json!({"stage":"finished","passed":result.is_ok()&&shutdown.is_ok(),"shutdown_ok":shutdown.is_ok()}),
    )?;
    result?;
    shutdown?;
    println!(
        "Mounted write check passed: two application generations uploaded, independent cloud readback matched, and the temporary mount stopped. The fixture is retained."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> FixtureGraph {
        FixtureGraph {
            graph: Arc::new(
                OneDrive::new(
                    "test-account".into(),
                    Arc::new(cirrove_onedrive::StaticToken(SecretString::from(
                        "unused-fixture-token",
                    ))),
                )
                .expect("client"),
            ),
            scope: Scope {
                account: "test-account".into(),
                provider: "onedrive".into(),
                collection: "test-drive".into(),
            },
            root: Node {
                id: "new-fixture-root".into(),
                parent_id: Some("drive-root".into()),
                name: "Cirrove-Mounted-Write-Validation-fixture".into(),
                kind: NodeKind::Folder,
                size: 0,
                modified_unix: 0,
                etag: Some("root-tag".into()),
                content_version: None,
                target: None,
            },
            owned: Mutex::new(HashSet::new()),
        }
    }
    #[tokio::test]
    async fn mounted_validator_cannot_address_an_existing_account_file_or_other_scope() {
        let fixture = fixture();
        let request = UploadRequest {
            scope: fixture.scope.clone(),
            intent: UploadIntent::Create {
                parent: fixture.root.id.clone(),
                name: "saved.txt".into(),
            },
            size: 0,
            sha256: format!("{:x}", Sha256::digest([])),
        };
        assert!(fixture.request_allowed(&request).is_ok());
        let mut outside = request.clone();
        outside.scope.collection = "another-drive".into();
        assert!(fixture.request_allowed(&outside).is_err());
        outside = request.clone();
        outside.intent = UploadIntent::Create {
            parent: "drive-root".into(),
            name: "existing-document.txt".into(),
        };
        assert!(fixture.request_allowed(&outside).is_err());
        outside.intent = UploadIntent::Replace {
            item: "existing-company-file".into(),
            expected_etag: "tag".into(),
        };
        assert!(fixture.request_allowed(&outside).is_err());
        let node = Node {
            id: "created-by-this-run".into(),
            name: "saved.txt".into(),
            parent_id: Some(fixture.root.id.clone()),
            kind: NodeKind::File,
            size: 0,
            modified_unix: 0,
            etag: Some("tag".into()),
            content_version: None,
            target: None,
        };
        let mut foreign_receipt = node.clone();
        foreign_receipt.parent_id = Some("drive-root".into());
        assert!(fixture.receipt(&request, &foreign_receipt).is_err());
        assert!(!fixture.item_allowed(&fixture.scope, &node.id));
        fixture.receipt(&request, &node).expect("validated receipt");
        outside.intent = UploadIntent::Replace {
            item: node.id,
            expected_etag: "tag".into(),
        };
        assert!(fixture.request_allowed(&outside).is_ok());
        assert!(matches!(
            fixture
                .node(
                    &fixture.scope,
                    "existing-company-file",
                    &CancellationToken::new()
                )
                .await,
            Err(ProviderError::Permission)
        ));
    }
    #[test]
    fn separate_application_script_reports_fsynced_generations_and_exact_final_bytes() {
        let temp = tempfile::tempdir().expect("fixture");
        let path = temp.path().join("Grüße & Kärnten.txt");
        let output = std::process::Command::new("python3")
            .args(["-c", APP_WRITES])
            .arg(&path)
            .output()
            .expect("python3");
        assert!(output.status.success());
        let rows: Vec<serde_json::Value> =
            serde_json::from_slice(&output.stdout).expect("save reports");
        assert_eq!(rows.len(), 2);
        let bytes = std::fs::read(path).expect("final bytes");
        assert_eq!(bytes.len(), 262177);
        assert!(
            bytes
                .iter()
                .enumerate()
                .all(|(i, b)| *b == ((i * 13 + 34) % 251) as u8)
        );
        assert_eq!(rows[1]["sha256"], format!("{:x}", Sha256::digest(&bytes)));
    }
}
