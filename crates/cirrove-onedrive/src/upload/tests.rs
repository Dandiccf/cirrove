#![allow(clippy::unwrap_used)]
use super::*;
use crate::StaticToken;
use cirrove_core::{MetadataProvider, Scope};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct Request {
    head: String,
    body: Vec<u8>,
}
struct Reply {
    status: u16,
    body: String,
    headers: String,
}
fn reply(status: u16, body: impl Into<String>) -> Reply {
    Reply {
        status,
        body: body.into(),
        headers: String::new(),
    }
}
async fn fixture(replies: Vec<Reply>) -> (OneDrive, tokio::task::JoinHandle<Vec<Request>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    let provider = OneDrive::build(
        "fixture".into(),
        Arc::new(StaticToken(SecretString::from("fake-graph-bearer"))),
        Url::parse(&format!("{host}/v1.0/")).unwrap(),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        let mut requests = vec![];
        for reply in replies {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = vec![];
            let split = loop {
                if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    break i + 4;
                }
                let mut buffer = [0; 8192];
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                assert!(bytes.len() < 65536);
            };
            let head = String::from_utf8(bytes[..split].to_vec()).unwrap();
            let length = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .map(|v| v.parse::<usize>().unwrap())
                .unwrap_or(0);
            let mut body = bytes[split..].to_vec();
            assert!(length <= PART_SIZE as usize + 65536);
            while body.len() < length {
                let mut buffer = [0; 65536];
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                body.extend_from_slice(&buffer[..n]);
            }
            assert_eq!(body.len(), length);
            requests.push(Request { head, body });
            if reply.status == 0 {
                continue; // Intentionally lose the response after receiving the request.
            }
            let body = reply.body.replace("HOST", &host);
            let response = format!(
                "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                reply.status,
                body.len(),
                reply.headers,
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
        requests
    });
    (provider, task)
}

