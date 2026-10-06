#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::upload::{PackageSemanticIdentity, PackageUploadReceipt, UploadRepresentation};
use serde_json::json;
use sha2::Digest;
use std::os::unix::fs::PermissionsExt;
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
    async fn begin_upload_from_payload_for_operation(
        &self,
        op: &str,
        r: &UploadRequest,
        mut file: File,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        use std::io::Read;
        self.call("payload-begin", op, r, None);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, r.size);
        assert_eq!(hex::encode(sha2::Sha256::digest(&bytes)), r.sha256);
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
async fn flat_create_and_replacement_payload_begins_forward_exact_operation_request_and_file() {
    for replacement in [false, true] {
        let (temp, mut provider) = super::super::tests::fixture();
        let root = temp.keep();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = root.join("flat.numbers");
        let bytes = crate::native_import::synthetic_package_archive(
            "Index/Document.iwa",
            b"owned Numbers source",
        );
        std::fs::write(&source, &bytes).unwrap();
        let cancel = CancellationToken::new();
        let archive = crate::native_import::ValidatedPackageArchive::capture_with_source_layout(
            &source,
            &root,
            crate::native_import::PackageSourceLayout::FlatNumbers,
            None,
            &cancel,
        )
        .unwrap();
        let row = {
            let mut journal = provider.journal.lock().unwrap();
            if replacement {
                journal
                    .enqueue_validated_package_replacement(
                        provider.scope.clone(),
                        Node {
                            id: "FILE::com.apple.CloudDocs::original".into(),
                            parent_id: Some("FOLDER::com.apple.CloudDocs::owned".into()),
                            name: "Owned.numbers".into(),
                            kind: NodeKind::Folder,
                            size: 123,
                            modified_unix: 0,
                            etag: Some("original-v1".into()),
                            content_version: None,
                            target: None,
                            package: true,
                        },
                        PackageSemanticIdentity {
                            version: 2,
                            sha256: "a".repeat(64),
                            entries: 2,
                            files: 1,
                            expanded_bytes: 123,
                        },
                        archive,
                        &cancel,
                    )
                    .unwrap()
            } else {
                journal
                    .enqueue_validated_package_archive(
                        provider.scope.clone(),
                        UploadIntent::Create {
                            parent: ROOT_ID.into(),
                            name: "Owned.numbers".into(),
                        },
                        archive,
                        &cancel,
                    )
                    .unwrap()
            }
        };
        let operation = row.id.to_string();
        let request = UploadRequest {
            scope: row.scope,
            intent: row.intent,
            representation: row.representation,
            size: row.size,
            sha256: row.sha256,
        };
        let raw = SecretString::from("opaque-flat-checkpoint");
        let routed = Arc::new(Routed {
            operation: operation.clone(),
            request: request.clone(),
            checkpoint: raw.clone(),
            calls: Mutex::new(Vec::new()),
        });
        provider.package_test_adapter = Some(routed.clone());
        assert!(provider.requires_begin_payload(&request));
        let file = provider.journal.lock().unwrap().payload(row.id).unwrap();
        assert!(matches!(
            provider
                .begin_upload_from_payload_for_operation(&operation, &request, file, &cancel)
                .await
                .unwrap(),
            UploadStep::Allocate(_)
        ));
        assert_eq!(*routed.calls.lock().unwrap(), ["payload-begin"]);
        let mut foreign = request.clone();
        foreign.scope.account = Uuid::new_v4().to_string();
        let file = provider.journal.lock().unwrap().payload(row.id).unwrap();
        assert!(matches!(
            provider
                .begin_upload_from_payload_for_operation(&operation, &foreign, file, &cancel)
                .await,
            Err(UploadError::Invalid)
        ));
        assert_eq!(*routed.calls.lock().unwrap(), ["payload-begin"]);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
    }
}
#[tokio::test]
async fn package_callbacks_are_operation_bound_and_do_not_enter_file_envelope() {
    use std::io::{Seek, SeekFrom, Write};
    let (_temp, mut provider) = super::super::tests::fixture();
    let (operation, request) = enqueue(&provider, ROOT_ID);
    assert!(!provider.requires_begin_payload(&request));
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

fn enqueue_native_replacement(
    provider: &ICloudWriteProvider,
    parent: &Node,
) -> (String, UploadRequest) {
    use std::io::Write;
    let staging = provider
        .state
        .join(format!("native-router-staging-{}", Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .unwrap();
    let source = staging.join("source.zip");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .unwrap();
    file.write_all(&crate::native_import::synthetic_package_archive(
        "Source.pages/Document",
        b"edited owned native bytes",
    ))
    .unwrap();
    file.sync_all().unwrap();
    let cancel = CancellationToken::new();
    let archive = crate::native_import::ValidatedPackageArchive::capture(
        &source,
        &staging,
        "Source.pages",
        &cancel,
    )
    .unwrap();
    let original = Node {
        id: format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
        parent_id: Some(parent.id.clone()),
        name: "Original.pages".into(),
        kind: NodeKind::Folder,
        size: 123,
        modified_unix: 0,
        etag: Some("selected-v1".into()),
        content_version: None,
        target: None,
        package: true,
    };
    let row = provider
        .journal
        .lock()
        .unwrap()
        .enqueue_validated_package_replacement(
            provider.scope.clone(),
            original,
            PackageSemanticIdentity {
                version: 1,
                sha256: "a".repeat(64),
                entries: 1,
                files: 1,
                expanded_bytes: 123,
            },
            archive,
            &cancel,
        )
        .unwrap();
    (
        row.id.to_string(),
        UploadRequest {
            scope: row.scope,
            intent: row.intent,
            representation: row.representation,
            size: row.size,
            sha256: row.sha256,
        },
    )
}
fn native_checkpoint(
    provider: &ICloudWriteProvider,
    operation: &str,
    request: &UploadRequest,
    parent: &Node,
) -> SecretString {
    use sha2::{Digest, Sha256};
    let UploadRepresentation::PackageReplacementArchive {
        expected_root,
        semantic,
        ..
    } = &request.representation
    else {
        panic!("native replacement")
    };
    let stage = UploadRequest {
        scope: request.scope.clone(),
        intent: UploadIntent::Create {
            parent: parent.id.clone(),
            name: format!("staged-by-cirrove-{operation}.pages"),
        },
        representation: UploadRepresentation::PackageArchive {
            expected_root: expected_root.clone(),
            semantic: semantic.clone(),
        },
        size: request.size,
        sha256: request.sha256.clone(),
    };
    let account_hash = hex::encode(Sha256::digest(
        provider.apple_id.trim().to_lowercase().as_bytes(),
    ));
    SecretString::from(json!({"version":1,"operation":operation,"request":request,"parent":parent,"account_hash":account_hash,"phase":{"Stage":{"inner":{"version":1,"operation":operation,"request":stage,"parent":parent,"account_hash":account_hash,"phase":"AllocationArmed","slot":null,"registration":null}}}}).to_string())
}
#[tokio::test]
async fn native_replacement_router_restores_without_metadata_or_credentials_and_refuses_rebinding()
{
    let (_temp, mut provider) = super::super::tests::fixture();
    let parent = super::super::tests::folder(ROOT_ID);
    let (operation, request) = enqueue_native_replacement(&provider, &parent);
    let saved = native_checkpoint(&provider, &operation, &request, &parent);
    provider.metadata = provider.state.join("missing-metadata-parent/metadata.db");
    assert!(
        provider
            .package_adapter(&operation, &request, None)
            .await
            .is_err()
    );
    assert!(
        provider
            .package_adapter(&operation, &request, Some(&saved))
            .await
            .is_ok()
    );
    // The recorded allocation arm has no returned slot. Inspect/reconcile must
    // return uncertainty without loading a keyring or repeating allocation.
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
            .package_adapter(&Uuid::new_v4().to_string(), &request, Some(&saved))
            .await
            .is_err()
    );
    for change in [
        "account",
        "original_semantic",
        "replacement_semantic",
        "original_revision",
        "raw_hash",
    ] {
        let mut changed = request.clone();
        match change {
            "account" => changed.scope.account = Uuid::new_v4().to_string(),
            "raw_hash" => changed.sha256 = "f".repeat(64),
            _ => {
                let UploadRepresentation::PackageReplacementArchive {
                    semantic,
                    original_semantic,
                    original,
                    ..
                } = &mut changed.representation
                else {
                    panic!("native")
                };
                match change {
                    "original_semantic" => original_semantic.sha256 = "b".repeat(64),
                    "replacement_semantic" => semantic.sha256 = "c".repeat(64),
                    _ => original.etag = Some("other".into()),
                }
            }
        }
        assert!(
            provider
                .package_adapter(&operation, &changed, Some(&saved))
                .await
                .is_err(),
            "{change}"
        );
        // A forged envelope cannot relabel the exact journal request either.
        let mut wire: serde_json::Value = serde_json::from_str(saved.expose_secret()).unwrap();
        wire["request"] = serde_json::to_value(changed).unwrap();
        assert!(
            provider
                .package_adapter(
                    &operation,
                    &request,
                    Some(&SecretString::from(wire.to_string()))
                )
                .await
                .is_err(),
            "{change}"
        );
    }
    for nested in ["operation", "semantic", "parent"] {
        let mut wire: serde_json::Value = serde_json::from_str(saved.expose_secret()).unwrap();
        let inner = &mut wire["phase"]["Stage"]["inner"];
        match nested {
            "operation" => inner["operation"] = json!(Uuid::new_v4()),
            "semantic" => {
                inner["request"]["representation"]["semantic"]["sha256"] = json!("d".repeat(64))
            }
            _ => inner["parent"]["id"] = json!("FOLDER::com.apple.CloudDocs::other"),
        }
        let forged = SecretString::from(wire.to_string());
        assert!(
            matches!(
                provider
                    .inspect_upload_for_operation(&operation, &request, &forged, &cancel)
                    .await,
                Err(UploadError::CheckpointInvalid)
            ),
            "nested {nested}"
        );
    }
    let mut wire: serde_json::Value = serde_json::from_str(saved.expose_secret()).unwrap();
    wire["account_hash"] = json!("0".repeat(64));
    assert!(
        provider
            .package_adapter(
                &operation,
                &request,
                Some(&SecretString::from(wire.to_string()))
            )
            .await
            .is_err()
    );
}
#[tokio::test]
async fn native_replacement_fresh_router_requires_plain_scoped_parent_ancestry() {
    let (_temp, provider) = super::super::tests::fixture();
    let parent = super::super::tests::folder(ROOT_ID);
    let (operation, request) = enqueue_native_replacement(&provider, &parent);
    let mut store = Store::open(&provider.metadata).unwrap();
    let root = Node {
        id: ROOT_ID.into(),
        parent_id: None,
        ..super::super::tests::folder(ROOT_ID)
    };
    store.observe_node(&provider.scope, &root).unwrap();
    // No parent indexed yet: construction cannot guess it from the request.
    assert!(
        provider
            .package_adapter(&operation, &request, None)
            .await
            .is_err()
    );
    store.observe_node(&provider.scope, &parent).unwrap();
    assert!(
        provider
            .package_adapter(&operation, &request, None)
            .await
            .is_ok()
    );
    for variant in ["app", "file", "alias", "ancestor"] {
        let mut changed = parent.clone();
        let mut ancestor = root.clone();
        match variant {
            "app" => changed.package = true,
            "file" => changed.kind = NodeKind::File,
            "alias" => {
                changed.target = Some(Box::new(cirrove_core::RemoteRef {
                    collection: "foreign".into(),
                    item: "foreign".into(),
                    kind: Some(NodeKind::Folder),
                }))
            }
            _ => {
                ancestor.id = "FOLDER::com.apple.CloudDocs::package-ancestor".into();
                ancestor.parent_id = Some(ROOT_ID.into());
                ancestor.package = true;
                changed.parent_id = Some(ancestor.id.clone());
            }
        }
        store.observe_node(&provider.scope, &changed).unwrap();
        store.observe_node(&provider.scope, &ancestor).unwrap();
        assert!(
            provider
                .package_adapter(&operation, &request, None)
                .await
                .is_err(),
            "{variant}"
        );
    }
    store.observe_node(&provider.scope, &parent).unwrap();
    store.observe_node(&provider.scope, &root).unwrap();
    assert!(
        provider
            .package_adapter(&operation, &request, None)
            .await
            .is_ok()
    );
}
#[tokio::test]
async fn native_replacement_router_reserves_pages_recovery_and_only_safe_begin() {
    use cirrove_core::upload::RecoveryLocation;
    let (_temp, provider) = super::super::tests::fixture();
    let parent = super::super::tests::folder(ROOT_ID);
    let (operation, request) = enqueue_native_replacement(&provider, &parent);
    assert!(provider.begin_is_mutation_free_until_checkpoint(&request));
    assert!(
        matches!(provider.staged_recovery_location(&operation,&request),Some(RecoveryLocation::Trash{local_name,parent}) if local_name==format!("recovery-by-cirrove-{operation}.pages") && parent=="FOLDER::com.apple.CloudDocs::TRASH_ROOT")
    );
    assert!(
        provider
            .staged_recovery_location(&Uuid::new_v4().to_string(), &request)
            .is_none()
    );
    let mut ordinary = request.clone();
    ordinary.representation = UploadRepresentation::FileBytes;
    assert!(!provider.begin_is_mutation_free_until_checkpoint(&ordinary));
    assert!(
        provider
            .staged_recovery_location(&operation, &ordinary)
            .is_none()
    );
    let (create, request) = enqueue(&provider, ROOT_ID);
    assert!(provider.begin_is_mutation_free_until_checkpoint(&request));
    assert!(
        provider
            .staged_recovery_location(&create, &request)
            .is_none()
    );
}
