//! One controlled foreign edit of an explicitly owned mounted-test fixture.
//! No Trash request, retries, or normal-provider entry point is supplied here.
use super::*;
use cirrove_core::{Node, NodeKind};

fn validate_fixture(plan: &HandoffPlan, original: &[u8], staged: &[u8]) -> Result<()> {
    plan.validate()?;
    let run = plan
        .folder_name
        .strip_prefix("Cirrove Write Validation-")
        .context("not an owned validation folder")?;
    if uuid::Uuid::parse_str(run).is_err()
        || plan.folder_parent() != ROOT_ID
        || plan.target_name != "Account Router.txt"
        || original.is_empty()
        || original.len() > 4096
        || staged.is_empty()
        || staged.len() > 4096
        || original == staged
        || hex::encode(Sha256::digest(original)) != plan.original_sha256
        || hex::encode(Sha256::digest(staged)) != plan.staged_sha256
    {
        bail!("invalid owned competing-edit fixture");
    }
    Ok(())
}

fn require_absent(items: &[serde_json::Value], complete: bool, id: &str) -> Result<()> {
    if !complete
        || items
            .iter()
            .any(|item| item.get("drivewsid").and_then(|v| v.as_str()) == Some(id))
    {
        bail!("owned cloud version is in Trash or its absence is unverified");
    }
    Ok(())
}

impl ICloudReadSession {
    /// Absence requires a complete Trash listing. The positive membership
    /// probe deliberately errors on zero matches and cannot establish this.
    pub async fn probe_exact_item_absent_from_trash(&mut self, id: &str) -> Result<()> {
        let (zone, _) = split_file_id(id)?;
        if zone != "com.apple.CloudDocs" {
            bail!("invalid owned Trash identity");
        }
        let (items, complete) = self.read_trash_items().await?;
        require_absent(&items, complete, id)
    }

    /// Change exactly the small original whose IDs, revisions and hashes were
    /// prepared by the normal writer. The caller records a durable one-shot
    /// marker before entering; an ambiguous result must never be retried.
    pub async fn probe_owned_competing_edit(
        &mut self,
        plan: &HandoffPlan,
        original: &[u8],
        staged: &[u8],
    ) -> Result<Node> {
        validate_fixture(plan, original, staged)?;
        if self.inspect_durable_trash_handoff(plan).await? != HandoffObserved::Prepared {
            bail!("owned competing-edit preflight changed");
        }
        let mut revised = original.to_vec();
        revised[0] ^= 1;
        let folder = ValidationFolder {
            id: plan.folder_id.clone(),
            name: plan.folder_name.clone(),
        };
        let file = ValidationFile {
            id: plan.original_id.clone(),
            document_id: plan.original_doc_id.clone(),
            etag: plan.original_etag.clone(),
            name: plan.target_name.clone(),
        };
        if self
            .probe_named_same_id_update(&folder, &file, original, &revised)
            .await?
            != SameIdUpdateOutcome::Updated
        {
            bail!("owned competing edit was not confirmed; do not retry");
        }
        let items = self.list_folder(&plan.folder_id).await?;
        if items.len() != 2 {
            bail!("owned fixture acquired unrelated entries");
        }
        let old = exactly_one(
            items
                .iter()
                .filter(|n| n.drivewsid == plan.original_id)
                .collect(),
            "owned revised identity",
        )?;
        let copy = exactly_one(
            items
                .iter()
                .filter(|n| n.drivewsid == plan.staged_id)
                .collect(),
            "owned staged identity",
        )?;
        if old.is_folder()
            || old.docwsid != plan.original_doc_id
            || old.etag == plan.original_etag
            || old.display_name() != plan.target_name
            || old.size != revised.len() as u64
            || copy.is_folder()
            || copy.docwsid != plan.staged_doc_id
            || copy.etag != plan.staged_etag
            || copy.display_name() != plan.staged_name
            || self
                .read_small_file_in_folder(&plan.folder_id, &old.drivewsid)
                .await?
                != revised
            || self
                .read_small_file_in_folder(&plan.folder_id, &copy.drivewsid)
                .await?
                != staged
        {
            bail!("owned competing-edit postflight failed");
        }
        Ok(Node {
            id: old.drivewsid.clone(),
            parent_id: Some(plan.folder_id.clone()),
            name: old.display_name(),
            kind: NodeKind::File,
            size: old.size,
            modified_unix: 0,
            etag: Some(old.etag.clone()),
            content_version: None,
            target: None,
            package: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absence_requires_complete_listing_and_no_exact_identity() {
        let items = vec![serde_json::json!({"drivewsid":"other"})];
        assert!(require_absent(&items, true, "owned").is_ok());
        assert!(require_absent(&items, false, "owned").is_err());
        assert!(require_absent(&items, true, "other").is_err());
    }
    fn plan() -> HandoffPlan {
        HandoffPlan {
            version: 3,
            folder_parent_id: ROOT_ID.into(),
            folder_id: "FOLDER::com.apple.CloudDocs::folder".into(),
            folder_name: format!("Cirrove Write Validation-{}", Uuid::new_v4()),
            original_id: "FILE::com.apple.CloudDocs::old".into(),
            original_doc_id: "old".into(),
            original_etag: "old-tag".into(),
            staged_id: "FILE::com.apple.CloudDocs::new".into(),
            staged_doc_id: "new".into(),
            staged_etag: "new-tag".into(),
            staged_name: format!("staged-by-cirrove-{}.txt", Uuid::new_v4()),
            recovery_name: format!("recovery-by-cirrove-{}.txt", Uuid::new_v4()),
            target_name: "Account Router.txt".into(),
            original_sha256: hex::encode(Sha256::digest(b"old")),
            staged_sha256: hex::encode(Sha256::digest(b"new")),
        }
    }
    #[test]
    fn competing_edit_requires_exact_owned_name_parent_and_both_payloads() {
        let p = plan();
        assert!(validate_fixture(&p, b"old", b"new").is_ok());
        for field in [
            "folder_name",
            "folder_parent_id",
            "target_name",
            "original_sha256",
            "staged_sha256",
        ] {
            let mut value = serde_json::to_value(&p).unwrap();
            value[field] = serde_json::json!("unrelated");
            let other = serde_json::from_value(value).unwrap();
            assert!(validate_fixture(&other, b"old", b"new").is_err(), "{field}");
        }
        assert!(validate_fixture(&p, b"changed", b"new").is_err());
        assert!(validate_fixture(&p, b"old", b"changed").is_err());
        assert!(validate_fixture(&p, b"", b"new").is_err());
        assert!(validate_fixture(&p, &vec![0; 4097], b"new").is_err());
    }
}
