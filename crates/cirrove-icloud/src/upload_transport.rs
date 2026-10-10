//! iCloud content-slot transport. A slot upload alone does not create a visible
//! Drive item; registration and reconciliation belong to an upload provider.
use crate::{
    ICLOUD_ORIGIN, ICloudReadSession, checked_content_url, drive_request_failure, read_json,
};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs::File;
use tokio_util::io::ReaderStream;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct UploadSlot {
    pub(crate) url: String,
    pub(crate) document_id: String,
}

#[derive(Deserialize)]
pub(crate) struct Uploaded {
    #[serde(rename = "singleFile")]
    pub(crate) file: UploadedFile,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct UploadedFile {
    #[serde(default, deserialize_with = "crate::null_to_default")]
    pub(crate) receipt: String,
    #[serde(rename = "fileChecksum")]
    pub(crate) signature: String,
    #[serde(rename = "referenceChecksum")]
    pub(crate) reference_signature: String,
    #[serde(rename = "wrappingKey")]
    pub(crate) wrapping_key: String,
    pub(crate) size: u64,
}

#[derive(Deserialize)]
struct UpdateReply {
    status: UpdateStatus,
    results: Vec<UpdateResult>,
}

#[derive(Deserialize)]
struct UpdateStatus {
    status_code: i64,
}

#[derive(Deserialize)]
struct UpdateResult {
    status: UpdateStatus,
    document: Option<UpdatedDocument>,
}

#[derive(Deserialize)]
struct UpdatedDocument {
    document_id: String,
    name: String,
    deleted: bool,
}

impl UploadedFile {
    pub(crate) fn valid_for(&self, size: u64) -> bool {
        self.size == size
            && !self.signature.is_empty()
            && (!self.receipt.is_empty()
                || (size == 0
                    && !self.reference_signature.is_empty()
                    && !self.wrapping_key.is_empty()))
    }
}

fn content_type_for_name(name: &str) -> &'static str {
    if name.ends_with(".txt") {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

impl ICloudReadSession {
    /// Register previously uploaded bytes under one exact parent. The caller
    /// must persist the slot and receipt first; an uncertain response must be
    /// reconciled by document ID rather than replayed.
    pub(crate) async fn register_uploaded_file(
        &mut self,
        parent: &str,
        name: &str,
        slot: &UploadSlot,
        data: &UploadedFile,
        size: u64,
    ) -> Result<()> {
        self.register_uploaded_file_response(parent, name, slot, data, size, false)
            .await
    }

    #[cfg(feature = "write-probe")]
    pub(crate) async fn register_uploaded_file_discard_response(
        &mut self,
        parent: &str,
        name: &str,
        slot: &UploadSlot,
        data: &UploadedFile,
        size: u64,
    ) -> Result<()> {
        self.register_uploaded_file_response(parent, name, slot, data, size, true)
            .await
    }

    async fn register_uploaded_file_response(
        &mut self,
        parent: &str,
        name: &str,
        slot: &UploadSlot,
        data: &UploadedFile,
        size: u64,
        discard_response: bool,
    ) -> Result<()> {
        let _timing = crate::probe_timing::Timing::start("file registration");
        if !parent.starts_with("FOLDER::com.apple.CloudDocs::")
            || parent.rsplit("::").next().is_none_or(str::is_empty)
            || name.is_empty()
            || name.len() > 255
            || matches!(name, "." | "..")
            || name.contains(['/', '\0', '\r', '\n'])
            || slot.document_id.is_empty()
            || !data.valid_for(size)
        {
            bail!("invalid iCloud file registration");
        }
        let starting_document_id = parent
            .rsplit("::")
            .next()
            .context("invalid iCloud parent")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let update_url = self
            .docs_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("ws/com.apple.CloudDocs/update/documents")?;
        let mut registration_data = json!({
            "reference_signature": data.reference_signature,
            "signature": data.signature,
            "size": data.size,
            "wrapping_key": data.wrapping_key
        });
        if !data.receipt.is_empty() {
            registration_data["receipt"] = json!(data.receipt);
        }
        let response = self
            .http
            .post(update_url)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({
                "allow_conflict": false,
                "btime": now,
                "command": "add_file",
                "create_short_guid": true,
                "data": registration_data,
                "document_id": slot.document_id,
                "file_flags": {"is_executable": false, "is_hidden": false, "is_writable": true},
                "mtime": now,
                "path": {"path": name, "starting_document_id": starting_document_id}
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud file registration outcome is uncertain"))?;
        #[cfg(feature = "write-probe")]
        if size == 0 {
            eprintln!(
                "Empty registration diagnostic: HTTP {}",
                response.status().as_u16()
            );
        }
        if discard_response {
            bail!("iCloud file registration response deliberately discarded");
        }
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud file registration",
            ));
        }
        let reply: UpdateReply = read_json(response, "iCloud file registration").await?;
        if reply.status.status_code != 0 || reply.results.len() != 1 {
            bail!("iCloud file registration receipt is incomplete");
        }
        let result = &reply.results[0];
        let document = result
            .document
            .as_ref()
            .context("iCloud file registration lacks identity")?;
        if result.status.status_code != 0
            || document.deleted
            || document.name != name
            || document.document_id != slot.document_id
        {
            bail!("iCloud file registration returned a different identity");
        }
        Ok(())
    }

    pub(crate) async fn allocate_upload_slot(
        &mut self,
        name: &str,
        size: u64,
    ) -> Result<UploadSlot> {
        let _timing = crate::probe_timing::Timing::start("upload slot");
        let endpoint = self
            .docs_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?;
        let slot_url = endpoint.join("ws/com.apple.CloudDocs/upload/web")?;
        let response = self
            .http
            .post(slot_url)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({
                "filename": name,
                "type": "FILE",
                "size": size.to_string(),
                "content_type": content_type_for_name(name)
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud upload allocation failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud upload allocation",
            ));
        }
        let mut slots: Vec<UploadSlot> = read_json(response, "iCloud upload allocation").await?;
        if slots.len() != 1 {
            bail!("iCloud upload allocation returned an unexpected result count");
        }
        let slot = slots.remove(0);
        if slot.document_id.is_empty() || slot.document_id.len() > 256 {
            bail!("iCloud upload allocation lacks a document identity");
        }
        checked_content_url(&slot.url)?;
        Ok(slot)
    }

