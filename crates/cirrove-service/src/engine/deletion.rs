//! Permanent deletion is the one operation with no way back, so what it refuses
//! matters more than what it does (ADR 0008).
#![allow(clippy::unwrap_used)]
use super::discovery::fixture_account;
use super::*;
use cirrove_core::mutation::{
    DeletionSupport, MutationError, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest,
};
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, NodeKind,
    ProviderError,
};
use std::sync::atomic::{AtomicBool, Ordering};

/// A drive with one folder and one file in it, and nothing else.
#[derive(Default)]
struct SmallDrive;

impl SmallDrive {
    fn folder(id: &str, name: &str, parent: Option<&str>) -> Node {
        Node {
            package: false,
            id: id.into(),
            parent_id: parent.map(ToOwned::to_owned),
            name: name.into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("1".into()),
            content_version: None,
            target: None,
        }
    }
    fn file(id: &str, name: &str, parent: &str) -> Node {
        Node {
            package: false,
            id: id.into(),
            parent_id: Some(parent.into()),
            name: name.into(),
            kind: NodeKind::File,
            size: 9,
            modified_unix: 0,
            etag: Some("file-etag".into()),
            content_version: Some("1".into()),
            target: None,
        }
    }
}

#[async_trait::async_trait]
impl MetadataProvider for SmallDrive {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        Ok(ChangePage {
            changes: vec![
                Change::Upsert(Self::folder("root", "Primary", None)),
                Change::Upsert(Self::folder("papers", "Papers", Some("root"))),
                Change::Upsert(Self::file("note", "Note.txt", "root")),
            ],
            checkpoint: Checkpoint::Complete(Cursor("done".into())),
        })
    }
}

#[async_trait::async_trait]
impl ReadProvider for SmallDrive {
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        match id {
            "root" => Ok(Self::folder("root", "Primary", None)),
            "papers" => Ok(Self::folder("papers", "Papers", Some("root"))),
            "note" => Ok(Self::file("note", "Note.txt", "root")),
            _ => Err(ProviderError::NotFound),
        }
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        Ok(DirectoryPage {
            nodes: vec![],
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
        Err(ProviderError::Permission)
    }
}

/// Records what it was asked to destroy, and says what it supports.
struct Destroyer {
    support: DeletionSupport,
    asked: std::sync::Mutex<Vec<(String, Option<String>)>>,
    fail: AtomicBool,
}

impl Destroyer {
    fn able() -> Self {
        Self {
            support: DeletionSupport {
                recycle_bin: true,
                permanent: true,
            },
            asked: std::sync::Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
        }
    }
    fn unable() -> Self {
        Self {
            support: DeletionSupport::default(),
            ..Self::able()
        }
    }
    fn asked(&self) -> Vec<(String, Option<String>)> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl MutationProvider for Destroyer {
    fn deletion(&self) -> DeletionSupport {
        self.support
    }
    async fn delete_permanently(
        &self,
        _: &Scope,
        item: &str,
        etag: Option<&str>,
        _: &CancellationToken,
    ) -> std::result::Result<(), MutationError> {
        self.asked
            .lock()
            .unwrap()
            .push((item.to_owned(), etag.map(ToOwned::to_owned)));
        if self.fail.load(Ordering::SeqCst) {
            return Err(MutationError::Conflict);
        }
        Ok(())
    }
    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> std::result::Result<MutationReceipt, MutationError> {
        Err(MutationError::Invalid)
    }
    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> std::result::Result<MutationReconciliation, MutationError> {
        Err(MutationError::Invalid)
    }
}

async fn drive() -> (tempfile::TempDir, Arc<Engine>) {
    let temp = tempfile::tempdir().unwrap();
    let account = fixture_account(temp.path().join("mount"));
    let engine = Engine::new(account, Arc::new(SmallDrive), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    for _ in 0..200 {
        if engine.health().await.iter().any(|h| h.state == "ready") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    (temp, engine)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_is_destroyed_with_the_version_that_was_looked_at() {
    let (_temp, engine) = drive().await;
    let provider = Destroyer::able();
    let done = engine
        .delete_permanently(&["Note.txt".into()], &provider)
        .await;
    engine.stop().await;
    assert_eq!(done.len(), 1);
    assert!(done[0].removed, "{:?}", done[0].refusal);
    assert_eq!(
        provider.asked(),
        vec![("note".to_owned(), Some("file-etag".to_owned()))],
        "the eTag goes with it, or a person destroys a version they never saw"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_is_refused_and_the_provider_is_never_asked() {
    let (_temp, engine) = drive().await;
    let provider = Destroyer::able();
    let done = engine
        .delete_permanently(&["Papers".into()], &provider)
        .await;
    engine.stop().await;
    assert!(!done[0].removed);
    let refusal = done[0].refusal.clone().unwrap();
    assert!(refusal.contains("recursive"), "{refusal}");
    assert!(
        provider.asked().is_empty(),
        "a folder must be refused here, not attempted and regretted: the delete is \
         recursive and the recycle bin is the only recovery from that"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_provider_that_cannot_do_it_gets_no_ordinary_delete_instead() {
    let (_temp, engine) = drive().await;
    let provider = Destroyer::unable();
    let done = engine
        .delete_permanently(&["Note.txt".into()], &provider)
        .await;
    engine.stop().await;
    assert!(!done[0].removed);
    assert!(
        provider.asked().is_empty(),
        "silence is not consent: a provider that has not said it can delete \
         permanently must not have an ordinary delete substituted for one that \
         was asked for by name"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_that_is_not_there_is_refused_by_name() {
    let (_temp, engine) = drive().await;
    let provider = Destroyer::able();
    let done = engine
        .delete_permanently(&["Note.txt".into(), "Missing.txt".into()], &provider)
        .await;
    engine.stop().await;
    // Every path gets its own answer, and one bad path does not cancel the rest.
    assert_eq!(done.len(), 2);
    assert!(done[0].removed);
    assert_eq!(done[1].path, "Missing.txt");
    assert!(!done[1].removed);
    assert!(done[1].refusal.is_some());
}