fn mutation_node() -> Node {
    Node {
        id: "file".into(),
        parent_id: Some("source".into()),
        name: "old.txt".into(),
        kind: NodeKind::File,
        size: 5,
        modified_unix: 0,
        etag: Some("\"original\"".into()),
        content_version: Some("content".into()),
        target: None,
    }
}
fn mutation_request(remove: bool) -> cirrove_core::mutation::MutationRequest {
    use cirrove_core::mutation::*;
    MutationRequest {
        scope: Scope {
            account: "fixture".into(),
            provider: "onedrive".into(),
            collection: "drive".into(),
        },
        intent: if remove {
            MutationIntent::RemoveFile {
                before: mutation_node(),
            }
        } else {
            MutationIntent::Relocate {
                before: mutation_node(),
                parent: "destination".into(),
                name: "Grüße & #1.txt".into(),
            }
        },
    }
}
fn mutation_wire(parent: &str, name: &str, etag: &str) -> String {
    json!({"id":"file","name":name,"size":5,"eTag":etag,"cTag":"content","file":{},"parentReference":{"driveId":"drive","id":parent}}).to_string()
}
#[tokio::test]
async fn namespace_reconciliation_rejects_foreign_item_or_drive_evidence() {
    use cirrove_core::mutation::*;
    for foreign_drive in [false, true] {
        let mut body: serde_json::Value =
            serde_json::from_str(&mutation_wire("source", "old.txt", "\"original\"")).unwrap();
        if foreign_drive {
            body["parentReference"]["driveId"] = "different-drive".into();
        } else {
            body["id"] = "different-item".into();
        }
        let (graph, task) = fixture(vec![reply(200, body.to_string())]).await;
        assert!(matches!(
            graph
                .reconcile_mutation(&mutation_request(false), &CancellationToken::new())
                .await,
            Err(MutationError::Provider(ProviderError::Protocol(_)))
        ));
        task.await.unwrap();
    }
}
#[tokio::test]
async fn namespace_patch_is_conditional_and_fails_on_collision() {
    use cirrove_core::mutation::*;
    let request = mutation_request(false);
    let (graph, task) = fixture(vec![reply(
        200,
        mutation_wire("destination", "Grüße & #1.txt", "new"),
    )])
    .await;
    let receipt = graph
        .mutate(&request, &CancellationToken::new())
        .await
        .unwrap();
    assert!(request.accepts(&receipt));
    let requests = task.await.unwrap();
    assert!(
        requests[0]
            .head
            .starts_with("PATCH /v1.0/drives/drive/items/file ")
    );
    assert!(
        requests[0]
            .head
            .to_lowercase()
            .contains("if-match: \"original\"")
    );
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["name"], "Grüße & #1.txt");
    assert_eq!(body["parentReference"]["id"], "destination");
    assert_eq!(body["@microsoft.graph.conflictBehavior"], "fail");
    for status in [409, 412] {
        let (graph, task) = fixture(vec![reply(status, "PRIVATE-PROVIDER-BODY")]).await;
        assert!(matches!(
            graph.mutate(&request, &CancellationToken::new()).await,
            Err(MutationError::Conflict)
        ));
        assert_eq!(task.await.unwrap().len(), 1);
    }
}
#[tokio::test]
async fn namespace_lost_reply_is_reconciled_by_identity_and_location() {
    use cirrove_core::mutation::*;
    let request = mutation_request(false);
    let (graph, task) = fixture(vec![
        reply(0, ""),
        reply(200, mutation_wire("destination", "Grüße & #1.txt", "new")),
    ])
    .await;
    assert!(matches!(
        graph.mutate(&request, &CancellationToken::new()).await,
        Err(MutationError::Uncertain)
    ));
    assert!(matches!(
        graph
            .reconcile_mutation(&request, &CancellationToken::new())
            .await
            .unwrap(),
        MutationReconciliation::Applied(_)
    ));
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].head.starts_with("GET "));
    for (etag, expected) in [("\"original\"", true), ("changed", false)] {
        let (graph, task) =
            fixture(vec![reply(200, mutation_wire("source", "old.txt", etag))]).await;
        let state = graph
            .reconcile_mutation(&request, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            matches!(state, MutationReconciliation::Uncommitted),
            expected
        );
        if !expected {
            assert!(matches!(state, MutationReconciliation::Conflict));
        }
        task.await.unwrap();
    }
}
#[tokio::test]
async fn file_delete_checks_remote_type_and_requires_original_etag() {
    use cirrove_core::mutation::*;
    let request = mutation_request(true);
    let (graph, task) = fixture(vec![
        reply(200, mutation_wire("source", "old.txt", "\"original\"")),
        reply(204, ""),
    ])
    .await;
    assert!(
        request.accepts(
            &graph
                .mutate(&request, &CancellationToken::new())
                .await
                .unwrap()
        )
    );
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .head
            .starts_with("DELETE /v1.0/drives/drive/items/file ")
    );
    assert!(
        requests[1]
            .head
            .to_lowercase()
            .contains("if-match: \"original\"")
    );
    assert!(!requests[1].head.to_lowercase().contains("bypass"));
    let mut folder: serde_json::Value =
        serde_json::from_str(&mutation_wire("source", "old.txt", "\"original\"")).unwrap();
    folder.as_object_mut().unwrap().remove("file");
    folder["folder"] = json!({});
    let (graph, task) = fixture(vec![reply(200, folder.to_string())]).await;
    assert!(matches!(
        graph.mutate(&request, &CancellationToken::new()).await,
        Err(MutationError::Conflict)
    ));
    assert_eq!(task.await.unwrap().len(), 1);
    let (graph, task) = fixture(vec![
        reply(200, mutation_wire("source", "old.txt", "\"original\"")),
        reply(412, ""),
    ])
    .await;
    assert!(matches!(
        graph.mutate(&request, &CancellationToken::new()).await,
        Err(MutationError::Conflict)
    ));
    task.await.unwrap();
}
#[tokio::test]
async fn missing_item_and_unidentified_folder_creation_do_not_confirm_success() {
    use cirrove_core::mutation::*;
    let request = mutation_request(true);
    let (graph, task) = fixture(vec![reply(404, "")]).await;
    assert!(matches!(
        graph
            .reconcile_mutation(&request, &CancellationToken::new())
            .await
            .unwrap(),
        MutationReconciliation::Indeterminate
    ));
    task.await.unwrap();
    let mut create = request.clone();
    create.intent = MutationIntent::CreateFolder {
        parent: "source".into(),
        name: "folder".into(),
    };
    assert!(matches!(
        graph
            .reconcile_mutation(&create, &CancellationToken::new())
            .await
            .unwrap(),
        MutationReconciliation::Indeterminate
    ));
    let mut wrong = request;
    wrong.scope.account = "other".into();
    assert!(matches!(
        graph.mutate(&wrong, &CancellationToken::new()).await,
        Err(MutationError::Invalid)
    ));
}
fn spec(data: &[u8], replace: bool) -> UploadRequest {
    UploadRequest {
        scope: Scope {
            account: "fixture".into(),
            provider: "onedrive".into(),
            collection: "drive".into(),
        },
        intent: if replace {
            UploadIntent::Replace {
                item: "existing".into(),
                expected_etag: "\"original\"".into(),
            }
        } else {
            UploadIntent::Create {
                parent: "root".into(),
                name: "Grüße &#x3e;.txt".into(),
            }
        },
        size: data.len() as u64,
        sha256: format!("{:x}", Sha256::digest(data)),
    }
}

