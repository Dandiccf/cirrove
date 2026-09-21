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
            assert!(request.starts_with("GET "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer synthetic-token")
            );
            let url = Url::parse(&format!(
                "http://fixture{}",
                request.split_ascii_whitespace().nth(1).unwrap()
            ))
            .unwrap();
            assert_eq!(url.path(), step.path);
            let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
            for (key, value) in step.query {
                assert_eq!(query.get(key).map(|v| v.as_ref()), Some(value));
            }
            if step.status == 206 {
                assert!(request.to_ascii_lowercase().contains("range: bytes=1-3"));
            }
            let reply = format!(
                "HTTP/1.1 {} Synthetic\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                step.status,
                step.body.len(),
                step.headers
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            socket.write_all(&step.body).await.unwrap();
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

#[tokio::test]
async fn baseline_frontier_precedes_scan_and_catchup_precedes_completion() {
    let mut second = json_step("/drive/v3/files", json!({"files":[]}));
    second.query.push(("pageToken", "listing-2"));
    let mut catchup = json_step(
        "/drive/v3/changes",
        json!({"changes":[{"fileId":"same-id","file":file("2")}],"newStartPageToken":"settled"}),
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
    let (p, task) = server(vec![
        json_step("/drive/v3/files/same-id", doc.clone()),
        Step {
            path: "/drive/v3/files/same-id/export",
            query: vec![(
                "mimeType",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            )],
            status: 200,
            body: exported.clone(),
            headers: "",
        },
        json_step("/drive/v3/files/same-id", doc),
    ])
    .await;
    let page = p
        .children_for_node(&scope(), &node, None, &CancellationToken::new())
        .await
        .unwrap();
    assert!(page.next.is_none());
    assert_eq!(page.nodes.len(), 2);
    let export = page
        .nodes
        .iter()
        .find(|child| child.name == "Document.docx")
        .unwrap();
    assert_eq!(export.size, exported.len() as u64);
    assert_eq!(export.parent_id.as_deref(), Some("same-id"));
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
    let (provider, task) = server(vec![
        json_step("/drive/v3/files/same-id", before),
        Step {
            path: "/drive/v3/files/same-id/export",
            query: vec![(
                "mimeType",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            )],
            status: 200,
            body: b"stale-xlsx".to_vec(),
            headers: "",
        },
        json_step("/drive/v3/files/same-id", after),
    ])
    .await;
    assert!(matches!(
        provider
            .children_for_node(&scope(), &parent, None, &CancellationToken::new())
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
    let steps = vec![
        json_step(
            "/drive/v3/files",
            json!({"files":[doc.clone()],"incompleteSearch":false}),
        ),
        json_step(
            "/drive/v3/files/same-id/revisions",
            json!({"revisions":[{"id":"revision-private"}]}),
        ),
        json_step("/drive/v3/files/same-id", doc.clone()),
        Step {
            path: "/drive/v3/files/same-id/export",
            query: vec![(
                "mimeType",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            )],
            status: 200,
            body: exported.clone(),
            headers: "",
        },
        json_step("/drive/v3/files/same-id", doc),
    ];
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
