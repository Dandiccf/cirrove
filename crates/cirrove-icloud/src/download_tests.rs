use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn lookup(reply: serde_json::Value) -> Result<ICloudReadSession> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.expect("request");
        let mut request = Vec::new();
        loop {
            let mut block = [0u8; 1024];
            let count = peer.read(&mut block).await.expect("read");
            assert!(count > 0 && request.len() < 8192);
            request.extend_from_slice(&block[..count]);
            if request.windows(4).any(|v| v == b"\r\n\r\n") {
                break;
            }
        }
        assert!(
            std::str::from_utf8(&request)
                .expect("header")
                .starts_with("GET /ws/com.apple.CloudDocs/download/by_id?document_id=fixture ")
        );
        let data = serde_json::to_vec(&reply).expect("json");
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            data.len()
        );
        peer.write_all(headers.as_bytes()).await.expect("headers");
        peer.write_all(&data).await.expect("body");
    });
    let mut session = ICloudReadSession::new()?;
    session.docs_endpoint = Some(format!("http://{address}/").parse()?);
    Ok(session)
}

#[tokio::test]
async fn ordinary_content_lookup_refuses_package_and_ambiguous_representations() -> Result<()> {
    let token = json!({"url":"https://fixture.icloud-content.com/content?secret=PRIVATE"});
    let id = "FILE::com.apple.CloudDocs::fixture";
    let mut ordinary = lookup(json!({"data_token":token.clone()})).await?;
    assert!(ordinary.ordinary_download_url(id).await.is_ok());
    for reply in [
        json!({"package_token":token.clone()}),
        json!({"data_token":token.clone(),"package_token":token}),
        json!({}),
    ] {
        let mut session = lookup(reply).await?;
        let result = session.ordinary_download_url(id).await;
        assert!(
            result.is_err(),
            "ordinary content must not accept a package URL"
        );
        assert!(
            !format!(
                "{:#}",
                result.expect_err("package or ambiguous response must fail")
            )
            .contains("PRIVATE")
        );
    }
    Ok(())
}

#[tokio::test]
async fn read_lookup_retains_the_representation_and_checks_both_url_kinds() -> Result<()> {
    let id = "FILE::com.apple.CloudDocs::fixture";
    for (field, package) in [("data_token", false), ("package_token", true)] {
        let mut session =
            lookup(json!({field:{"url":"https://fixture.icloud-content.com/content"}})).await?;
        let representation = session.download_representation(id).await?;
        assert_eq!(
            matches!(representation, ContentRepresentation::Package(_)),
            package
        );
        assert_eq!(
            representation.for_exact_read().host_str(),
            Some("fixture.icloud-content.com")
        );
        let mut session = lookup(json!({field:{"url":"https://outside.invalid/private"}})).await?;
        assert!(session.download_representation(id).await.is_err());
    }
    Ok(())
}

async fn empty_file_fixture(
    location: serde_json::Value,
) -> Result<(
    ICloudReadSession,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
)> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let lookups = Arc::new(AtomicUsize::new(0));
    let count = lookups.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut peer, _) = listener.accept().await.expect("request");
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 4096];
                let n = peer.read(&mut bytes).await.expect("request bytes");
                assert!(n > 0 && request.len() < 16384);
                request.extend_from_slice(&bytes[..n]);
                if request.windows(4).any(|v| v == b"\r\n\r\n") {
                    break;
                }
            }
            let header = std::str::from_utf8(&request).expect("header");
            let body = if header
                .starts_with("GET /ws/com.apple.CloudDocs/download/by_id?document_id=fixture ")
            {
                count.fetch_add(1, Ordering::SeqCst);
                location.clone()
            } else {
                assert!(header.starts_with("POST /retrieveItemDetailsInFolders"));
                json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":1,"items":[{
                    "drivewsid":"FILE::com.apple.CloudDocs::fixture","type":"FILE","name":"Empty","etag":"v1","size":0
                }]}])
            };
            let body = serde_json::to_vec(&body).expect("response");
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            peer.write_all(header.as_bytes()).await.expect("header");
            peer.write_all(&body).await.expect("body");
        }
    });
    let mut session = ICloudReadSession::new()?;
    session.docs_endpoint = Some(format!("http://{address}/").parse()?);
    session.drive_endpoint = session.docs_endpoint.clone();
    Ok((session, lookups, server))
}

#[tokio::test]
async fn zero_length_write_preflight_still_requires_an_ordinary_representation() -> Result<()> {
    use std::sync::atomic::Ordering;
    let token = json!({"url":"https://fixture.icloud-content.com/content?secret=PRIVATE"});
    for location in [
        json!({"package_token":token.clone()}),
        json!({"data_token":token.clone(),"package_token":token.clone()}),
        json!({}),
    ] {
        let (mut session, lookups, server) = empty_file_fixture(location).await?;
        let result = session
            .hash_file_in_folder_for_revision(
                ROOT_ID,
                "FILE::com.apple.CloudDocs::fixture",
                "v1",
                0,
            )
            .await;
        server.abort();
        assert!(
            result.is_err(),
            "a zero-length package or ambiguous representation must not qualify for ordinary replacement"
        );
        assert_eq!(lookups.load(Ordering::SeqCst), 1);
    }
    let (mut session, lookups, server) = empty_file_fixture(json!({"data_token":token})).await?;
    let result = session
        .hash_file_in_folder_for_revision(ROOT_ID, "FILE::com.apple.CloudDocs::fixture", "v1", 0)
        .await;
    server.abort();
    assert_eq!(result?, hex::encode(Sha256::digest([])));
    assert_eq!(lookups.load(Ordering::SeqCst), 1);
    Ok(())
}