#[tokio::test]
async fn folder_creation_fails_on_collision_and_checks_scope_and_receipt() {
    let body = json!({"id":"new-folder", "name":"Kärnten #1", "folder":{}, "size":0,
        "parentReference":{"id":"root", "driveId":"drive"}})
    .to_string();
    let (graph, server) = fixture(vec![
        reply(201, body),
        reply(409, "private provider details"),
    ])
    .await;
    let scope = spec(b"", false).scope;
    let cancel = CancellationToken::new();
    let node = graph
        .create_folder(&scope, "root", "Kärnten #1", &cancel)
        .await
        .unwrap();
    assert_eq!(node.id, "new-folder");
    assert!(matches!(
        graph
            .create_folder(&scope, "root", "Kärnten #1", &cancel)
            .await,
        Err(UploadError::Conflict)
    ));
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(
            request
                .head
                .starts_with("POST /v1.0/drives/drive/items/root/children ")
        );
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["@microsoft.graph.conflictBehavior"], "fail");
        assert_eq!(body["name"], "Kärnten #1");
    }
    let (graph, server) = fixture(vec![reply(
        201,
        json!({"id":"foreign", "name":"Fixture", "folder":{},
        "parentReference":{"id":"root", "driveId":"wrong-drive"}})
        .to_string(),
    )])
    .await;
    assert!(matches!(
        graph
            .create_folder(&scope, "root", "Fixture", &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    server.await.unwrap();
    let mut foreign = scope;
    foreign.account = "another-account".into();
    assert!(matches!(
        graph
            .create_folder(&foreign, "root", "Fixture", &cancel)
            .await,
        Err(UploadError::Invalid)
    ));
}
fn session(ranges: Option<Vec<String>>, include_url: bool) -> String {
    let mut value =
        json!({"expirationDateTime":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()});
    if include_url {
        value["uploadUrl"] = json!("HOST/session/PRIVATE-CHECKPOINT");
    }
    if let Some(ranges) = ranges {
        value["nextExpectedRanges"] = json!(ranges);
    }
    value.to_string()
}
fn node(request: &UploadRequest, etag: &str) -> serde_json::Value {
    let (id, name) = match &request.intent {
        UploadIntent::Create { name, .. } => ("new-id", name.as_str()),
        UploadIntent::Replace { item, .. } => (item.as_str(), "Saved.txt"),
    };
    json!({"id":id,"name":name,"size":request.size,"file":{},"eTag":etag,"cTag":"content-v2","parentReference":{"id":"root","driveId":"drive"},"@microsoft.graph.downloadUrl":"HOST/download"})
}
fn more(step: UploadStep) -> UploadProgress {
    match step {
        UploadStep::Continue(p) => p,
        _ => panic!("expected transfer range"),
    }
}

#[tokio::test]
async fn create_upload_sends_exact_ranges_without_bearer_on_session_requests() {
    let data = vec![42; PART_SIZE as usize + 13];
    let request = spec(&data, false);
    let (p, server) = fixture(vec![
        reply(200, session(None, true)),
        reply(202, session(Some(vec![format!("{PART_SIZE}-")]), false)),
        reply(201, node(&request, "new-tag").to_string()),
    ])
    .await;
    let cancel = CancellationToken::new();
    let first = p.begin_upload(&request, &cancel).await.unwrap();
    assert!(!format!("{first:?}").contains("PRIVATE-CHECKPOINT"));
    let first = more(first);
    assert_eq!((first.offset, first.length), (0, PART_SIZE));
    let next = more(
        p.upload_part(
            &request,
            &first.checkpoint,
            0,
            data[..PART_SIZE as usize].to_vec(),
            &cancel,
        )
        .await
        .unwrap(),
    );
    assert_eq!((next.offset, next.length), (u64::from(PART_SIZE), 13));
    assert!(matches!(
        p.upload_part(
            &request,
            &next.checkpoint,
            next.offset,
            data[PART_SIZE as usize..].to_vec(),
            &cancel
        )
        .await
        .unwrap(),
        UploadStep::Complete(_)
    ));
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].head.starts_with(
        "POST /v1.0/drives/drive/items/root:/Gr%C3%BC%C3%9Fe%20&%23x3e;.txt:/createUploadSession "
    ));
    assert!(
        requests[0]
            .head
            .to_lowercase()
            .contains("authorization: bearer fake-graph-bearer")
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&requests[0].body).unwrap()["item"]["@microsoft.graph.conflictBehavior"],
        "fail"
    );
    for req in &requests[1..] {
        assert!(!req.head.to_lowercase().contains("authorization:"));
    }
    assert!(requests[1].body == data[..PART_SIZE as usize]);
    assert!(requests[2].body == data[PART_SIZE as usize..]);
    assert!(requests[2].head.to_lowercase().contains(&format!(
        "content-range: bytes {PART_SIZE}-{}/{}",
        data.len() - 1,
        data.len()
    )));
}

