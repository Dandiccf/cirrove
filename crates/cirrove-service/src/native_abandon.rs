//! Explicit local resolution of one proven pre-handoff native replacement.
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAbandonRequest {
    pub label: String,
    pub expected_account_id: String,
    pub operation: uuid::Uuid,
}
/// Historical evidence only. Both remote items were active when checked; neither
/// is moved or deleted here. The staged archive and encrypted checkpoint remain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeAbandonReceipt {
    pub account_id: String,
    pub operation: uuid::Uuid,
    pub original: cirrove_core::Node,
    pub retained_stage: cirrove_core::Node,
    pub observed_unix: u64,
}
impl NativeAbandonReceipt {
    pub(crate) fn bound(
        record: cirrove_icloud::NativeReplacementAbandonRecord,
        account: &str,
        operation: uuid::Uuid,
    ) -> anyhow::Result<Self> {
        record
            .validate()
            .map_err(|_| anyhow::anyhow!("native abandonment record invalid"))?;
        anyhow::ensure!(
            record.operation == operation && record.request.scope.account == account,
            "native abandonment record binding changed"
        );
        Ok(Self {
            account_id: account.into(),
            operation,
            original: record.original,
            retained_stage: record.staged,
            observed_unix: record.observed_unix,
        })
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeAbandonReply {
    #[serde(default)]
    pub job: Option<crate::jobs::Job>,
    #[serde(default)]
    pub receipt: Option<NativeAbandonReceipt>,
    pub refusal: Option<String>,
}
pub async fn abandon_native_stage(
    socket: &Path,
    input: &NativeAbandonRequest,
) -> anyhow::Result<NativeAbandonReply> {
    crate::request(
        socket,
        "abandon-native-stage",
        Some(input),
        "Cirrove native Stage abandonment",
    )
    .await
}
/// Fetch recorded local outcome only; no cloud call or retry, including after restart.
pub async fn native_stage_abandonment(
    socket: &Path,
    input: &NativeAbandonRequest,
) -> anyhow::Result<NativeAbandonReply> {
    crate::request(
        socket,
        "native-stage-abandonment",
        Some(input),
        "Cirrove native Stage abandonment receipt",
    )
    .await
}
pub(crate) async fn handle(
    verb: &str,
    body: &str,
    manager: Option<&std::sync::Arc<crate::manager::Manager>>,
) -> NativeAbandonReply {
    let result = async {
        let input: NativeAbandonRequest = serde_json::from_str(body)?;
        let manager = manager.ok_or_else(|| anyhow::anyhow!("account service unavailable"))?;
        let engine = manager.engine(&input.label).await?;
        if verb == "abandon-native-stage" {
            Ok::<_, anyhow::Error>(NativeAbandonReply {
                job: Some(manager.start_native_abandon(engine, input).await?),
                ..Default::default()
            })
        } else {
            Ok(NativeAbandonReply {
                receipt: manager.native_abandon_receipt(&engine, &input).await?,
                ..Default::default()
            })
        }
    }
    .await;
    result.unwrap_or_else(|_|NativeAbandonReply{refusal:Some("Selected native Stage abandonment is unavailable or changed; no cloud cleanup or replay was requested.".into()),..Default::default()})
}
