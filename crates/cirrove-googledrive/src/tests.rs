#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::{
    CancellationToken, Checkpoint, Cursor, MetadataProvider, ReadProvider, StaticToken,
    reads::ReadWindowSink,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[derive(Clone)]
struct Step {
    path: &'static str,
    query: Vec<(&'static str, &'static str)>,
    status: u16,
    body: Vec<u8>,
    headers: &'static str,
}
fn json_step(path: &'static str, body: Value) -> Step {
    Step {
        path,
        query: vec![],
        status: 200,
        body: serde_json::to_vec(&body).unwrap(),
        headers: "",
    }
}
fn completed_download(mime: &'static str, body: &[u8]) -> Vec<Step> {
    vec![
        Step {
            path: "/drive/v3/files/same-id/download",
            query: vec![("mimeType", mime)],
            status: 200,
            body: serde_json::to_vec(&json!({
                "name":"operations/synthetic",
                "done":true,
                "response":{"downloadUri":"__ENDPOINT__fixture-download"}
            }))
            .unwrap(),
            headers: "",
        },
        Step {
            path: "/drive/v3/fixture-download",
            query: vec![],
            status: 200,
            body: body.to_vec(),
            headers: "",
        },
    ]
}
fn file(version: &str) -> Value {
    json!({"id":"same-id","name":"report.txt","mimeType":"text/plain","parents":["root-id"],"size":"6","version":version,"headRevisionId":format!("revision-{version}"),"capabilities":{"canDownload":true}})
}
fn scope() -> Scope {
    Scope {
        account: "account".into(),
        provider: PROVIDER_ID.into(),
        collection: "root-id".into(),
    }
}
async fn server(steps: Vec<Step>) -> (GoogleDrive, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Url::parse(&format!(
        "http://{}/drive/v3/",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let fixture_endpoint = endpoint.to_string();
    let task = tokio::spawn(async move {
        for step in steps {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 2048];
                let n = socket.read(&mut bytes).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&bytes[..n]);
                assert!(request.len() < 65536);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            let expected_method = if step.path.ends_with("/download") {
                "POST "
            } else {
                "GET "
            };
            assert!(request.starts_with(expected_method));
            let url = Url::parse(&format!(
                "http://fixture{}",
                request.split_ascii_whitespace().nth(1).unwrap()
            ))
            .unwrap();
            assert_eq!(url.path(), step.path);
            let has_auth = request
                .to_ascii_lowercase()
                .contains("authorization: bearer synthetic-token");
            if url.path() == "/drive/v3/redirect-target" {
                assert!(!has_auth);
            } else {
                assert!(has_auth);
            }
            let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
            for (key, value) in step.query {
                assert_eq!(query.get(key).map(|v| v.as_ref()), Some(value));
            }
            if step.status == 206 {
                assert!(request.to_ascii_lowercase().contains("range: bytes=1-3"));
            }
            let body = if step
                .body
                .windows(b"__ENDPOINT__".len())
                .any(|window| window == b"__ENDPOINT__")
            {
                String::from_utf8(step.body)
                    .unwrap()
                    .replace("__ENDPOINT__", &fixture_endpoint)
                    .into_bytes()
            } else {
                step.body
            };
            let reply = format!(
                "HTTP/1.1 {} Synthetic\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                step.status,
                body.len(),
                step.headers.replace("__ENDPOINT__", &fixture_endpoint)
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    (
        GoogleDrive::build(
            "account".into(),
            "root-id".into(),
            Arc::new(StaticToken(secrecy::SecretString::from("synthetic-token"))),
            endpoint,
        )
        .unwrap(),
        task,
    )
}

fn shared_collection() -> CollectionInfo {
    CollectionInfo {
        id: "shared-root".into(),
        name: "Team".into(),
        drive_type: "shared_drive".into(),
        web_url: String::new(),
    }
}

fn shared_scope() -> Scope {
    Scope {
        collection: "shared-root".into(),
        ..scope()
    }
}

fn shared_file(version: &str) -> Value {
    let mut value = file(version);
    value["parents"] = json!(["shared-root"]);
    value["driveId"] = json!("shared-root");
    value
}

#[tokio::test]
async fn lists_shared_drives_with_pagination_after_my_drive() {
    let (provider, task) = server(vec![
        json_step(
            "/drive/v3/files/root",
            json!({"id":"my-root","name":"My Drive","mimeType":"application/vnd.google-apps.folder","version":"1"}),
        ),
        json_step(
            "/drive/v3/drives",
            json!({"drives":[{"id":"shared-root","name":"Team"}],"nextPageToken":"page-2"}),
        ),
        Step {
            path: "/drive/v3/drives",
            query: vec![("pageToken", "page-2")],
            status: 200,
            body: serde_json::to_vec(&json!({"drives":[{"id":"other-root","name":"Other"}]})).unwrap(),
            headers: "",
        },
    ])
    .await;
    let collections = provider
        .collections(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(collections.len(), 3);
    assert_eq!(collections[0].drive_type, "my_drive");
    assert_eq!(collections[1].drive_type, "shared_drive");
    task.await.unwrap();
}

#[tokio::test]
async fn shared_drive_uses_its_own_feed_listing_and_content_scope() {
    let mut start = json_step(
        "/drive/v3/changes/startPageToken",
        json!({"startPageToken":"start-shared"}),
    );
    start.query = vec![("driveId", "shared-root"), ("supportsAllDrives", "true")];
    let mut listing = json_step("/drive/v3/files", json!({"files":[shared_file("1")]}));
    listing.query = vec![
        ("corpora", "drive"),
        ("driveId", "shared-root"),
        ("includeItemsFromAllDrives", "true"),
        ("supportsAllDrives", "true"),
    ];
    let mut changes = json_step(
        "/drive/v3/changes",
        json!({"changes":[],"newStartPageToken":"next-shared"}),
    );
    changes.query = vec![("driveId", "shared-root"), ("supportsAllDrives", "true")];
    let mut metadata = json_step("/drive/v3/files/same-id", shared_file("1"));
    metadata.query = vec![("supportsAllDrives", "true")];
    let mut root = json_step(
        "/drive/v3/files/shared-root",
        json!({
            "id":"shared-root","name":"Team","mimeType":"application/vnd.google-apps.folder",
            "version":"1","driveId":"shared-root"
        }),
    );
    root.query = vec![("supportsAllDrives", "true")];
    let (provider, task) = server(vec![start, listing, root, changes, metadata]).await;
    let provider = provider.for_collection(&shared_collection()).unwrap();
    let cancel = CancellationToken::new();
    let first = provider
        .changes(&shared_scope(), None, &cancel)
        .await
        .unwrap();
    assert!(
        matches!(&first.changes[0], cirrove_core::Change::Upsert(node) if node.parent_id.as_deref() == Some("shared-root"))
    );
    assert!(
        matches!(&first.changes[1], cirrove_core::Change::Upsert(node) if node.id == "shared-root")
    );
    let second = provider
        .changes(&shared_scope(), Some(first.checkpoint.cursor()), &cancel)
        .await
        .unwrap();
    assert!(second.checkpoint.complete());
    let node = provider
        .node(&shared_scope(), "same-id", &cancel)
        .await
        .unwrap();
    assert_eq!(node.size, 6);
    task.await.unwrap();
    assert!(matches!(
        provider.node(&scope(), "same-id", &cancel).await,
        Err(ProviderError::Permission)
    ));
}

#[tokio::test]
async fn baseline_frontier_precedes_scan_and_catchup_precedes_completion() {
    let mut second = json_step("/drive/v3/files", json!({"files":[]}));
    second.query.push(("pageToken", "listing-2"));
    let mut catchup = json_step(
        "/drive/v3/changes",
        json!({"changes":[{"changeType":"drive","driveId":"other-shared","removed":false},{"changeType":"file","fileId":"same-id","file":file("2")}],"newStartPageToken":"settled"}),
    );
    catchup.query.push(("pageToken", "before-scan"));
    let (p, task) = server(vec![
        json_step(
            "/drive/v3/changes/startPageToken",
            json!({"startPageToken":"before-scan"}),
        ),
        json_step(
            "/drive/v3/files",
            json!({"files":[file("1")],"nextPageToken":"listing-2"}),
        ),
        second,
        catchup,
    ])
    .await;
    let cancel = CancellationToken::new();
    let first = p.changes(&scope(), None, &cancel).await.unwrap();
    assert!(!first.checkpoint.complete());
    assert_eq!(first.changes.len(), 1);
    let second = p
        .changes(&scope(), Some(first.checkpoint.cursor()), &cancel)
        .await
        .unwrap();
    assert!(
        !second.checkpoint.complete(),
        "end of listing is not end of baseline"
    );
    let last = p
        .changes(&scope(), Some(second.checkpoint.cursor()), &cancel)
        .await
        .unwrap();
    assert!(last.checkpoint.complete());
    assert_eq!(
        last.changes.len(),
        1,
        "My Drive ignores Shared Drive events"
    );
    assert!(
        matches!(&last.changes[0], cirrove_core::Change::Upsert(n) if n.content_version.as_deref() == Some("google-revision:revision-2"))
    );
    assert!(!format!("{:?}", last.checkpoint).contains("settled"));
    task.await.unwrap();
}
#[tokio::test]
async fn scopes_and_continuations_cannot_cross_accounts_collections_or_directories() {
    let (p, task) = server(vec![json_step(
        "/drive/v3/files",
        json!({"files":[],"nextPageToken":"opaque-private"}),
    )])
    .await;
    let cancel = CancellationToken::new();
    let page = p
        .children(&scope(), "root-id", None, &cancel)
        .await
        .unwrap();
    task.await.unwrap();
    assert!(matches!(
        p.children(&scope(), "another-parent", page.next.as_ref(), &cancel)
            .await,
        Err(ProviderError::Permission)
    ));
    for foreign in [
        Scope {
            account: "other".into(),
            ..scope()
        },
        Scope {
            provider: "onedrive".into(),
            ..scope()
        },
        Scope {
            collection: "other".into(),
            ..scope()
        },
    ] {
        assert!(matches!(
            p.changes(&foreign, None, &cancel).await,
            Err(ProviderError::Permission)
        ));
        assert!(matches!(
            p.node(&foreign, "same-id", &cancel).await,
            Err(ProviderError::Permission)
        ));
    }
    assert!(
        p.changes(
            &scope(),
            Some(&Cursor("https://evil.invalid/steal".into())),
            &cancel
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn reads_require_exact_ranges_and_unchanged_monotonic_versions() {
    for after in ["1", "2"] {
        let (p, task) = server(vec![
            json_step("/drive/v3/files/same-id", file("1")),
            Step {
                path: "/drive/v3/files/same-id",
                query: vec![("alt", "media")],
                status: 206,
                body: b"bcd".to_vec(),
                headers: "Content-Range: bytes 1-3/6\r\n",
            },
            json_step("/drive/v3/files/same-id", file(after)),
        ])
        .await;
        let node = serde_json::from_value::<files::File>(file("1"))
            .unwrap()
            .node("root-id")
            .unwrap();
        let result = p
            .read_range(&scope(), &node, 1, 3, &CancellationToken::new())
            .await;
        if after == "1" {
            assert_eq!(result.unwrap(), b"bcd");
        } else {
            assert!(matches!(result, Err(ProviderError::VersionChanged)));
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn metadata_version_changes_do_not_replace_the_binary_content_revision() {
    let mut after = file("2");
    after["name"] = json!("renamed.txt");
    after["headRevisionId"] = json!("revision-1");
    let (provider, task) = server(vec![
        json_step("/drive/v3/files/same-id", file("1")),
        Step {
            path: "/drive/v3/files/same-id",
            query: vec![("alt", "media")],
            status: 206,
            body: b"bcd".to_vec(),
            headers: "Content-Range: bytes 1-3/6\r\n",
        },
        json_step("/drive/v3/files/same-id", after),
    ])
    .await;
    let node = serde_json::from_value::<files::File>(file("1"))
        .unwrap()
        .node("root-id")
        .unwrap();
    assert_eq!(
        provider
            .read_range(&scope(), &node, 1, 3, &CancellationToken::new())
            .await
            .unwrap(),
        b"bcd"
    );
    task.await.unwrap();
}

#[derive(Default)]
struct Sink(Vec<u8>);

#[async_trait::async_trait]
impl ReadWindowSink for Sink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

#[tokio::test]
async fn read_sessions_stream_a_window_and_reject_a_changed_final_version() {
    for after in ["1", "2"] {
        let (provider, task) = server(vec![
            json_step("/drive/v3/files/same-id", file("1")),
            Step {
                path: "/drive/v3/files/same-id",
                query: vec![("alt", "media")],
                status: 206,
                body: b"bcd".to_vec(),
                headers: "Content-Range: bytes 1-3/6\r\n",
            },
            json_step("/drive/v3/files/same-id", file(after)),
        ])
        .await;
        let node = serde_json::from_value::<files::File>(file("1"))
            .unwrap()
            .node("root-id")
            .unwrap();
        let session = provider
            .open_read_session(&scope(), &node, &CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.window_limit(), 8 * 1024 * 1024);
        let mut sink = Sink::default();
        let result = session
            .read_window(1, 3, &mut sink, &CancellationToken::new())
            .await;
        assert_eq!(sink.0, b"bcd");
        if after == "1" {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(ProviderError::VersionChanged)));
        }
        task.await.unwrap();
    }
}
#[tokio::test]
async fn mismatched_range_truncated_body_and_ignored_range_are_never_returned() {
    for (status, body, headers) in [
        (206, b"bcd".to_vec(), "Content-Range: bytes 0-2/6\r\n"),
        (206, b"bc".to_vec(), "Content-Range: bytes 1-3/6\r\n"),
        (200, b"abcdef".to_vec(), ""),
    ] {
        let (p, task) = server(vec![
            json_step("/drive/v3/files/same-id", file("1")),
            Step {
                path: "/drive/v3/files/same-id",
                query: vec![],
                status,
                body,
                headers,
            },
        ])
        .await;
        let node = serde_json::from_value::<files::File>(file("1"))
            .unwrap()
            .node("root-id")
            .unwrap();
        assert!(matches!(
            p.read_range(&scope(), &node, 1, 3, &CancellationToken::new())
                .await,
            Err(ProviderError::Protocol(_))
        ));
        task.await.unwrap();
    }
}
#[tokio::test]
async fn quota_403_sets_account_wide_cooldown_without_exposing_provider_body() {
    let mut step = json_step(
        "/drive/v3/files/same-id",
        json!({"error":{"message":"private-provider-body","errors":[{"reason":"userRateLimitExceeded"}]}}),
    );
    step.status = 403;
    let (p, task) = server(vec![step]).await;
    let cancel = CancellationToken::new();
    let error = p.node(&scope(), "same-id", &cancel).await.unwrap_err();
    assert!(matches!(error, ProviderError::Throttled(_)));
    assert!(!error.to_string().contains("private-provider-body"));
    task.await.unwrap();
    assert!(matches!(
        p.changes(&scope(), None, &cancel).await,
        Err(ProviderError::Throttled(_))
    ));
}
#[tokio::test]
async fn native_documents_expand_to_exact_versioned_exports_and_a_browser_link() {
    let mut doc = file("1");
    doc["name"] = "Quarterly plan".into();
    doc["mimeType"] = "application/vnd.google-apps.document".into();
    doc["size"] = "777".into();
    doc.as_object_mut().unwrap().remove("headRevisionId");
    let node = serde_json::from_value::<files::File>(doc.clone())
        .unwrap()
        .node("root-id")
        .unwrap();
    assert_eq!(node.kind, cirrove_core::NodeKind::Folder);
    assert!(node.package);
    assert!(node.name.ends_with(".gdoc"));
    assert_eq!(node.size, 0);

    let exported = b"synthetic-docx-content".to_vec();
    let mut steps = Vec::new();
    for (mime, bytes) in [
        (
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            exported.as_slice(),
        ),
        ("application/pdf", b"synthetic-pdf-content".as_slice()),
        (
            "application/vnd.oasis.opendocument.text",
            b"synthetic-odt-content".as_slice(),
        ),
    ] {
        steps.push(json_step("/drive/v3/files/same-id", doc.clone()));
        steps.extend(completed_download(mime, bytes));
        steps.push(json_step("/drive/v3/files/same-id", doc.clone()));
    }
    let (p, task) = server(steps).await;
    let page = p
        .children_for_node(&scope(), &node, None, &CancellationToken::new())
        .await
        .unwrap();
    assert!(page.next.is_none());
    assert_eq!(page.nodes.len(), 4);
    let folder = page
        .nodes
        .iter()
        .find(|child| child.name == "DOCX")
        .unwrap();
    let export_page = p
        .children_for_node(&scope(), folder, None, &CancellationToken::new())
        .await
        .unwrap();
    let export = &export_page.nodes[0];
    assert_eq!(export.name, "Document.docx");
    assert_eq!(export.size, exported.len() as u64);
    assert_eq!(export.parent_id.as_deref(), Some(folder.id.as_str()));
    assert!(
        export
            .content_version
            .as_deref()
            .unwrap()
            .starts_with("google-export-docx:1:")
    );
    let content = p
        .read_range(
            &scope(),
            export,
            1,
            exported.len() as u32,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(content, exported[1..]);
    for (name, expected) in [
        ("PDF", b"synthetic-pdf-content".as_slice()),
        ("ODT", b"synthetic-odt-content".as_slice()),
    ] {
        let folder = page.nodes.iter().find(|child| child.name == name).unwrap();
        let selected = p
            .children_for_node(&scope(), folder, None, &CancellationToken::new())
            .await
            .unwrap();
        let child = &selected.nodes[0];
        assert_ne!(child.id, export.id);
        assert_eq!(child.parent_id.as_deref(), Some(folder.id.as_str()));
        let staged = p
            .staged_content(&scope(), child, &CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(staged.as_ref(), expected);
        assert_eq!(
            p.read_range(&scope(), child, 0, 4096, &CancellationToken::new())
                .await
                .unwrap(),
            expected
        );
    }

    let link = page
        .nodes
        .iter()
        .find(|child| child.name == "Open in Google.url")
        .unwrap();
    let content = p
        .read_range(&scope(), link, 0, 4096, &CancellationToken::new())
        .await
        .unwrap();
    assert!(
        String::from_utf8(content)
            .unwrap()
            .contains("https://drive.google.com/open?id=same-id")
    );
    let repeated = p
        .children_for_node(&scope(), &node, None, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(repeated.nodes, page.nodes);
    task.await.unwrap();
}

#[tokio::test]
async fn native_package_listing_defers_exports_until_one_format_is_opened() {
    let mut doc = file("1");
    doc["mimeType"] = "application/vnd.google-apps.document".into();
    doc.as_object_mut().unwrap().remove("headRevisionId");
    let parent = serde_json::from_value::<files::File>(doc.clone())
        .unwrap()
        .node("root-id")
        .unwrap();
    let bytes = b"selected-docx-only".to_vec();
    // Exactly one format is requested. A slow or oversized PDF must not hold
    // the source package's directory listing or the selected DOCX open.
    let (provider, task) = server(
        [
            vec![json_step("/drive/v3/files/same-id", doc.clone())],
            completed_download(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                &bytes,
            ),
            vec![json_step("/drive/v3/files/same-id", doc)],
        ]
        .concat(),
    )
    .await;
    let top = provider
        .children_for_node(&scope(), &parent, None, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(top.nodes.len(), 4);
    let names: Vec<_> = top.nodes.iter().map(|child| child.name.as_str()).collect();
    assert_eq!(names, ["DOCX", "PDF", "ODT", "Open in Google.url"]);
    let selected = &top.nodes[0];
    assert_eq!(selected.kind, cirrove_core::NodeKind::Folder);
    assert!(selected.package);
    assert_eq!(selected.parent_id.as_deref(), Some("same-id"));
    assert_eq!(
        provider.directory_fetch_timeout(Some(&parent)),
        Duration::from_secs(60)
    );
    assert_eq!(
        provider.directory_fetch_timeout(Some(selected)),
        Duration::from_secs(360)
    );
    let page = provider
        .children_for_node(&scope(), selected, None, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(page.nodes.len(), 1);
    let export = &page.nodes[0];
    assert_eq!(export.name, "Document.docx");
    assert_eq!(export.parent_id.as_deref(), Some(selected.id.as_str()));
    assert_eq!(export.size, bytes.len() as u64);
    assert_eq!(
        provider
            .staged_content(&scope(), export, &CancellationToken::new())
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        bytes.as_slice()
    );
    task.await.unwrap();
}

#[tokio::test]
async fn native_download_strips_bearer_on_redirect() {
    let mut doc = file("1");
    doc["mimeType"] = "application/vnd.google-apps.document".into();
    doc.as_object_mut().unwrap().remove("headRevisionId");
    let parent = serde_json::from_value::<files::File>(doc.clone())
        .unwrap()
        .node("root-id")
        .unwrap();
    let mut steps = vec![json_step("/drive/v3/files/same-id", doc.clone())];
    steps.extend(completed_download(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        b"ignored",
    ));
    steps[2].status = 302;
    steps[2].body.clear();
    steps[2].headers = "Location: __ENDPOINT__redirect-target\r\n";
    steps.push(Step {
        path: "/drive/v3/redirect-target",
        query: vec![],
        status: 200,
        body: b"redirected-docx".to_vec(),
        headers: "",
    });
    steps.push(json_step("/drive/v3/files/same-id", doc));
    let (provider, task) = server(steps).await;
    let top = provider
        .children_for_node(&scope(), &parent, None, &CancellationToken::new())
        .await
        .unwrap();
    let page = provider
        .children_for_node(&scope(), &top.nodes[0], None, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(page.nodes[0].size, b"redirected-docx".len() as u64);
    task.await.unwrap();
}

#[tokio::test]
async fn native_download_refuses_a_non_google_uri() {
    let mut doc = file("1");
    doc["mimeType"] = "application/vnd.google-apps.document".into();
    doc.as_object_mut().unwrap().remove("headRevisionId");
    let parent = serde_json::from_value::<files::File>(doc.clone())
        .unwrap()
        .node("root-id")
        .unwrap();
    let (provider, task) = server(vec![
        json_step("/drive/v3/files/same-id", doc),
        json_step(
            "/drive/v3/files/same-id/download",
            json!({"name":"operations/synthetic","done":true,"response":{"downloadUri":"https://example.invalid/download"}}),
        ),
    ])
    .await;
    let top = provider
        .children_for_node(&scope(), &parent, None, &CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        provider
            .children_for_node(&scope(), &top.nodes[0], None, &CancellationToken::new())
            .await,
        Err(ProviderError::Permission)
    ));
    task.await.unwrap();
}

#[tokio::test]
async fn native_download_polls_bounded_operation_before_publication() {
    let mut sheet = file("3");
    sheet["mimeType"] = "application/vnd.google-apps.spreadsheet".into();
    sheet.as_object_mut().unwrap().remove("headRevisionId");
    let parent = serde_json::from_value::<files::File>(sheet.clone())
        .unwrap()
        .node("root-id")
        .unwrap();
    let (provider, task) = server(vec![
        json_step("/drive/v3/files/same-id", sheet.clone()),
        json_step(
            "/drive/v3/files/same-id/download",
            json!({"name":"operations/synthetic","done":false}),
        ),
        json_step(
            "/drive/v3/operations/synthetic",
            json!({"name":"operations/synthetic","done":true,"response":{"downloadUri":"__ENDPOINT__fixture-download"}}),
        ),
        Step {
            path: "/drive/v3/fixture-download",
            query: vec![],
            status: 200,
            body: b"polled-xlsx".to_vec(),
            headers: "",
        },
        json_step("/drive/v3/files/same-id", sheet),
    ])
    .await;
    let top = provider
        .children_for_node(&scope(), &parent, None, &CancellationToken::new())
        .await
        .unwrap();
    let page = provider
        .children_for_node(&scope(), &top.nodes[0], None, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(page.nodes[0].size, b"polled-xlsx".len() as u64);
    task.await.unwrap();
}

#[tokio::test]
async fn native_spreadsheets_expose_distinct_pdf_and_ods_bytes() {
    let mut sheet = file("1");
    sheet["mimeType"] = "application/vnd.google-apps.spreadsheet".into();
    sheet.as_object_mut().unwrap().remove("headRevisionId");
    let parent = serde_json::from_value::<files::File>(sheet.clone())
        .unwrap()
        .node("root-id")
        .unwrap();
    let mut steps = Vec::new();
    for (mime, content) in [
        (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            b"synthetic-xlsx".as_slice(),
        ),
        ("application/pdf", b"synthetic-sheet-pdf".as_slice()),
        (
            "application/vnd.oasis.opendocument.spreadsheet",
            b"synthetic-ods".as_slice(),
        ),
    ] {
        steps.push(json_step("/drive/v3/files/same-id", sheet.clone()));
        steps.extend(completed_download(mime, content));
        steps.push(json_step("/drive/v3/files/same-id", sheet.clone()));
    }
    let (provider, task) = server(steps).await;
    let page = provider
        .children_for_node(&scope(), &parent, None, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(page.nodes.len(), 4);
    for (name, expected, output_name) in [
        ("XLSX", b"synthetic-xlsx".as_slice(), "Spreadsheet.xlsx"),
        ("PDF", b"synthetic-sheet-pdf".as_slice(), "Spreadsheet.pdf"),
        ("ODS", b"synthetic-ods".as_slice(), "Spreadsheet.ods"),
    ] {
        let folder = page.nodes.iter().find(|child| child.name == name).unwrap();
        let selected = provider
            .children_for_node(&scope(), folder, None, &CancellationToken::new())
            .await
            .unwrap();
        let child = &selected.nodes[0];
        assert_eq!(child.name, output_name);
        assert_eq!(child.size, expected.len() as u64);
        let staged = provider
            .staged_content(&scope(), child, &CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(staged.as_ref(), expected);
        assert_eq!(
            provider
                .read_range(&scope(), child, 0, 4096, &CancellationToken::new())
                .await
                .unwrap(),
            expected
        );
    }
    assert_eq!(page.nodes[0].name, "XLSX");
    assert_eq!(page.nodes[1].name, "PDF");
    assert_eq!(page.nodes[2].name, "ODS");
    assert_ne!(page.nodes[0].id, page.nodes[1].id);
    assert_ne!(page.nodes[1].id, page.nodes[2].id);
    task.await.unwrap();
}

#[tokio::test]
async fn native_export_is_discarded_when_the_source_changes_during_materialization() {
    let mut before = file("1");
    before["mimeType"] = "application/vnd.google-apps.spreadsheet".into();
    before.as_object_mut().unwrap().remove("headRevisionId");
    let mut after = before.clone();
    after["version"] = "2".into();
    let parent = serde_json::from_value::<files::File>(before.clone())
        .unwrap()
        .node("root-id")
        .unwrap();
    let (provider, task) = server(
        [
            vec![json_step("/drive/v3/files/same-id", before)],
            completed_download(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                b"stale-xlsx",
            ),
            vec![json_step("/drive/v3/files/same-id", after)],
        ]
        .concat(),
    )
    .await;
    let page = provider
        .children_for_node(&scope(), &parent, None, &CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        provider
            .children_for_node(&scope(), &page.nodes[0], None, &CancellationToken::new())
            .await,
        Err(ProviderError::VersionChanged)
    ));
    task.await.unwrap();
}
#[tokio::test]
async fn native_export_preflight_compares_selected_bytes_without_exposing_identity() {
    let mut doc = file("7");
    doc["name"] = "private-title".into();
    doc["mimeType"] = "application/vnd.google-apps.document".into();
    doc["size"] = "777".into();
    doc.as_object_mut().unwrap().remove("headRevisionId");
    let exported = b"synthetic-docx".to_vec();
    let steps = [
        vec![
            json_step(
                "/drive/v3/files",
                json!({"files":[doc.clone()],"incompleteSearch":false}),
            ),
            json_step(
                "/drive/v3/files/same-id/revisions",
                json!({"revisions":[{"id":"revision-private"}]}),
            ),
            json_step("/drive/v3/files/same-id", doc.clone()),
        ],
        completed_download(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            &exported,
        ),
        vec![json_step("/drive/v3/files/same-id", doc)],
    ]
    .concat();
    let (provider, task) = server(steps).await;
    let report = provider
        .native_export_preflight(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.scanned_files, 1);
    assert_eq!(report.observations.len(), 1);
    let observed = &report.observations[0];
    assert_eq!(observed.kind, "document");
    assert_eq!(observed.metadata_size, Some(777));
    assert_eq!(observed.export_bytes, exported.len() as u64);
    assert!(!observed.metadata_matches_export);
    assert!(observed.version_stable);
    assert!(!observed.head_revision_available);
    assert!(observed.listed_revision_available);
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains("same-id") && !serialized.contains("private-title"));
    task.await.unwrap();
}
#[test]
fn duplicate_and_posix_invalid_names_remain_distinct_stable_and_bounded() {
    let mut a = file("1");
    a["name"] = "a/b%\u{0}.txt".into();
    let mut b = a.clone();
    b["id"] = "other-id".into();
    let a = serde_json::from_value::<files::File>(a)
        .unwrap()
        .node("root-id")
        .unwrap();
    let b = serde_json::from_value::<files::File>(b)
        .unwrap()
        .node("root-id")
        .unwrap();
    assert_ne!(a.name, b.name);
    assert!(a.name.ends_with(".txt"));
    assert!(!a.name.contains('/') && !a.name.contains('\0'));
    let mut long = file("1");
    long["name"] = "ä".repeat(500).into();
    assert!(
        serde_json::from_value::<files::File>(long)
            .unwrap()
            .node("root-id")
            .unwrap()
            .name
            .len()
            <= 255
    );
}
#[tokio::test]
async fn incomplete_search_and_ambiguous_checkpoints_do_not_publish_success() {
    let (p, task) = server(vec![
        json_step(
            "/drive/v3/changes/startPageToken",
            json!({"startPageToken":"start"}),
        ),
        json_step(
            "/drive/v3/files",
            json!({"files":[],"incompleteSearch":true}),
        ),
    ])
    .await;
    assert!(
        p.changes(&scope(), None, &CancellationToken::new())
            .await
            .is_err()
    );
    task.await.unwrap();
    let (p, task) = server(vec![json_step(
        "/drive/v3/changes",
        json!({"changes":[],"nextPageToken":"next","newStartPageToken":"last"}),
    )])
    .await;
    let cursor =
        Cursor(json!({"scope":scope(),"phase":{"phase":"changes","page":"start"}}).to_string());
    assert!(
        p.changes(&scope(), Some(&cursor), &CancellationToken::new())
            .await
            .is_err()
    );
    task.await.unwrap();
}
#[tokio::test]
async fn removed_and_trashed_items_translate_to_deletions() {
    let mut trashed = file("2");
    trashed["trashed"] = true.into();
    let (p,task) = server(vec![json_step("/drive/v3/changes",json!({"changes":[{"fileId":"gone","removed":true},{"fileId":"same-id","file":trashed}],"newStartPageToken":"last"}))]).await;
    let cursor =
        Cursor(json!({"scope":scope(),"phase":{"phase":"changes","page":"start"}}).to_string());
    let result = p
        .changes(&scope(), Some(&cursor), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        result.changes,
        vec![
            cirrove_core::Change::Delete { id: "gone".into() },
            cirrove_core::Change::Delete {
                id: "same-id".into()
            }
        ]
    );
    assert!(matches!(result.checkpoint, Checkpoint::Complete(_)));
    task.await.unwrap();
}
#[tokio::test]
async fn cancellation_interrupts_an_inflight_http_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = GoogleDrive::build(
        "account".into(),
        "root-id".into(),
        Arc::new(StaticToken(secrecy::SecretString::from("synthetic-token"))),
        Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap(),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let request_cancel = cancel.clone();
    let request = tokio::spawn(async move { p.node(&scope(), "same-id", &request_cancel).await });
    let (_socket, _) = listener.accept().await.unwrap();
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .unwrap()
            .unwrap(),
        Err(ProviderError::Cancelled)
    ));
}

#[tokio::test]
async fn rejected_listing_token_requests_a_new_baseline_and_redirects_are_not_followed() {
    let mut rejected = json_step(
        "/drive/v3/files",
        json!({"error":{"message":"opaque-private-token"}}),
    );
    rejected.status = 400;
    let (p, task) = server(vec![rejected]).await;
    let cursor = Cursor(
        json!({"scope":scope(),"phase":{"phase":"listing","start":"start","page":"expired"}})
            .to_string(),
    );
    assert!(matches!(
        p.changes(&scope(), Some(&cursor), &CancellationToken::new())
            .await,
        Err(ProviderError::CursorExpired)
    ));
    task.await.unwrap();
    let mut redirect = json_step("/drive/v3/files/same-id", json!({}));
    redirect.status = 302;
    redirect.headers = "Location: https://evil.invalid/steal\r\n";
    let (p, task) = server(vec![redirect]).await;
    assert!(matches!(
        p.node(&scope(), "same-id", &CancellationToken::new()).await,
        Err(ProviderError::Protocol(_))
    ));
    task.await.unwrap();
}