#[tokio::test]
async fn replacements_stage_bytes_then_condition_the_final_commit_and_preserve_conflicts() {
    for conflict in [false, true] {
        let request = spec(b"replacement", true);
        let (p, server) = fixture(vec![
            reply(200, node(&request, "\"original\"").to_string()),
            reply(200, session(None, true)),
            reply(202, session(Some(vec![]), false)),
            reply(
                200,
                r#"{"id":"drive","name":"Fixture","driveType":"personal"}"#,
            ),
            reply(
                if conflict { 412 } else { 200 },
                node(&request, "new-tag").to_string(),
            ),
        ])
        .await;
        let cancel = CancellationToken::new();
        let first = more(p.begin_upload(&request, &cancel).await.unwrap());
        let step = p
            .upload_part(
                &request,
                &first.checkpoint,
                0,
                b"replacement".to_vec(),
                &cancel,
            )
            .await
            .unwrap();
        let UploadStep::Commit(checkpoint) = step else {
            panic!("replacement must require a separate commit")
        };
        let result = p.commit_upload(&request, &checkpoint, &cancel).await;
        if conflict {
            assert!(matches!(result, Err(UploadError::Conflict)));
        } else {
            assert!(matches!(result, Ok(UploadStep::Complete(_))));
        }
        let requests = server.await.unwrap();
        assert!(
            requests[1]
                .head
                .to_lowercase()
                .contains("if-match: \"original\"")
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&requests[1].body).unwrap()["deferCommit"],
            true
        );
        assert!(!requests[2].head.to_lowercase().contains("authorization:"));
        assert!(
            requests[4]
                .head
                .starts_with("PUT /v1.0/drives/drive/items/root:/Saved.txt ")
        );
        assert!(
            requests[4]
                .head
                .to_lowercase()
                .contains("if-match: \"original\"")
        );
        assert!(serde_json::from_slice::<serde_json::Value>(&requests[4].body).unwrap()["@microsoft.graph.sourceUrl"].as_str().unwrap().ends_with("/PRIVATE-CHECKPOINT"));
    }
}

#[tokio::test]
async fn business_commit_uses_zero_length_session_post_with_precondition_and_no_bearer() {
    for kind in ["business", "documentLibrary"] {
        for conflict in [false, true] {
            let request = spec(b"replacement", true);
            let (p, server) = fixture(vec![
                reply(200, node(&request, "\"original\"").to_string()),
                reply(200, session(None, true)),
                reply(202, session(Some(vec![]), false)),
                reply(
                    200,
                    json!({"id":"drive", "name":"Fixture", "driveType":kind}).to_string(),
                ),
                reply(
                    if conflict { 412 } else { 200 },
                    node(&request, "new-tag").to_string(),
                ),
            ])
            .await;
            let cancel = CancellationToken::new();
            let first = more(p.begin_upload(&request, &cancel).await.unwrap());
            let UploadStep::Commit(checkpoint) = p
                .upload_part(
                    &request,
                    &first.checkpoint,
                    0,
                    b"replacement".to_vec(),
                    &cancel,
                )
                .await
                .unwrap()
            else {
                panic!("expected staged commit");
            };
            let result = p.commit_upload(&request, &checkpoint, &cancel).await;
            if conflict {
                assert!(matches!(result, Err(UploadError::Conflict)));
            } else {
                assert!(matches!(result, Ok(UploadStep::Complete(_))));
            }
            let requests = server.await.unwrap();
            let commit = &requests[4];
            assert!(commit.head.starts_with("POST /session/PRIVATE-CHECKPOINT "));
            assert!(commit.head.to_lowercase().contains("content-length: 0\r\n"));
            assert!(
                commit
                    .head
                    .to_lowercase()
                    .contains("if-match: \"original\"\r\n")
            );
            assert!(!commit.head.to_lowercase().contains("authorization:"));
            assert!(commit.body.is_empty());
        }
    }
}

