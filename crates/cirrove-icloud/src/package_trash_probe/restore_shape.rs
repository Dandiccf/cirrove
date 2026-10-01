//! Read-only exact-owned recovery-path diagnosis. No restore transport exists here.
use super::*;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct RestorePathShape {
    pub json_type: &'static str,
    pub bounded: bool,
    pub segment_count: Option<u16>,
    pub leading_slash: bool,
    pub trailing_slash: bool,
    pub matches_owned_parent: bool,
    pub matches_owned_item: bool,
    pub matches_owned_relative_form: bool,
    pub matches_owned_absolute_form: bool,
}
#[derive(Debug, Serialize)]
pub struct OwnedPackageRestoreShape {
    pub restore_path: RestorePathShape,
    pub exact_trash_metadata_stable: bool,
    pub semantic_recovery_verified: bool,
    pub owned_parent_stable_and_vacant: bool,
    pub restore_authorized: bool,
}

fn shape(value: Option<&Value>, parent: &str, item: &str) -> RestorePathShape {
    let mut output = RestorePathShape {
        json_type: match value {
            None => "missing",
            Some(Value::Null) => "null",
            Some(Value::String(_)) => "string",
            Some(Value::Array(_)) => "array",
            Some(Value::Object(_)) => "object",
            Some(Value::Bool(_)) => "boolean",
            Some(Value::Number(_)) => "number",
        },
        bounded: false,
        segment_count: None,
        leading_slash: false,
        trailing_slash: false,
        matches_owned_parent: false,
        matches_owned_item: false,
        matches_owned_relative_form: false,
        matches_owned_absolute_form: false,
    };
    let Some(Value::String(path)) = value else {
        return output;
    };
    if path.len() > 4096 || path.split('/').take(65).count() > 64 {
        output.bounded = false;
        return output;
    }
    output.bounded = true;
    output.segment_count = Some(path.split('/').count() as u16);
    output.leading_slash = path.starts_with('/');
    output.trailing_slash = path.ends_with('/');
    let relative = path.strip_prefix('/').unwrap_or(path);
    let parts: Vec<_> = relative.split('/').collect();
    if parts.len() == 2 {
        output.matches_owned_parent = parts[0] == parent;
        output.matches_owned_item = parts[1] == item;
    }
    // Exact string forms only: never decode, normalize dot-components or follow
    // provider names to infer a destination identity.
    output.matches_owned_relative_form = path == &format!("{parent}/{item}");
    output.matches_owned_absolute_form = path == &format!("/{parent}/{item}");
    output
}

impl OwnedPackageTrashProbe {
    pub async fn restore_shape(
        mut session: ICloudReadSession,
        apple_account: &str,
        plan: &OwnedPackagePlan,
        probe_state: &Path,
        staging: &Path,
        cancel: &CancellationToken,
    ) -> Result<OwnedPackageRestoreShape> {
        ensure!(
            plan.operation == uuid::Uuid::parse_str("97be33d2-b216-49bf-9e49-465b1ca85d1d")?,
            "restore shape requires the exact owned run"
        );
        private_directory(probe_state)?;
        private_directory(staging)?;
        let vault = SealedPackageTrashCheckpointVault::new(probe_state, &plan.scope.account)?;
        let secret = vault
            .load(&SealedPackageTrashCheckpointVault::key(
                &plan.scope.account,
                plan.operation,
            ))
            .await?
            .context("package Trash checkpoint absent")?;
        ensure!(
            secret.expose_secret().len() <= MAX_CHECKPOINT,
            "package Trash checkpoint exceeds bound"
        );
        let saved: Checkpoint = serde_json::from_str(secret.expose_secret())
            .map_err(|_| anyhow::anyhow!("invalid package Trash checkpoint"))?;
        saved.validate(&session, plan, apple_account)?;
        observe(&mut session, &saved, apple_account, staging, cancel).await
    }
}

async fn parent(session: &mut ICloudReadSession, saved: &Checkpoint) -> Result<DriveEntry> {
    let parent = session.active_folder_metadata(&saved.plan.parent).await?;
    ensure!(
        parent.kind == "FOLDER"
            && parent.zone == "com.apple.CloudDocs"
            && parent.drivewsid == saved.plan.parent
            && parent.parent_id == ROOT_ID
            && parent.display_name() == saved.plan.parent_name,
        "restore shape owned parent changed"
    );
    let entries = session.list_folder(&saved.plan.parent).await?;
    ensure!(
        entries.len() == 1 && entries[0] == saved.plan.source,
        "restore shape destination is not the unchanged owned source-only folder"
    );
    Ok(parent)
}

