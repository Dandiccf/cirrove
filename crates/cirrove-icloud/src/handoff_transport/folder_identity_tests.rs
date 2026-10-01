#![allow(clippy::unwrap_used)]
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
fn plan() -> HandoffPlan {
    HandoffPlan {
        version: 5,
        folder_parent_id: ROOT_ID.into(),
        folder_id: "FOLDER::com.apple.CloudDocs::owned".into(),
        folder_name: "Known folder".into(),
        original_id: "FILE::com.apple.CloudDocs::old".into(),
        original_doc_id: "old".into(),
        original_etag: "old-tag".into(),
        staged_id: "FILE::com.apple.CloudDocs::new".into(),
        staged_doc_id: "new".into(),
        staged_etag: "new-tag".into(),
        staged_name: format!("staged-by-cirrove-{}.txt", Uuid::new_v4()),
        recovery_name: format!("recovery-by-cirrove-{}.txt", Uuid::new_v4()),
        target_name: "File.txt".into(),
        original_sha256: "a".repeat(64),
        staged_sha256: "b".repeat(64),
    }
}
async fn fixture(
    expected: Vec<String>,
    replies: Vec<serde_json::Value>,
) -> (ICloudReadSession, tokio::task::JoinHandle<()>) {
    assert_eq!(expected.len(), replies.len());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        for (id, reply) in expected.into_iter().zip(replies) {
            let (mut socket, _) =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                    .await
                    .expect("expected request within deadline")
                    .unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 4096];
                let n = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    socket.read(&mut bytes),
                )
                .await
                .expect("complete request within deadline")
                .unwrap();
                assert!(n > 0 && request.len() < 16384);
                request.extend_from_slice(&bytes[..n]);
                let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") else {
                    continue;
                };
                let end = end + 4;
                let headers = std::str::from_utf8(&request[..end]).unwrap().to_lowercase();
                let count: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if request.len() < end + count {
                    continue;
                }
                let body: serde_json::Value =
                    serde_json::from_slice(&request[end..end + count]).unwrap();
                assert_eq!(
                    body[0]["drivewsid"], id,
                    "only registered folder requests are permitted"
                );
                break;
            }
            let body = reply.to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let mut session = ICloudReadSession::new().unwrap();
    session.drive_endpoint = Some(url.parse().unwrap());
    (session, task)
}
fn folder(plan: &HandoffPlan) -> serde_json::Value {
    serde_json::json!({"drivewsid":plan.folder_id,"parentId":plan.folder_parent_id,"type":"FOLDER","name":plan.folder_name,"numberOfItems":1,"items":[{"drivewsid":plan.original_id,"name":"File.txt","type":"FILE","etag":"old-tag","size":3}]})
}
#[tokio::test]
async fn new_handoff_fetches_the_known_folder_once_and_reuses_its_children() {
    let plan = plan();
    let (mut session, task) = fixture(
        vec![plan.folder_id.clone()],
        vec![serde_json::json!([folder(&plan)])],
    )
    .await;
    let items = session.handoff_items(&plan).await.unwrap().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].drivewsid, plan.original_id);
    task.await.unwrap();
}

#[tokio::test]
async fn new_handoff_refuses_changed_folder_location_name_or_kind() {
    for (field, value) in [
        ("parentId", "FOLDER::com.apple.CloudDocs::elsewhere"),
        ("parentId", ""),
        ("name", "Renamed folder"),
        ("type", "FILE"),
    ] {
        let plan = plan();
        let mut response = folder(&plan);
        response[field] = value.into();
        let (mut session, task) = fixture(
            vec![plan.folder_id.clone()],
            vec![serde_json::json!([response])],
        )
        .await;
        assert!(
            session.handoff_items(&plan).await.unwrap().is_none(),
            "{field}"
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn new_handoff_refuses_incomplete_or_ambiguous_envelopes() {
    let plan = plan();
    let mut incomplete = folder(&plan);
    incomplete["numberOfItems"] = 2.into();
    let mut wrong_id = folder(&plan);
    wrong_id["drivewsid"] = "FOLDER::com.apple.CloudDocs::other".into();
    for response in [
        serde_json::json!([incomplete]),
        serde_json::json!([wrong_id]),
        serde_json::json!([folder(&plan), folder(&plan)]),
        serde_json::json!([]),
    ] {
        let (mut session, task) = fixture(vec![plan.folder_id.clone()], vec![response]).await;
        assert!(session.handoff_items(&plan).await.is_err());
        task.await.unwrap();
    }
}

#[tokio::test]
async fn new_handoff_validates_a_nested_parent_without_enumerating_it() {
    let mut plan = plan();
    plan.folder_parent_id = "FOLDER::com.apple.CloudDocs::nested-parent".into();
    let (mut session, task) = fixture(
        vec![plan.folder_id.clone()],
        vec![serde_json::json!([folder(&plan)])],
    )
    .await;
    assert!(session.handoff_items(&plan).await.unwrap().is_some());
    task.await.unwrap();
}

fn parent(plan: &HandoffPlan, duplicate_name: bool) -> serde_json::Value {
    let mut children = vec![folder(plan)];
    if duplicate_name {
        let mut other = folder(plan);
        other["drivewsid"] = "FOLDER::com.apple.CloudDocs::unrelated".into();
        children.push(other);
    }
    serde_json::json!([{"drivewsid": plan.folder_parent_id, "type":"FOLDER", "name":"parent", "numberOfItems":children.len(), "items":children}])
}

#[tokio::test]
async fn legacy_plan_still_lists_parent_and_refuses_duplicate_sibling_names() {
    let mut plan = plan();
    plan.version = 3;
    let (mut session, task) = fixture(
        vec![plan.folder_parent_id.clone()],
        vec![parent(&plan, true)],
    )
    .await;
    assert!(session.handoff_items(&plan).await.unwrap().is_none());
    task.await.unwrap();
    let (mut session, task) = fixture(
        vec![plan.folder_parent_id.clone(), plan.folder_id.clone()],
        vec![parent(&plan, false), serde_json::json!([folder(&plan)])],
    )
    .await;
    assert!(session.handoff_items(&plan).await.unwrap().is_some());
    task.await.unwrap();
}

#[test]
fn new_contract_cannot_be_used_for_root_or_unknown_versions() {
    let mut plan = plan();
    assert!(plan.validate().is_ok());
    plan.version = 6;
    assert!(plan.validate().is_err());
    plan.version = 5;
    plan.folder_id = ROOT_ID.into();
    assert!(plan.validate().is_err());
}