#[tokio::test]
async fn zero_byte_upload_uses_a_conditional_single_request() {
    for replace in [false, true] {
        let request = spec(b"", replace);
        let (p, server) = fixture(vec![reply(
            if replace { 412 } else { 201 },
            node(&request, "new-tag").to_string(),
        )])
        .await;
        let result = p.begin_upload(&request, &CancellationToken::new()).await;
        if replace {
            assert!(matches!(result, Err(UploadError::Conflict)));
        } else {
            assert!(matches!(result, Ok(UploadStep::Complete(_))));
        }
        let requests = server.await.unwrap();
        assert!(requests[0].body.is_empty());
        assert!(
            requests[0]
                .head
                .to_lowercase()
                .contains("content-length: 0\r\n")
        );
        assert!(!requests[0].head.to_lowercase().contains("content-range:"));
        if replace {
            assert!(
                requests[0]
                    .head
                    .to_lowercase()
                    .contains("if-match: \"original\"")
            );
        } else {
            assert!(
                requests[0]
                    .head
                    .contains("%40microsoft.graph.conflictBehavior=fail")
            );
        }
    }
}

#[tokio::test]
async fn upload_throttling_applies_to_metadata_and_never_exposes_response_details() {
    let request = spec(b"abc", false);
    let mut throttled = reply(429, "PRIVATE-PROVIDER-BODY");
    throttled.headers = "Retry-After: 60\r\n".into();
    let (p, server) = fixture(vec![reply(200, session(None, true)), throttled]).await;
    let cancel = CancellationToken::new();
    let first = more(p.begin_upload(&request, &cancel).await.unwrap());
    let error = p
        .upload_part(&request, &first.checkpoint, 0, b"abc".to_vec(), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        UploadError::Provider(ProviderError::Throttled(_))
    ));
    assert!(!format!("{error:?}").contains("PRIVATE"));
    assert!(matches!(
        p.changes(&request.scope, None, &cancel).await,
        Err(ProviderError::Throttled(_))
    ));
    assert_eq!(server.await.unwrap().len(), 2);
}

#[tokio::test]
async fn upload_redirects_are_not_followed_and_missing_sessions_are_uncertain() {
    let trap = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let request = spec(b"abc", false);
    let mut redirect = reply(307, "");
    redirect.headers = format!("Location: http://{}/leak\r\n", trap.local_addr().unwrap());
    let (p, server) = fixture(vec![
        reply(200, session(None, true)),
        redirect,
        reply(404, "PRIVATE-PROVIDER-BODY"),
    ])
    .await;
    let cancel = CancellationToken::new();
    let first = more(p.begin_upload(&request, &cancel).await.unwrap());
    assert!(matches!(
        p.upload_part(&request, &first.checkpoint, 0, b"abc".to_vec(), &cancel)
            .await,
        Err(UploadError::UnexpectedStatus(307))
    ));
    assert!(matches!(
        p.inspect_upload(&request, &first.checkpoint, &cancel).await,
        Err(UploadError::SessionGone)
    ));
    assert_eq!(server.await.unwrap().len(), 3);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), trap.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn checkpoints_bind_account_target_and_bytes_and_validate_expiry_before_network() {
    let request = spec(b"abc", false);
    let (p, server) = fixture(vec![reply(200, session(None, true))]).await;
    let cancel = CancellationToken::new();
    let first = more(p.begin_upload(&request, &cancel).await.unwrap());
    let mut different = request.clone();
    different.scope.collection = "other-drive".into();
    assert!(matches!(
        p.inspect_upload(&different, &first.checkpoint, &cancel)
            .await,
        Err(UploadError::CheckpointInvalid)
    ));
    for invalid in ["", "{", "{}"] {
        assert!(matches!(
            p.inspect_upload(&request, &SecretString::from(invalid), &cancel)
                .await,
            Err(UploadError::CheckpointInvalid)
        ));
    }
    assert!(matches!(
        p.upload_part(&request, &first.checkpoint, 1, b"bc".to_vec(), &cancel)
            .await,
        Err(UploadError::Invalid)
    ));
    let mut saved: serde_json::Value =
        serde_json::from_str(first.checkpoint.expose_secret()).unwrap();
    saved["expires_at"] = json!(1);
    assert!(matches!(
        p.inspect_upload(&request, &SecretString::from(saved.to_string()), &cancel)
            .await,
        Err(UploadError::SessionGone)
    ));
    assert_eq!(server.await.unwrap().len(), 1);
}

