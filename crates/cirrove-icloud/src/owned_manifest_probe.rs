//! Experimental read-only refresh of one preregistered owned Pages manifest.
//! No document model fetch, SCMP initialization, or mutation is performed.
use crate::ICloudReadSession;
use anyhow::{Result, anyhow, ensure};
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashSet, time::Duration};
use tokio_util::sync::CancellationToken;
use url::Url;

const SHORT: &str = "04d58BKwMzWS05eauV_IDr5BQ";
const DOCUMENT: &str = "8A909C8D-32D7-4E39-9202-DF1862E46C5A";
const LIMIT: usize = 256 * 1024;
const REDIRECTS: usize = 3;

/// Only fixed schema/identity facts may leave the manifest parser.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OwnedManifestObservation {
    pub owner: bool,
    pub exact_document: bool,
    pub exact_zone: bool,
    pub iwmb_service_present: bool,
    pub iwres_service_present: bool,
    pub document_path_present: bool,
}

fn role_host(host: &str, role: &str) -> bool {
    if host == format!("{role}.icloud.com") {
        return true;
    }
    host.strip_prefix('p')
        .and_then(|v| v.strip_suffix(&format!("-{role}.icloud.com")))
        .is_some_and(|v| !v.is_empty() && v.len() <= 4 && v.bytes().all(|b| b.is_ascii_digit()))
}
fn service_url(value: &str, role: &str) -> Result<Url> {
    ensure!(value.len() <= 2048, "manifest service refused");
    let url = Url::parse(value).map_err(|_| anyhow!("manifest service refused"))?;
    ensure!(
        url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.host_str().is_some_and(|h| role_host(h, role)),
        "manifest service refused"
    );
    Ok(url)
}
fn affinity(body: &Value) -> Result<(String, String)> {
    let host = body["X-Apple-MMe-Host"]
        .as_str()
        .ok_or_else(|| anyhow!("manifest affinity refused"))?;
    ensure!(role_host(host, "iwmb"), "manifest affinity refused");
    let sticky = body["StickySessionId"]
        .as_str()
        .ok_or_else(|| anyhow!("manifest affinity refused"))?;
    ensure!(
        !sticky.is_empty() && sticky.len() <= 4096 && !sticky.chars().any(char::is_control),
        "manifest affinity refused"
    );
    Ok((host.to_owned(), sticky.to_owned()))
}
fn request_url(host: &str, sticky: Option<&str>) -> Result<Url> {
    ensure!(role_host(host, "iwmb"), "manifest host refused");
    let mut url = service_url(
        &format!("https://{host}/iwmb/pages/{SHORT}/manifest"),
        "iwmb",
    )?;
    let mut query = url.query_pairs_mut();
    query
        .append_pair("version", "1540")
        .append_pair("clientType", "G15.4")
        .append_pair("clientBuildNumber", "15D120");
    if let Some(sticky) = sticky {
        query.append_pair("s", sticky);
    }
    drop(query);
    Ok(url)
}
fn observation(body: &Value) -> Result<OwnedManifestObservation> {
    ensure!(
        body["shareInfo"]["owner"] == true,
        "manifest is not an owned document"
    );
    let address = body["shareInfo"]["documentId"]
        .as_str()
        .ok_or_else(|| anyhow!("manifest identity missing"))?;
    ensure!(address.len() <= 2048, "manifest identity refused");
    let parts: Vec<_> = address.split(':').collect();
    ensure!(
        parts.len() == 4
            && matches!(parts[0], "docws" | "ndocws")
            && !parts[1].is_empty()
            && !parts[1].chars().any(char::is_control)
            && parts[2] == "com.apple.CloudDocs"
            && parts[3] == DOCUMENT,
        "manifest identity mismatch"
    );
    let services = &body["webservices"];
    let iwmb = services["iwmb"]["url"]
        .as_str()
        .ok_or_else(|| anyhow!("manifest service missing"))?;
    service_url(iwmb, "iwmb")?;
    let iwres_present = !services["iwres"].is_null();
    if iwres_present {
        service_url(
            services["iwres"]["url"]
                .as_str()
                .ok_or_else(|| anyhow!("manifest service refused"))?,
            "iwres",
        )?;
    }
    let path_present = !services["documentPath"].is_null();
    if path_present {
        let path = services["documentPath"]
            .as_str()
            .ok_or_else(|| anyhow!("manifest document path refused"))?;
        ensure!(
            !path.is_empty() && path.len() <= 4096 && !path.chars().any(char::is_control),
            "manifest document path refused"
        );
        // Presence only: never use this opaque provider path to build a request.
    }
    Ok(OwnedManifestObservation {
        owner: true,
        exact_document: true,
        exact_zone: true,
        iwmb_service_present: true,
        iwres_service_present: iwres_present,
        document_path_present: path_present,
    })
}
pub(super) async fn bounded_body(mut response: reqwest::Response) -> Result<Value> {
    ensure!(
        response.content_length().is_none_or(|n| n <= LIMIT as u64),
        "manifest response too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("manifest response unavailable"))?
    {
        ensure!(
            chunk.len() <= LIMIT.saturating_sub(bytes.len()),
            "manifest response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("manifest response invalid"))
}
impl ICloudReadSession {
    /// Exact fixture identity only. Caller additionally binds the account and
    /// durable imported receipt before invoking this read-only method.
    pub async fn probe_owned_pages_manifest(
        &self,
        document_id: &str,
        cancel: &CancellationToken,
    ) -> Result<OwnedManifestObservation> {
        ensure!(
            document_id == format!("FILE::com.apple.CloudDocs::{DOCUMENT}")
                && self.account_hash.is_some(),
            "manifest fixture binding refused"
        );
        let action = async {
            let mut host = "iwmb.icloud.com".to_owned();
            let mut sticky = None;
            let mut seen = HashSet::new();
            for hop in 0..=REDIRECTS {
                ensure!(
                    seen.insert((host.clone(), sticky.clone())),
                    "manifest affinity loop refused"
                );
                let url = request_url(&host, sticky.as_deref())?;
                ensure!(!cancel.is_cancelled(), "manifest request cancelled");
                // Existing cookie jar only; never copy Apple authentication headers
                // or a browser document access token onto an editor service.
                let response = self
                    .http
                    .get(url)
                    .send()
                    .await
                    .map_err(|_| anyhow!("manifest request unavailable"))?;
                match response.status().as_u16() {
                    200 => return observation(&bounded_body(response).await?),
                    330 if hop < REDIRECTS => {
                        let next = affinity(&bounded_body(response).await?)?;
                        host = next.0;
                        sticky = Some(next.1);
                    }
                    status @ (401 | 403 | 421) => {
                        return Err(anyhow!("manifest session rejected (HTTP {status})"));
                    }
                    _ => return Err(anyhow!("manifest response status refused")),
                }
            }
            Err(anyhow!("manifest affinity limit exceeded"))
        };
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(anyhow!("manifest request cancelled")),
            result = tokio::time::timeout(Duration::from_secs(60), action) => result.map_err(|_| anyhow!("manifest request timed out"))?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn manifest() -> Value {
        serde_json::json!({"shareInfo":{"owner":true,"documentId":format!("docws:synthetic-owner:com.apple.CloudDocs:{DOCUMENT}")},"webservices":{"iwmb":{"url":"https://p123-iwmb.icloud.com/iwmb/"},"iwres":{"url":"https://p123-iwres.icloud.com/"},"documentPath":"opaque-synthetic-path"}})
    }
    #[test]
    fn owned_manifest_exact_identity_and_sanitized_observation() {
        let observed = observation(&manifest()).expect("valid synthetic manifest");
        assert!(observed.owner && observed.exact_document && observed.exact_zone);
        assert!(observed.iwres_service_present && observed.document_path_present);
        let serialized = serde_json::to_string(&observed).expect("observation JSON");
        for secret in [
            "synthetic-owner",
            "opaque-synthetic-path",
            "icloud.com",
            DOCUMENT,
        ] {
            assert!(!serialized.contains(secret));
        }
        for (key, value) in [
            ("owner", serde_json::json!(false)),
            (
                "documentId",
                serde_json::json!("docws:owner:com.apple.CloudDocs:wrong"),
            ),
            (
                "documentId",
                serde_json::json!(format!("docws:owner:other:{DOCUMENT}")),
            ),
        ] {
            let mut body = manifest();
            body["shareInfo"][key] = value;
            assert!(observation(&body).is_err());
        }
    }
    #[test]
    fn owned_manifest_role_and_affinity_boundaries() {
        for bad in [
            "https://iwmb.icloud.com.evil/",
            "https://iwres.icloud.com/",
            "http://iwmb.icloud.com/",
            "https://user@iwmb.icloud.com/",
            "https://iwmb.icloud.com:444/",
            "https://iwmb.icloud.com/?token=secret",
            "https://127.0.0.1/",
            "https://p12345-iwmb.icloud.com/",
        ] {
            assert!(service_url(bad, "iwmb").is_err());
        }
        for host in ["iwmb.icloud.com", "p123-iwmb.icloud.com"] {
            assert!(
                affinity(
                    &serde_json::json!({"X-Apple-MMe-Host":host,"StickySessionId":"synthetic"})
                )
                .is_ok()
            );
        }
        assert!(affinity(&serde_json::json!({"X-Apple-MMe-Host":"p123-iwres.icloud.com","StickySessionId":"synthetic"})).is_err());
        assert!(
            affinity(
                &serde_json::json!({"X-Apple-MMe-Host":"iwmb.icloud.com","StickySessionId":"\n"})
            )
            .is_err()
        );
        let url = request_url("iwmb.icloud.com", None).expect("initial request");
        assert_eq!(url.path(), format!("/iwmb/pages/{SHORT}/manifest"));
        assert_eq!(
            url.query(),
            Some("version=1540&clientType=G15.4&clientBuildNumber=15D120")
        );
    }
    #[tokio::test]
    async fn owned_manifest_wrong_binding_and_cancellation_refuse_before_network() {
        let mut session = ICloudReadSession::new().expect("synthetic session");
        let cancel = CancellationToken::new();
        assert!(
            session
                .probe_owned_pages_manifest("wrong", &cancel)
                .await
                .is_err()
        );
        session.account_hash = Some("synthetic".into());
        cancel.cancel();
        assert!(
            session
                .probe_owned_pages_manifest(
                    &format!("FILE::com.apple.CloudDocs::{DOCUMENT}"),
                    &cancel
                )
                .await
                .expect_err("cancelled")
                .to_string()
                .contains("cancelled")
        );
    }
}

#[cfg(test)]
#[path = "owned_manifest_probe/http_tests.rs"]
mod http_tests;