    pub(crate) async fn upload_stream_to_slot(
        &mut self,
        slot: &UploadSlot,
        name: &str,
        file: File,
        size: u64,
    ) -> Result<UploadedFile> {
        let _timing = crate::probe_timing::Timing::start("content upload");
        let upload_url = checked_content_url(&slot.url)?;
        let body = if size == 0 {
            reqwest::Body::from(Vec::<u8>::new())
        } else {
            let file = tokio::fs::File::from_std(file);
            #[cfg(feature = "write-probe")]
            let file = crate::upload_stream_probe::reader(file, name, size);
            reqwest::Body::wrap_stream(ReaderStream::with_capacity(file, 64 * 1024))
        };
        let response = self
            .http
            .post(upload_url)
            .timeout(crate::UPLOAD_TRANSFER_TIMEOUT)
            .header("content-type", content_type_for_name(name))
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(body)
            .send()
            .await
            .map_err(|_| anyhow!("iCloud streamed content upload failed"))?;
        #[cfg(feature = "write-probe")]
        if size == 0 {
            eprintln!(
                "Empty upload diagnostic: HTTP {}",
                response.status().as_u16()
            );
        }
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud streamed content upload",
            ));
        }
        #[cfg(feature = "write-probe")]
        let uploaded: Uploaded = if size == 0 {
            let value: serde_json::Value = read_json(response, "iCloud empty upload").await?;
            let single = &value["singleFile"];
            let nonempty = |field: &str| single[field].as_str().is_some_and(|v| !v.is_empty());
            // Only fixed labels and booleans: never dump provider fields or values.
            eprintln!(
                "Empty upload diagnostic: object={} single_file={} zero_size={} receipt={} signature={} reference={} wrapping_key={}",
                value.is_object(),
                single.is_object(),
                single["size"].as_u64() == Some(0),
                nonempty("receipt"),
                nonempty("fileChecksum"),
                nonempty("referenceChecksum"),
                nonempty("wrappingKey")
            );
            serde_json::from_value(value)
                .map_err(|_| anyhow!("iCloud empty upload receipt has unexpected shape"))?
        } else {
            read_json(response, "iCloud streamed content upload").await?
        };
        #[cfg(not(feature = "write-probe"))]
        let uploaded: Uploaded = read_json(response, "iCloud streamed content upload").await?;
        let data = uploaded.file;
        if !data.valid_for(size) {
            bail!("iCloud streamed upload receipt is incomplete");
        }
        Ok(data)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    /// Exercise the real allocation and streamed-body methods, using the
    /// existing pinned synthetic TLS certificate and a loopback-only client.
    pub(crate) async fn observe_upload_declarations(name: &str) -> (serde_json::Value, String) {
        use base64::Engine as _;
        use std::io::{Seek, SeekFrom, Write};
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio_rustls::{
            TlsAcceptor,
            rustls::{
                ServerConfig,
                pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
            },
        };
        const ORIGIN: &str = "https://fixture.icloud-content.com";
        const PAYLOAD: &[u8] = b"PK\x03\x04\x00synthetic binary payload\xff";
        let decoder = base64::engine::general_purpose::STANDARD;
        let cert = decoder
            .decode(include_str!("package_create/fixtures/server-cert.b64").trim())
            .expect("existing fixture certificate");
        let key = decoder
            .decode(include_str!("package_create/fixtures/server-key.b64").trim())
            .expect("existing fixture key");
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .expect("existing TLS fixture");
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback fixture");
        let client = reqwest::Client::builder()
            .no_proxy()
            .resolve(
                "fixture.icloud-content.com",
                listener.local_addr().expect("fixture address"),
            )
            .tls_certs_only([reqwest::Certificate::from_der(&cert).expect("fixture CA")])
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .expect("local fixture client");
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(8), async move {
                let mut allocation = None;
                let mut upload_type = None;
                for step in 0..2 {
                    let (socket, _) = listener.accept().await.expect("fixture connection");
                    let mut stream = acceptor.accept(socket).await.expect("fixture TLS");
                    let mut raw = Vec::new();
                    let (path, header, body) = loop {
                        let mut block = [0; 4096];
                        let n = stream.read(&mut block).await.expect("fixture request");
                        assert!(n > 0 && raw.len() + n < 32 * 1024, "bounded request");
                        raw.extend_from_slice(&block[..n]);
                        let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let headers = std::str::from_utf8(&raw[..end]).expect("HTTP headers");
                        let size: usize = headers.lines().find_map(|line| {
                            line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned)
                        }).expect("known body size").parse().expect("body size");
                        if raw.len() < end + 4 + size { continue; }
                        let path = headers.lines().next().expect("request line")
                            .split_whitespace().nth(1).expect("request path").to_owned();
                        let header = headers.lines().find_map(|line| {
                            line.to_ascii_lowercase().strip_prefix("content-type: ").map(str::to_owned)
                        }).expect("Content-Type header");
                        break (path, header, raw[end + 4..end + 4 + size].to_vec());
                    };
                    let reply = if step == 0 {
                        assert_eq!(path, "/ws/com.apple.CloudDocs/upload/web");
                        let value: serde_json::Value = serde_json::from_slice(&body).expect("allocation JSON");
                        assert_eq!(value["size"], PAYLOAD.len().to_string());
                        allocation = Some(value);
                        json!([{"url":format!("{ORIGIN}/body"),"document_id":"synthetic-document"}])
                    } else {
                        assert_eq!(path, "/body");
                        assert_eq!(body, PAYLOAD, "real streamed bytes unchanged");
                        upload_type = Some(header);
                        json!({"singleFile":{"receipt":"receipt","fileChecksum":"signature",
                            "referenceChecksum":"reference","wrappingKey":"key","size":PAYLOAD.len()}})
                    }.to_string();
                    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
                    stream.write_all(response.as_bytes()).await.expect("fixture response");
                    stream.shutdown().await.expect("fixture close");
                }
                (allocation.expect("allocation observed"), upload_type.expect("upload observed"))
            }).await.expect("bounded fixture server")
        });
        let mut session = ICloudReadSession::new().expect("fixture session");
        session.http = client;
        session.docs_endpoint = Some(url::Url::parse(ORIGIN).expect("synthetic endpoint"));
        let mut file = tempfile::tempfile().expect("synthetic payload file");
        file.write_all(PAYLOAD).expect("synthetic payload");
        file.seek(SeekFrom::Start(0)).expect("rewind payload");
        let slot = session
            .allocate_upload_slot(name, PAYLOAD.len() as u64)
            .await;
        let uploaded = if let Ok(slot) = &slot {
            session
                .upload_stream_to_slot(slot, name, file, PAYLOAD.len() as u64)
                .await
                .map(|_| ())
        } else {
            Err(anyhow!("allocation fixture did not complete"))
        };
        let observed = server.await.expect("joined original TLS fixture task");
        slot.expect("actual allocation method");
        uploaded.expect("actual streamed upload method");
        observed
    }

    #[tokio::test]
    async fn ordinary_replacement_binary_mime_transport_keeps_plain_text_exception() {
        for (name, expected) in [
            ("stage.key", "application/octet-stream"),
            ("stage.numbers", "application/octet-stream"),
            ("stage.pages", "application/octet-stream"),
            ("stage.pdf", "application/octet-stream"),
            ("stage.tmp", "application/octet-stream"),
            ("stage.txt", "text/plain"),
        ] {
            let (allocation, header) = observe_upload_declarations(name).await;
            assert_eq!(allocation["filename"], name);
            assert_eq!(allocation["type"], "FILE");
            assert_eq!(allocation["content_type"], expected);
            assert_eq!(header, expected);
        }
    }

    #[test]
    fn empty_upload_accepts_integrity_fields_without_receipt_but_nonempty_does_not() {
        for receipt in [None, Some(serde_json::Value::Null), Some(json!(""))] {
            let mut value = json!({"fileChecksum":"checksum", "referenceChecksum":"reference",
                "wrappingKey":"key", "size":0});
            if let Some(receipt) = receipt {
                value["receipt"] = receipt;
            }
            let data: UploadedFile =
                serde_json::from_value(value).expect("zero-byte response shape");
            assert!(data.valid_for(0));
            assert!(!data.valid_for(1));
            let mut missing_signature = data.clone();
            missing_signature.signature.clear();
            assert!(!missing_signature.valid_for(0));
            let mut missing_reference = data.clone();
            missing_reference.reference_signature.clear();
            assert!(!missing_reference.valid_for(0));
            let mut missing_key = data.clone();
            missing_key.wrapping_key.clear();
            assert!(!missing_key.valid_for(0));
            let mut nonempty = data;
            nonempty.size = 1;
            assert!(!nonempty.valid_for(1));
            nonempty.receipt = "receipt".into();
            assert!(nonempty.valid_for(1));
        }
    }
}