#[test]
fn missing_ranges_are_bounded_sorted_nonoverlapping_and_aligned() {
    let size = 4 * ALIGNMENT;
    assert_eq!(
        next_range(
            &[
                format!("{}-", 2 * ALIGNMENT),
                format!("0-{}", ALIGNMENT - 1)
            ],
            size
        )
        .unwrap(),
        (0, ALIGNMENT as u32)
    );
    for ranges in [
        vec![],
        vec!["0-".into(), "1-".into()],
        vec!["-1".into()],
        vec![format!("{size}-")],
        vec!["1-".into()],
        vec!["0-2".into()],
    ] {
        assert!(next_range(&ranges, size).is_err());
    }
    assert_eq!(next_range(&["0-".into()], 3).unwrap(), (0, 3));
}

#[tokio::test]
async fn reconciliation_checks_actual_content_instead_of_acknowledging_equal_size() {
    for matches in [false, true] {
        let request = spec(b"abc", false);
        let metadata = node(&request, "new-tag").to_string();
        let mut download = reply(206, if matches { "abc" } else { "xyz" });
        download.headers = "Content-Range: bytes 0-2/3\r\n".into();
        let (p, server) = fixture(vec![
            reply(200, metadata.clone()),
            reply(200, metadata.clone()),
            download,
            reply(200, metadata.clone()),
            reply(200, metadata),
        ])
        .await;
        let result = p
            .reconcile_upload(&request, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(matches, matches!(result, Reconciliation::Committed(_)));
        let requests = server.await.unwrap();
        assert!(requests.iter().all(|r| r.head.starts_with("GET ")));
        assert!(!requests[2].head.to_lowercase().contains("authorization:"));
    }
}

#[tokio::test]
#[allow(clippy::result_large_err)] // tungstenite fixes this callback's error type.
async fn graph_notification_endpoint_uses_scoped_auth_and_releases_background_capacity() {
    use cirrove_core::notifications::{ChangeHintSender, NotificationState};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{accept_hdr_async, tungstenite::Message};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let notification_url = format!(
        "http://{}/callback?signed=fixture-only",
        listener.local_addr().unwrap()
    );
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert!(!request.headers().contains_key("authorization"));
                assert_eq!(request.uri().path(), "/socket.io/");
                assert_eq!(
                    request.uri().query(),
                    Some("signed=fixture-only&EIO=4&transport=websocket")
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        socket
            .send(Message::Text(
                "0{\"sid\":\"fixture\",\"pingInterval\":25000,\"pingTimeout\":20000}".into(),
            ))
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            "40/callback,"
        );
        socket
            .send(Message::Text("40/callback,{\"sid\":\"ready\"}".into()))
            .await
            .unwrap();
        stop.cancelled().await;
    });
    let (provider, requests) = fixture(vec![
        reply(200, json!({"notificationUrl":notification_url}).to_string()),
        reply(
            200,
            r#"{"value":[],"@odata.deltaLink":"HOST/v1.0/drives/drive/root/delta?token=next"}"#,
        ),
    ])
    .await;
    let provider = Arc::new(provider);
    let (hints, mut receiver) = ChangeHintSender::channel();
    let graph = provider.clone();
    let token = cancel.clone();
    let scope = mutation_request(false).scope;
    let watched = scope.clone();
    let task = tokio::spawn(async move { graph.watch_changes(&watched, hints, &token).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if receiver.borrow_and_update().state == NotificationState::Connected {
                break;
            }
            receiver.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    provider.changes(&scope, None, &cancel).await.unwrap();
    cancel.cancel();
    assert!(matches!(task.await.unwrap(), Err(ProviderError::Cancelled)));
    server.await.unwrap();
    let requests = requests.await.unwrap();
    assert!(
        requests[0]
            .head
            .starts_with("GET /v1.0/drives/drive/root/subscriptions/socketIo ")
    );
    assert!(
        requests[0]
            .head
            .to_lowercase()
            .contains("authorization: bearer fake-graph-bearer")
    );
}
