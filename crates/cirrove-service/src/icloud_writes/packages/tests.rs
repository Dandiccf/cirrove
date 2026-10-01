#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::upload::{PackageSemanticIdentity, PackageUploadReceipt, UploadRepresentation};
use serde_json::json;
fn enqueue(provider: &ICloudWriteProvider, parent: &str) -> (String, UploadRequest) {
    let row = provider
        .journal
        .lock()
        .unwrap()
        .enqueue_package_archive(
            provider.scope.clone(),
            UploadIntent::Create {
                parent: parent.into(),
                name: "Target.pages".into(),
            },
            "Source.pages".into(),
            PackageSemanticIdentity {
                version: 1,
                sha256: "a".repeat(64),
                entries: 1,
                files: 1,
                expanded_bytes: 3,
            },
            &b"zip"[..],
        )
        .unwrap();
    (
        row.id.to_string(),
        UploadRequest {
            scope: row.scope,
            intent: row.intent,
            size: row.size,
            sha256: row.sha256,
            representation: row.representation,
        },
    )
}
fn checkpoint(operation: &str, request: &UploadRequest, parent: &Node) -> SecretString {
    SecretString::from(
        json!({"version":1,"operation":operation,"request":request,"account_hash":"synthetic",
        "parent":parent,"phase":"AllocationArmed","slot":null,"registration":null})
        .to_string(),
    )
}
#[tokio::test]
async fn package_recovery_restores_typed_parent_without_metadata_or_credentials_and_never_reallocates()
 {
    let (_temp, mut provider) = super::super::tests::fixture();
    let parent = super::super::tests::folder(ROOT_ID);
    let (operation, request) = enqueue(&provider, &parent.id);
    let saved = checkpoint(&operation, &request, &parent);
    // A fresh path resolution cannot work; persisted-parent recovery must not
    // open/create this path or ask for a credential just to inspect uncertainty.
    provider.metadata = provider.state.join("missing-metadata-parent/metadata.db");
    assert!(provider.parent(&parent.id).await.is_err());
    let cancel = CancellationToken::new();
    assert!(matches!(
        provider
            .inspect_upload_for_operation(&operation, &request, &saved, &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    assert!(matches!(
        provider
            .reconcile_upload_for_operation(&operation, &request, Some(&saved), &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    assert!(!provider.metadata.exists());
    assert!(
        provider
            .package_adapter(&operation, &request, Some(&saved))
            .await
            .is_ok()
    );
    let (other, same) = enqueue(&provider, &parent.id);
    assert!(matches!(
        provider.package_adapter(&other, &same, Some(&saved)).await,
        Err(UploadError::CheckpointInvalid)
    ));
}
#[tokio::test]
async fn package_router_preserves_large_raw_checkpoint_and_refuses_binding_changes() {
    let (_temp, provider) = super::super::tests::fixture();
    let parent = super::super::tests::folder(ROOT_ID);
    let (operation, request) = enqueue(&provider, &parent.id);
    let mut value: serde_json::Value =
        serde_json::from_str(checkpoint(&operation, &request, &parent).expose_secret()).unwrap();
    value["phase"] = "RegistrationArmed".into();
    value["slot"] = json!({"url":"https://fixture.icloud-content.com/body","document_id":"allocated","owner_id":""});
    value["registration"]=json!({"pkgSignature":"aa","manifest":null,"package":{"sections":[{"receipt":"a".repeat(40*1024)}]}}).to_string().into();
    let saved = SecretString::from(value.to_string());
    assert!(saved.expose_secret().len() > 32 * 1024 && saved.expose_secret().len() < 96 * 1024);
    assert!(
        provider
            .package_adapter(&operation, &request, Some(&saved))
            .await
            .is_ok()
    );
    for change in ["root", "semantic", "account", "size", "name"] {
        let mut changed = request.clone();
        match change {
            "root" => {
                let UploadRepresentation::PackageArchive { expected_root, .. } =
                    &mut changed.representation
                else {
                    panic!("package")
                };
                *expected_root = "Other.pages".into();
            }
            "semantic" => {
                let UploadRepresentation::PackageArchive { semantic, .. } =
                    &mut changed.representation
                else {
                    panic!("package")
                };
                semantic.sha256 = "b".repeat(64);
            }
            "account" => changed.scope.account = Uuid::new_v4().to_string(),
            "size" => changed.size += 1,
            _ => {
                let UploadIntent::Create { name, .. } = &mut changed.intent else {
                    panic!("create")
                };
                *name = "Other.pages".into();
            }
        }
        // Keep the candidate provider checkpoint self-consistent: rejection
        // must come from its disagreement with the immutable journal request.
        let mut changed_checkpoint = value.clone();
        changed_checkpoint["request"] = serde_json::to_value(&changed).unwrap();
        let changed_checkpoint = SecretString::from(changed_checkpoint.to_string());
        assert!(
            provider
                .package_adapter(&operation, &changed, Some(&changed_checkpoint))
                .await
                .is_err()
        );
    }
    assert!(
        provider
            .package_adapter(
                &operation,
                &request,
                Some(&SecretString::from(" ".repeat(96 * 1024 + 1)))
            )
            .await
            .is_err()
    );
}
struct Routed {
    operation: String,
    request: UploadRequest,
    checkpoint: SecretString,
    calls: Mutex<Vec<&'static str>>,
}
impl Routed {
    fn call(
        &self,
        stage: &'static str,
        operation: &str,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
    ) {
        assert_eq!(operation, self.operation);
        assert!(request == &self.request);
        if let Some(saved) = checkpoint {
            assert_eq!(saved.expose_secret(), self.checkpoint.expose_secret());
        }
        self.calls.lock().unwrap().push(stage);
    }
    fn receipt(&self) -> PackageUploadReceipt {
        let UploadIntent::Create { parent, name } = &self.request.intent else {
            panic!("create")
        };
        let UploadRepresentation::PackageArchive { semantic, .. } = &self.request.representation
        else {
            panic!("package")
        };
        PackageUploadReceipt {
            remote: Node {
                id: "FILE::com.apple.CloudDocs::native".into(),
                parent_id: Some(parent.clone()),
                name: name.clone(),
                kind: NodeKind::Folder,
                size: 17,
                modified_unix: 0,
                etag: Some("v1".into()),
                content_version: Some("v1".into()),
                target: None,
                package: true,
            },
            semantic: semantic.clone(),
        }
    }
}
#[async_trait]
impl UploadProvider for Routed {
    async fn begin_upload(&self, _: &UploadRequest, _: &CancellationToken) -> Result<UploadStep> {
        panic!("unbound begin")
    }
    async fn begin_upload_for_operation(
        &self,
        op: &str,
        r: &UploadRequest,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        self.call("begin", op, r, None);
        Ok(UploadStep::Allocate(self.checkpoint.clone()))
    }
    async fn allocate_upload_for_operation(
        &self,
        op: &str,
        r: &UploadRequest,
        c: &SecretString,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        self.call("allocate", op, r, Some(c));
        Ok(UploadStep::Stream(c.clone()))
    }
    async fn inspect_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        panic!("unbound inspect")
    }
    async fn inspect_upload_for_operation(
        &self,
        op: &str,
        r: &UploadRequest,
        c: &SecretString,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        self.call("inspect", op, r, Some(c));
        Err(UploadError::Uncertain)
    }
    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        panic!("multipart")
    }
    async fn upload_stream_for_operation(
        &self,
        op: &str,
        r: &UploadRequest,
        c: &SecretString,
        mut file: File,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        use std::io::Read;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"zip");
        self.call("body", op, r, Some(c));
        Ok(UploadStep::Commit(c.clone()))
    }
    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        panic!("unbound commit")
    }
    async fn commit_upload_for_operation(
        &self,
        op: &str,
        r: &UploadRequest,
        c: &SecretString,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        self.call("commit", op, r, Some(c));
        Ok(UploadStep::PackageComplete(self.receipt()))
    }
    async fn reconcile_upload(
        &self,
        _: &UploadRequest,
        _: Option<&SecretString>,
        _: &CancellationToken,
    ) -> Result<Reconciliation> {
        panic!("unbound reconcile")
    }
    async fn reconcile_upload_for_operation(
        &self,
        op: &str,
        r: &UploadRequest,
        c: Option<&SecretString>,
        _: &CancellationToken,
    ) -> Result<Reconciliation> {
        self.call("reconcile", op, r, c);
        Ok(Reconciliation::PackageCommitted(self.receipt()))
    }
}
#[tokio::test]
async fn package_callbacks_are_operation_bound_and_do_not_enter_file_envelope() {
    use std::io::{Seek, SeekFrom, Write};
    let (_temp, mut provider) = super::super::tests::fixture();
    let (operation, request) = enqueue(&provider, ROOT_ID);
    let raw = SecretString::from("opaque-provider-checkpoint-".repeat(2000));
    let routed = Arc::new(Routed {
        operation: operation.clone(),
        request: request.clone(),
        checkpoint: raw.clone(),
        calls: Mutex::new(Vec::new()),
    });
    provider.package_test_adapter = Some(routed.clone());
    let cancel = CancellationToken::new();
    let UploadStep::Allocate(begin) = provider
        .begin_upload_for_operation(&operation, &request, &cancel)
        .await
        .unwrap()
    else {
        panic!("allocation")
    };
    assert_eq!(begin.expose_secret(), raw.expose_secret());
    let UploadStep::Stream(body) = provider
        .allocate_upload_for_operation(&operation, &request, &raw, &cancel)
        .await
        .unwrap()
    else {
        panic!("stream")
    };
    assert_eq!(body.expose_secret(), raw.expose_secret());
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"zip").unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let UploadStep::Commit(commit) = provider
        .upload_stream_for_operation(&operation, &request, &raw, file, &cancel)
        .await
        .unwrap()
    else {
        panic!("commit")
    };
    assert_eq!(commit.expose_secret(), raw.expose_secret());
    let UploadStep::PackageComplete(receipt) = provider
        .commit_upload_for_operation(&operation, &request, &raw, &cancel)
        .await
        .unwrap()
    else {
        panic!("package receipt")
    };
    assert_eq!(receipt.remote.kind, NodeKind::Folder);
    assert!(receipt.remote.package);
    assert_eq!(receipt.remote.size, 17);
    assert!(matches!(
        provider
            .inspect_upload_for_operation(&operation, &request, &raw, &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    assert!(matches!(
        provider
            .reconcile_upload_for_operation(&operation, &request, Some(&raw), &cancel)
            .await
            .unwrap(),
        Reconciliation::PackageCommitted(_)
    ));
    assert_eq!(
        *routed.calls.lock().unwrap(),
        vec![
            "begin",
            "allocate",
            "body",
            "commit",
            "inspect",
            "reconcile"
        ]
    );
    let mut wrong = request.clone();
    wrong.size += 1;
    assert!(
        provider
            .allocate_upload_for_operation(&operation, &wrong, &raw, &cancel)
            .await
            .is_err()
    );
    assert_eq!(routed.calls.lock().unwrap().len(), 6);
    assert!(
        provider
            .staged_recovery_location(&operation, &request)
            .is_none()
    );
}
