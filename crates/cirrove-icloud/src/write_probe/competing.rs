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

fn validate_editor(plan: &HandoffPlan, editor: Option<&Node>, size: usize) -> Result<()> {
    if let Some(n) = editor
        && (n.id == plan.original_id
            || n.id == plan.staged_id
            || n.parent_id.as_ref() != Some(&plan.folder_id)
            || n.name != ".cirrove-conflicting-editor"
            || n.kind != NodeKind::File
            || n.package
            || n.target.is_some()
            || n.etag.as_ref().is_none_or(|s| s.is_empty())
            || n.size != size as u64)
    {
        bail!("invalid owned editor source");
    }
    Ok(())
}
fn require_editor_ids(plan: &HandoffPlan, editor: &Node, ids: &[&str]) -> Result<()> {
    let expected = [&plan.original_id, &plan.staged_id, &editor.id];
    if ids.len() != 3
        || expected
            .iter()
            .any(|id| ids.iter().filter(|actual| *actual == id).count() != 1)
    {
        bail!("owned fixture acquired unrelated entries");
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
        self.probe_owned_competing_editor_edit(plan, original, staged, None)
            .await
    }

    /// The optional editor source must be an independently confirmed receipt
    /// from the same owned fixture, not permission for arbitrary extra entries.
    pub async fn probe_owned_competing_editor_edit(
        &mut self,
        plan: &HandoffPlan,
        original: &[u8],
        staged: &[u8],
        editor: Option<&Node>,
    ) -> Result<Node> {
        validate_fixture(plan, original, staged)?;
        validate_editor(plan, editor, staged.len())?;
        if self.inspect_durable_trash_handoff(plan).await? != HandoffObserved::Prepared {
            bail!("owned competing-edit preflight changed");
        }
        if let Some(editor) = editor {
            self.verify_editor_entries(plan, editor, staged).await?;
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
        if let Some(editor) = editor {
            self.verify_editor_entries(plan, editor, staged).await?;
        } else if items.len() != 2 {
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
    async fn verify_editor_entries(
        &mut self,
        plan: &HandoffPlan,
        editor: &Node,
        staged: &[u8],
    ) -> Result<()> {
        let items = self.list_folder(&plan.folder_id).await?;
        let ids: Vec<_> = items.iter().map(|n| n.drivewsid.as_str()).collect();
        require_editor_ids(plan, editor, &ids)?;
        let source = exactly_one(
            items.iter().filter(|n| n.drivewsid == editor.id).collect(),
            "owned editor source",
        )?;
        if source.is_folder()
            || source.display_name() != editor.name
            || Some(&source.etag) != editor.etag.as_ref()
            || source.size != editor.size
            || self
                .read_small_file_in_folder(&plan.folder_id, &source.drivewsid)
                .await?
                != staged
        {
            bail!("owned editor source changed");
        }
        Ok(())
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
            package: None,
        }
    }
    #[test]
    fn atomic_competing_fixture_allows_only_the_exact_confirmed_editor_source() {
        let p = plan();
        let n = Node {
            id: "FILE::com.apple.CloudDocs::editor".into(),
            parent_id: Some(p.folder_id.clone()),
            name: ".cirrove-conflicting-editor".into(),
            kind: NodeKind::File,
            size: 3,
            etag: Some("editor-tag".into()),
            content_version: None,
            modified_unix: 0,
            target: None,
            package: false,
        };
        assert!(validate_editor(&p, Some(&n), 3).is_ok());
        assert!(require_editor_ids(&p, &n, &[&p.original_id, &p.staged_id, &n.id]).is_ok());
        for ids in [
            vec![p.original_id.as_str(), p.staged_id.as_str()],
            vec![p.original_id.as_str(), p.staged_id.as_str(), "foreign"],
            vec![p.original_id.as_str(), n.id.as_str(), n.id.as_str()],
            vec![
                p.original_id.as_str(),
                p.staged_id.as_str(),
                n.id.as_str(),
                "foreign",
            ],
        ] {
            assert!(require_editor_ids(&p, &n, &ids).is_err());
        }
        for field in ["id", "parent_id", "name", "size", "package", "kind", "etag"] {
            let mut encoded = serde_json::to_value(&n).expect("synthetic node");
            encoded[field] = match field {
                "id" => serde_json::json!(p.original_id),
                "size" => serde_json::json!(4),
                "package" => serde_json::json!(true),
                "kind" => serde_json::json!(NodeKind::Folder),
                "etag" => serde_json::Value::Null,
                _ => serde_json::json!("foreign"),
            };
            let foreign: Node = serde_json::from_value(encoded).expect("synthetic changed node");
            assert!(validate_editor(&p, Some(&foreign), 3).is_err(), "{field}");
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
            let mut value = serde_json::to_value(&p).expect("synthetic handoff");
            value[field] = serde_json::json!("unrelated");
            let other = serde_json::from_value(value).expect("synthetic changed handoff");
            assert!(validate_fixture(&other, b"old", b"new").is_err(), "{field}");
        }
        assert!(validate_fixture(&p, b"changed", b"new").is_err());
        assert!(validate_fixture(&p, b"old", b"changed").is_err());
        assert!(validate_fixture(&p, b"", b"new").is_err());
        assert!(validate_fixture(&p, &vec![0; 4097], b"new").is_err());
    }
}
