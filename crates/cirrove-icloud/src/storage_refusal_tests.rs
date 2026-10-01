//! Synthetic HTTP storage refusal, not evidence of Apple's quota response format.
#![allow(clippy::unwrap_used)]
use super::*;
use crate::upload_transport::{UploadSlot, UploadedFile};
use cirrove_core::{ProviderError, mutation::MutationError, upload::UploadError};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn fixture(
    status: u16,
    path: &'static str,
    body: &'static str,
) -> (ICloudReadSession, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0;4096];
                let n = peer.read(&mut chunk).await.unwrap();
                assert!(n > 0 && request.len() + n <= 16384);
                request.extend_from_slice(&chunk[..n]);
                let Some(end) = request.windows(4).position(|p|p == b"\r\n\r\n") else {continue};
                let end = end + 4;
                let header = std::str::from_utf8(&request[..end]).unwrap().to_lowercase();
                assert!(header.starts_with(&format!("post {} ",path.to_lowercase())));
                let size:usize = header.lines().find_map(|l|l.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                if request.len() < end + size {continue;}
                break;
            }
            peer.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }).await.expect("bounded synthetic HTTP request");
    });
    let mut session = ICloudReadSession::new().unwrap();
    session.drive_endpoint = Some(endpoint.parse().unwrap());
    session.docs_endpoint = Some(endpoint.parse().unwrap());
    (session, server)
}

#[tokio::test]
async fn storage_refusal_survives_allocation_registration_and_folder_creation_http() {
    for status in [507, 401, 403, 503, 509] {
        for stage in ["allocation", "registration", "folder"] {
            let path = match stage {
                "allocation" => "/ws/com.apple.CloudDocs/upload/web",
                "registration" => "/ws/com.apple.CloudDocs/update/documents",
                _ => "/createFolders",
            };
            let (mut session, server) = fixture(status, path, "PRIVATE RESPONSE").await;
            let error = match stage {
                "allocation" => session
                    .allocate_upload_slot("new.txt", 1)
                    .await
                    .err()
                    .unwrap(),
                "registration" => session
                    .register_uploaded_file(
                        ROOT_ID,
                        "new.txt",
                        &UploadSlot {
                            url: "https://fixture.icloud-content.com/PRIVATE".into(),
                            document_id: "fixture".into(),
                        },
                        &UploadedFile {
                            receipt: "private".into(),
                            signature: "checksum".into(),
                            reference_signature: "reference".into(),
                            wrapping_key: "key".into(),
                            size: 1,
                        },
                        1,
                    )
                    .await
                    .err()
                    .unwrap(),
                _ => session
                    .create_folder_request(ROOT_ID, "new")
                    .await
                    .err()
                    .unwrap(),
            };
            assert!(!error.to_string().contains("PRIVATE"));
            if stage == "folder" {
                let error = mutation_error(error);
                match status {
                    507 => assert!(matches!(error, MutationError::InsufficientStorage)),
                    401 | 403 => assert!(matches!(
                        error,
                        MutationError::Provider(ProviderError::Authentication)
                    )),
                    _ => assert!(matches!(error, MutationError::Uncertain)),
                }
            } else {
                let error = file_create::map_session_error(error);
                match status {
                    507 => assert!(matches!(error, UploadError::InsufficientStorage)),
                    401 | 403 => assert!(matches!(
                        error,
                        UploadError::Provider(ProviderError::Authentication)
                    )),
                    _ => assert!(matches!(error, UploadError::Uncertain)),
                }
            }
            server.await.unwrap();
        }
    }
}

#[tokio::test]
async fn storage_refusal_survives_conditional_namespace_http() {
    for status in [507, 401, 403, 503, 509] {
        for (stage, path) in [
            ("rename", "/renameItems"),
            ("move", "/moveItems"),
            ("trash", "/moveItemsToTrash"),
        ] {
            let (mut session, server) = fixture(status, path, "PRIVATE RESPONSE").await;
            let id = "FILE::com.apple.CloudDocs::fixture";
            let result = match stage {
                "rename" => session.send_rename(id, "etag", "new.txt").await,
                "move" => session.send_move(id, "etag", ROOT_ID).await,
                _ => session.send_trash(id, "etag").await,
            };
            let error = result.err().unwrap();
            assert!(!error.to_string().contains("PRIVATE"));
            match (status, mutation_error(error)) {
                (507, MutationError::InsufficientStorage)
                | (401 | 403, MutationError::Provider(ProviderError::Authentication))
                | (503 | 509, MutationError::Uncertain) => {}
                (_, error) => panic!("unexpected classification for {stage}/{status}: {error}"),
            }
            server.await.unwrap();
        }
    }
}

#[test]
fn storage_refusal_signed_content_keeps_expired_urls_distinct_from_account_auth() {
    for status in [507, 401, 403, 503, 509] {
        let error = drive_request_failure(StatusCode::from_u16(status).unwrap(), "content upload");
        let error = file_create::map_content_error(error.context("PRIVATE signed URL"));
        if status == 507 {
            assert!(matches!(error, UploadError::InsufficientStorage));
        } else {
            assert!(matches!(error, UploadError::Uncertain));
        }
        assert!(!error.to_string().contains("PRIVATE"));
    }
    for text in ["507 PRIVATE", "quota exceeded PRIVATE"] {
        assert!(matches!(
            file_create::map_session_error(anyhow!(text)),
            UploadError::Uncertain
        ));
        assert!(matches!(
            file_create::map_content_error(anyhow!(text)),
            UploadError::Uncertain
        ));
        assert!(matches!(
            mutation_error(anyhow!(text)),
            MutationError::Uncertain
        ));
    }
}

#[tokio::test]
async fn storage_refusal_does_not_guess_apple_json_status_codes() {
    let (mut session, server) = fixture(
        200,
        "/ws/com.apple.CloudDocs/update/documents",
        r#"{"status":{"status_code":507},"results":[]}"#,
    )
    .await;
    let result = session
        .register_uploaded_file(
            ROOT_ID,
            "new.txt",
            &UploadSlot {
                url: "https://fixture.icloud-content.com/PRIVATE".into(),
                document_id: "fixture".into(),
            },
            &UploadedFile {
                receipt: "private".into(),
                signature: "checksum".into(),
                reference_signature: "reference".into(),
                wrapping_key: "key".into(),
                size: 1,
            },
            1,
        )
        .await;
    assert!(matches!(
        file_create::map_session_error(result.err().unwrap()),
        UploadError::Uncertain
    ));
    server.await.unwrap();
}
