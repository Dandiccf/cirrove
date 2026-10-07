//! Explicit Pages PACKAGE A/B receipt proof; no mutation or replay.
use super::*;

pub async fn icloud_owned_pages_replacement_receipt_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    super::keynote::replacement::verify_pages(path, digest).await
}
