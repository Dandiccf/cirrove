//! Fixed-account setup validation only; never signs in or follows editor URLs.
use super::{ICloudReadSession, account_hash, owned_manifest_probe::bounded_body};
use anyhow::{Result, anyhow, ensure};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct OwnedReadinessObservation {
    pub account_identity_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcs_service_identities_included: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_access_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icdrs_disabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_consented_for_pcs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hsa_challenge_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iwmb_pcs_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iwres_pcs_required: Option<bool>,
}
fn optional_bool(value: &Value) -> Result<Option<bool>> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| anyhow!("readiness schema refused"))
}
fn observation(
    value: &Value,
    expected_account_hash: &str,
    trusted_dsid_hash: Option<&str>,
) -> Result<OwnedReadinessObservation> {
    if let Some(trusted) = trusted_dsid_hash {
        ensure!(
            super::session_identity::dsid_hash(&value["dsInfo"]["dsid"]).as_deref()
                == Some(trusted),
            "readiness stable account identity mismatch"
        );
    } else {
        let identity = value["dsInfo"]["appleId"]
            .as_str()
            .ok_or_else(|| anyhow!("readiness identity missing"))?;
        ensure!(
            account_hash(identity).map_err(|_| anyhow!("readiness identity refused"))?
                == expected_account_hash,
            "readiness account identity mismatch"
        );
    }
    ensure!(
        value["webservices"].is_object(),
        "readiness service schema missing"
    );
    Ok(OwnedReadinessObservation {
        account_identity_verified: true,
        pcs_service_identities_included: optional_bool(&value["pcsServiceIdentitiesIncluded"])?,
        web_access_allowed: optional_bool(&value["dsInfo"]["isWebAccessAllowed"])?,
        icdrs_disabled: optional_bool(&value["dsInfo"]["isICDRSDisabled"])?,
        device_consented_for_pcs: optional_bool(&value["dsInfo"]["isDeviceConsentedForPCS"])?,
        hsa_challenge_required: optional_bool(&value["hsaChallengeRequired"])?,
        iwmb_pcs_required: optional_bool(&value["webservices"]["iwmb"]["pcsRequired"])?,
        iwres_pcs_required: optional_bool(&value["webservices"]["iwres"]["pcsRequired"])?,
    })
}
fn request_url(client: Uuid, request: Uuid) -> Result<Url> {
    let mut url = Url::parse("https://setup.icloud.com/setup/ws/1/validate")
        .map_err(|_| anyhow!("invalid fixed readiness endpoint"))?;
    url.query_pairs_mut()
        .append_pair("requestId", &request.to_string())
        .append_pair("clientBuildNumber", "2636Build34")
        .append_pair("clientMasteringNumber", "2636Build34")
        .append_pair("clientId", &client.to_string());
    // The public wrapper adds dsid only when it already knows one. Never infer
    // it from a cookie or substitute Cirrove's local account identifier.
    Ok(url)
}
impl ICloudReadSession {
    pub async fn probe_owned_pages_readiness(
        &self,
        expected_apple_id: &str,
        cancel: &CancellationToken,
    ) -> Result<OwnedReadinessObservation> {
        let expected = account_hash(expected_apple_id)?;
        ensure!(
            self.account_hash.as_deref() == Some(expected.as_str()),
            "readiness local account binding mismatch"
        );
        let action = async {
            ensure!(!cancel.is_cancelled(), "readiness request cancelled");
            let response = self
                .http
                .post(request_url(Uuid::new_v4(), Uuid::new_v4())?)
                .header("origin", "https://www.icloud.com")
                .header("referer", "https://www.icloud.com/")
                .send()
                .await
                .map_err(|_| anyhow!("readiness request unavailable"))?;
            let status = response.status().as_u16();
            ensure!(
                status == 200,
                "readiness validation refused (HTTP {status})"
            );
            observation(
                &bounded_body(response)
                    .await
                    .map_err(|_| anyhow!("readiness response invalid or oversized"))?,
                &expected,
                self.trusted_dsid_hash.as_deref(),
            )
        };
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(anyhow!("readiness request cancelled")),
            result = tokio::time::timeout(Duration::from_secs(30), action) => result.map_err(|_| anyhow!("readiness request timed out"))?,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_readiness_identity_schema_and_boolean_only_output() {
        let hash = account_hash("fixture@example.invalid").expect("synthetic account");
        let mut value = serde_json::json!({"dsInfo":{"appleId":"fixture@example.invalid","isWebAccessAllowed":true},"pcsServiceIdentitiesIncluded":false,"webservices":{"iwmb":{"url":"secret-unused-url","pcsRequired":true}}});
        let observed = observation(&value, &hash, None).expect("bound account");
        let safe = serde_json::to_value(observed).expect("safe observation");
        assert!(
            safe.as_object()
                .expect("object")
                .values()
                .all(Value::is_boolean)
        );
        assert_eq!(safe["pcs_service_identities_included"], false);
        assert!(safe.get("iwres_pcs_required").is_none());
        assert!(!safe.to_string().contains("fixture@example.invalid"));
        value["dsInfo"]["appleId"] = "other@example.invalid".into();
        assert!(observation(&value, &hash, None).is_err());
        value["dsInfo"]["appleId"] = "fixture@example.invalid".into();
        value["pcsServiceIdentitiesIncluded"] = "true".into();
        assert!(observation(&value, &hash, None).is_err());
    }
    #[test]
    fn owned_readiness_fixed_bodyless_validate_contract() {
        let url = request_url(Uuid::nil(), Uuid::nil()).expect("fixed endpoint");
        assert_eq!(url.host_str(), Some("setup.icloud.com"));
        assert_eq!(url.path(), "/setup/ws/1/validate");
        let pairs: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs.len(), 4);
        assert_eq!(pairs["clientBuildNumber"], "2636Build34");
        assert_eq!(pairs["clientMasteringNumber"], "2636Build34");
        assert!(!pairs.contains_key("dsid"));
    }
    #[tokio::test]
    async fn owned_readiness_wrong_local_account_refused_before_network() {
        let mut session = ICloudReadSession::new().expect("synthetic session");
        session.account_hash = Some(account_hash("fixture@example.invalid").expect("account hash"));
        assert!(
            session
                .probe_owned_pages_readiness("other@example.invalid", &CancellationToken::new())
                .await
                .expect_err("wrong account")
                .to_string()
                .contains("binding mismatch")
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            session
                .probe_owned_pages_readiness("fixture@example.invalid", &cancel)
                .await
                .expect_err("cancelled")
                .to_string()
                .contains("cancelled")
        );
    }
}

#[cfg(test)]
#[path = "owned_readiness_probe/http_tests.rs"]
mod http_tests;