pub(super) async fn observe(
    session: &mut ICloudReadSession,
    saved: &Checkpoint,
    apple_account: &str,
    staging: &Path,
    cancel: &CancellationToken,
) -> Result<OwnedPackageRestoreShape> {
    ensure!(
        saved.version == 2
            && saved.phase == PackageTrashPhase::Recovered
            && saved.revision_refusal
                == Some(PackageTrashRefusal::ItemEtagConflict { http_status: 200 }),
        "restore shape requires the completed bound-refusal arm"
    );
    tokio::select! { biased;
        _ = cancel.cancelled() => anyhow::bail!("restore shape cancelled"),
        result = async {
            let request = OwnedPackageTrashRequest {
                apple_account: apple_account.into(), drive_id: saved.imported.drivewsid.clone(),
                document_id: saved.imported.docwsid.clone(), expected_root: saved.renamed_name(),
                semantic: saved.semantic.clone(),
            };
            let before = crate::package_trash::observation(&session.item_details(&request.drive_id).await?, &request)?;
            let entry: DriveEntry = serde_json::from_value(before.clone()).map_err(|_| anyhow::anyhow!("restore shape target metadata invalid"))?;
            ensure!(entry.display_name() == saved.renamed_name(), "restore shape target name changed");
            let owned_parent = parent(session, saved).await?;
            let verified = session.verify_owned_package_in_trash(request, anonymous_staging(staging)?, cancel).await?;
            ensure!(before["etag"].as_str() == Some(verified.trash_etag.as_str()), "restore shape Trash revision changed");
            let request = OwnedPackageTrashRequest {
                apple_account: apple_account.into(), drive_id: saved.imported.drivewsid.clone(),
                document_id: saved.imported.docwsid.clone(), expected_root: saved.renamed_name(),
                semantic: saved.semantic.clone(),
            };
            let after = crate::package_trash::observation(&session.item_details(&request.drive_id).await?, &request)?;
            ensure!(after == before, "restore shape Trash metadata changed");
            ensure!(parent(session, saved).await? == owned_parent, "restore shape owned parent changed");
            ensure!(!cancel.is_cancelled(), "restore shape cancelled");
            Ok(OwnedPackageRestoreShape {
                restore_path: shape(before.get("restorePath"), &saved.plan.parent_name, &saved.renamed_name()),
                exact_trash_metadata_stable: true, semantic_recovery_verified: true,
                owned_parent_stable_and_vacant: true, restore_authorized: false,
            })
        } => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_restore_shape_reports_only_bounded_forms_without_private_values() {
        let parent = "Private Owned Parent";
        let item = "Private Owned Item.pages";
        for (value, expected_type, relative, absolute) in [
            (
                Value::String(format!("{parent}/{item}")),
                "string",
                true,
                false,
            ),
            (
                Value::String(format!("/{parent}/{item}")),
                "string",
                false,
                true,
            ),
            (serde_json::json!([parent, item]), "array", false, false),
            (
                Value::String(format!("Other/{item}")),
                "string",
                false,
                false,
            ),
            (
                Value::String(format!("{parent}/../{item}")),
                "string",
                false,
                false,
            ),
            (
                Value::String(format!("//{parent}/{item}")),
                "string",
                false,
                false,
            ),
            (Value::Null, "null", false, false),
        ] {
            let report = shape(Some(&value), parent, item);
            assert_eq!(report.json_type, expected_type);
            assert_eq!(report.bounded, expected_type == "string");
            assert_eq!(report.matches_owned_relative_form, relative);
            assert_eq!(report.matches_owned_absolute_form, absolute);
            let encoded = serde_json::to_string(&report).unwrap();
            assert!(!encoded.contains(parent) && !encoded.contains(item));
        }
        for excessive in ["x".repeat(4097), "x/".repeat(65)] {
            let report = shape(Some(&Value::String(excessive)), parent, item);
            assert!(!report.bounded && report.segment_count.is_none());
        }
    }
}
