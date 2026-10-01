//! Exact97be one-shot restore experiment. No installed provider routes here.
use super::*;
use crate::sealed_session::SealedPackageRestoreCheckpointVault;

const OWNED_RUN: &str = "97be33d2-b216-49bf-9e49-465b1ca85d1d";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageRestorePhase {
    Prepared,
    RestoreArmed,
    ReceiptObserved,
    Verified,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreCheckpoint {
    pub(super) version: u8,
    pub(super) source: Checkpoint,
    pub(super) phase: PackageRestorePhase,
    pub(super) trash_etag: String,
    pub(super) receipt_etag: Option<String>,
    pub(super) receipt_http_status: Option<u16>,
}
impl RestoreCheckpoint {
    fn key(&self) -> String {
        SealedPackageRestoreCheckpointVault::key(
            &self.source.plan.scope.account,
            self.source.plan.operation,
        )
    }
    pub(super) fn validate(
        &self,
        session: &ICloudReadSession,
        account: &str,
        plan: &OwnedPackagePlan,
    ) -> Result<()> {
        self.source.validate(session, plan, account)?;
        ensure!(
            self.version == 1
                && self.source.version == 2
                && self.source.phase == PackageTrashPhase::Recovered
                && self.source.plan.operation == uuid::Uuid::parse_str(OWNED_RUN)?
                && self.source.revision_refusal
                    == Some(PackageTrashRefusal::ItemEtagConflict { http_status: 200 })
                && revision(&self.trash_etag),
            "owned restore checkpoint identity mismatch"
        );
        let receipt = matches!(
            self.phase,
            PackageRestorePhase::ReceiptObserved | PackageRestorePhase::Verified
        );
        ensure!(
            if receipt {
                self.receipt_etag.as_deref().is_some_and(revision)
                    && self
                        .receipt_http_status
                        .is_some_and(|code| (200..300).contains(&code))
            } else {
                self.receipt_etag.is_none() && self.receipt_http_status.is_none()
            },
            "owned restore checkpoint receipt mismatch"
        );
        Ok(())
    }
}
fn revision(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.contains(['\0', '\r', '\n', '*'])
}
#[derive(Debug, Serialize)]
pub struct PackageRestoreInspection {
    pub phase: PackageRestorePhase,
    pub restore_receipt_recorded: bool,
    pub receipt_http_status: Option<u16>,
    pub still_recoverable_trash: bool,
    pub restored_same_identity_and_semantics: bool,
    pub restored_name_preserved: Option<bool>,
    pub owned_source_semantics_verified: bool,
    pub destination_vacancy_atomicity_proven: bool,
}
pub struct OwnedPackageRestoreProbe {
    pub(super) session: ICloudReadSession,
    pub(super) apple_account: String,
    pub(super) saved: RestoreCheckpoint,
    pub(super) vault: Arc<dyn CredentialVault>,
    pub(super) staging: PathBuf,
    pub(super) _marker: File,
}
impl OwnedPackageRestoreProbe {
    pub async fn prepare(
        mut session: ICloudReadSession,
        apple_account: String,
        plan: &OwnedPackagePlan,
        trash_state: &Path,
        restore_state: &Path,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        ensure!(
            plan.operation == uuid::Uuid::parse_str(OWNED_RUN)?,
            "restore requires the exact owned run"
        );
        separate_directories(trash_state, restore_state)?;
        let trash = SealedPackageTrashCheckpointVault::new(trash_state, &plan.scope.account)?;
        let secret = trash
            .load(&SealedPackageTrashCheckpointVault::key(
                &plan.scope.account,
                plan.operation,
            ))
            .await?
            .context("owned Trash checkpoint absent")?;
        ensure!(
            secret.expose_secret().len() <= MAX_CHECKPOINT,
            "owned Trash checkpoint exceeds bound"
        );
        let source: Checkpoint = serde_json::from_str(secret.expose_secret())
            .map_err(|_| anyhow::anyhow!("invalid owned Trash checkpoint"))?;
        source.validate(&session, plan, &apple_account)?;
        let trash_etag =
            preflight(&mut session, &source, &apple_account, restore_state, cancel).await?;
        let saved = RestoreCheckpoint {
            version: 1,
            source,
            phase: PackageRestorePhase::Prepared,
            trash_etag,
            receipt_etag: None,
            receipt_http_status: None,
        };
        saved.validate(&session, &apple_account, plan)?;
        // The permanent source-specific marker prevents another invocation from
        // sending a second restore even if its checkpoint becomes unavailable.
        let marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(restore_state.join(format!("package-restore-{}.owner", plan.operation)))
            .context("owned restore already started or unavailable")?;
        marker.sync_all()?;
        File::open(restore_state)?.sync_all()?;
        let vault = Arc::new(SealedPackageRestoreCheckpointVault::new(
            restore_state,
            &plan.scope.account,
        )?);
        ensure!(
            vault.load(&saved.key()).await?.is_none(),
            "owned restore checkpoint already exists"
        );
        let mut this = Self {
            session,
            apple_account,
            saved,
            vault,
            staging: restore_state.into(),
            _marker: marker,
        };
        this.persist(PackageRestorePhase::Prepared).await?;
        Ok(this)
    }
    async fn persist(&mut self, phase: PackageRestorePhase) -> Result<()> {
        self.saved.phase = phase;
        let json = serde_json::to_string(&self.saved)?;
        ensure!(
            json.len() <= MAX_CHECKPOINT,
            "owned restore checkpoint exceeds bound"
        );
        self.vault
            .save(&self.saved.key(), SecretString::from(json))
            .await
    }
    /// Consumes a newly prepared owner. There is no load-and-execute/resume API.
    pub async fn execute(mut self, cancel: &CancellationToken) -> Result<PackageRestoreInspection> {
        ensure!(
            self.saved.phase == PackageRestorePhase::Prepared && !cancel.is_cancelled(),
            "owned restore is not eligible"
        );
        let current = preflight(
            &mut self.session,
            &self.saved.source,
            &self.apple_account,
            &self.staging,
            cancel,
        )
        .await?;
        ensure!(
            current == self.saved.trash_etag,
            "owned restore Trash revision changed before arming"
        );
        self.persist(PackageRestorePhase::RestoreArmed).await?;
        ensure!(!cancel.is_cancelled(), "owned restore cancelled");
        let receipt = self
            .session
            .send_probe_restore(
                &self.saved.source.imported.drivewsid,
                &self.saved.source.imported.docwsid,
                &self.saved.trash_etag,
                &self.saved.source.plan.parent,
            )
            .await?;
        self.saved.receipt_etag = Some(receipt.etag);
        self.saved.receipt_http_status = Some(receipt.http_status);
        self.persist(PackageRestorePhase::ReceiptObserved).await?;
        let preserved =
            verify_active(&mut self.session, &self.saved, &self.staging, cancel).await?;
        self.persist(PackageRestorePhase::Verified).await?;
        Ok(report(&self.saved, false, Some(preserved)))
    }
    /// Observes retained history and exact identities. Never resends any request.
    pub async fn inspect(
        mut session: ICloudReadSession,
        apple_account: &str,
        plan: &OwnedPackagePlan,
        restore_state: &Path,
        staging: &Path,
        cancel: &CancellationToken,
    ) -> Result<PackageRestoreInspection> {
        private_directory(restore_state)?;
        private_directory(staging)?;
        ensure!(
            plan.operation == uuid::Uuid::parse_str(OWNED_RUN)?,
            "restore inspection requires the exact owned run"
        );
        let vault = SealedPackageRestoreCheckpointVault::new(restore_state, &plan.scope.account)?;
        let secret = vault
            .load(&SealedPackageRestoreCheckpointVault::key(
                &plan.scope.account,
                plan.operation,
            ))
            .await?
            .context("owned restore checkpoint absent")?;
        ensure!(
            secret.expose_secret().len() <= MAX_CHECKPOINT,
            "owned restore checkpoint exceeds bound"
        );
        let saved: RestoreCheckpoint = serde_json::from_str(secret.expose_secret())
            .map_err(|_| anyhow::anyhow!("invalid owned restore checkpoint"))?;
        saved.validate(&session, apple_account, plan)?;
        inspect_saved_restore(&mut session, &saved, apple_account, staging, cancel).await
    }
}
async fn preflight(
    session: &mut ICloudReadSession,
    source: &Checkpoint,
    account: &str,
    staging: &Path,
    cancel: &CancellationToken,
) -> Result<String> {
    let (shape, observed) =
        restore_shape::observe_bound(session, source, account, staging, cancel).await?;
    ensure!(
        shape.restore_path.bounded
            && shape.restore_path.matches_owned_relative_form
            && shape.restore_path.matches_owned_parent
            && shape.restore_path.matches_owned_item,
        "owned restore path is not the observed exact relative form"
    );
    let etag = observed["etag"]
        .as_str()
        .filter(|etag| revision(etag))
        .context("owned restore Trash revision missing")?;
    Ok(etag.into())
}
fn report(
    saved: &RestoreCheckpoint,
    trash: bool,
    active_name: Option<bool>,
) -> PackageRestoreInspection {
    PackageRestoreInspection {
        phase: saved.phase,
        restore_receipt_recorded: saved.receipt_http_status.is_some(),
        receipt_http_status: saved.receipt_http_status,
        still_recoverable_trash: trash,
        restored_same_identity_and_semantics: active_name.is_some(),
        restored_name_preserved: active_name,
        owned_source_semantics_verified: active_name.is_some(),
        destination_vacancy_atomicity_proven: false,
    }
}
pub(super) async fn inspect_saved_restore(
    session: &mut ICloudReadSession,
    saved: &RestoreCheckpoint,
    account: &str,
    staging: &Path,
    cancel: &CancellationToken,
) -> Result<PackageRestoreInspection> {
    ensure!(!cancel.is_cancelled(), "owned restore inspection cancelled");
    let item = session
        .item_details(&saved.source.imported.drivewsid)
        .await?;
    if matches!(
        item.get("parentId").and_then(serde_json::Value::as_str),
        Some("TRASH_ROOT" | crate::write_transport::TRASH_ROOT)
    ) {
        let etag = preflight(session, &saved.source, account, staging, cancel).await?;
        ensure!(
            etag == saved.trash_etag,
            "owned restore retained Trash revision changed"
        );
        return Ok(report(saved, true, None));
    }
    let preserved = verify_active(session, saved, staging, cancel).await?;
    Ok(report(saved, false, Some(preserved)))
}
fn active_entry(item: serde_json::Value, saved: &RestoreCheckpoint) -> Result<DriveEntry> {
    ensure!(
        item.get("restorePath")
            .is_none_or(serde_json::Value::is_null),
        "owned restore item remains recoverable"
    );
    let entry: DriveEntry = serde_json::from_value(item)
        .map_err(|_| anyhow::anyhow!("owned restore active metadata invalid"))?;
    ensure!(
        entry.drivewsid == saved.source.imported.drivewsid
            && entry.docwsid == saved.source.imported.docwsid
            && entry.kind == "FILE"
            && entry.zone == "com.apple.CloudDocs"
            && entry.parent_id == saved.source.plan.parent
            && revision(&entry.etag)
            && !entry.display_name().is_empty()
            && saved
                .receipt_etag
                .as_ref()
                .is_none_or(|etag| etag == &entry.etag),
        "owned restore active identity or revision changed"
    );
    Ok(entry)
}
/// Fixed diagnostic keys, counts and comparison booleans only. Never serialize
/// either provider node or a provider-controlled string into an error/report.
fn inventory_diagnostic(
    entries: &[DriveEntry],
    source: &DriveEntry,
    target: &DriveEntry,
) -> serde_json::Value {
    fn binding(entries: &[DriveEntry], expected: &DriveEntry) -> serde_json::Value {
        let mut candidates = entries
            .iter()
            .filter(|entry| entry.drivewsid == expected.drivewsid);
        let first = candidates.next();
        let count = usize::from(first.is_some()) + candidates.count();
        let candidate = first.filter(|_| count == 1);
        serde_json::json!({
            "id_matches": count,
            "exact_matches": entries.iter().filter(|entry| *entry == expected).count(),
            "unique_id_match": candidate.is_some(),
            "etag_equal": candidate.is_some_and(|entry| entry.etag == expected.etag),
            "size_equal": candidate.is_some_and(|entry| entry.size == expected.size),
            "name_equal": candidate.is_some_and(|entry| entry.name == expected.name),
            "extension_equal": candidate.is_some_and(|entry| entry.extension == expected.extension),
            "display_name_equal": candidate.is_some_and(|entry| entry.display_name() == expected.display_name()),
            "parent_equal": candidate.is_some_and(|entry| entry.parent_id == expected.parent_id),
            "kind_equal": candidate.is_some_and(|entry| entry.kind == expected.kind),
            "document_id_equal": candidate.is_some_and(|entry| entry.docwsid == expected.docwsid),
            "zone_equal": candidate.is_some_and(|entry| entry.zone == expected.zone),
            "item_id_equal": candidate.is_some_and(|entry| entry.item_id == expected.item_id),
            "items_equal": candidate.is_some_and(|entry| entry.items == expected.items),
            "number_of_items_equal": candidate.is_some_and(|entry| entry.number_of_items == expected.number_of_items)
        })
    }
    serde_json::json!({"entry_count": entries.len(), "source": binding(entries, source), "target": binding(entries, target)})
}

/// Compare list/detail representations of this already identity-bound target.
/// Apple's Drive projection caches optional item_id by drivewsid and fills it
/// into otherwise missing responses (public Build2636Build19, byte18469).
/// Every other field remains exact; the untouched source guard stays full Eq.
fn same_restored_target(listed: &DriveEntry, detailed: &DriveEntry) -> bool {
    listed.drivewsid == detailed.drivewsid
        && listed.docwsid == detailed.docwsid
        && listed.zone == detailed.zone
        && listed.name == detailed.name
        && listed.extension == detailed.extension
        && listed.parent_id == detailed.parent_id
        && listed.etag == detailed.etag
        && listed.kind == detailed.kind
        && listed.size == detailed.size
        && listed.items == detailed.items
        && listed.number_of_items == detailed.number_of_items
}

async fn active_parent(
    session: &mut ICloudReadSession,
    saved: &RestoreCheckpoint,
    restored: &DriveEntry,
) -> Result<DriveEntry> {
    let plan = &saved.source.plan;
    let parent = session.active_folder_metadata(&plan.parent).await?;
    ensure!(
        parent.kind == "FOLDER"
            && parent.zone == "com.apple.CloudDocs"
            && parent.drivewsid == plan.parent
            && parent.parent_id == ROOT_ID
            && parent.display_name() == plan.parent_name,
        "owned restore destination changed"
    );
    let entries = session.list_folder(&plan.parent).await?;
    ensure!(
        entries.len() == 2
            && entries.iter().filter(|e| *e == &plan.source).count() == 1
            && entries
                .iter()
                .filter(|e| same_restored_target(e, restored))
                .count()
                == 1,
        "owned restore destination inventory changed: {}",
        inventory_diagnostic(&entries, &plan.source, restored)
    );
    Ok(parent)
}
async fn verify_active(
    session: &mut ICloudReadSession,
    saved: &RestoreCheckpoint,
    staging: &Path,
    cancel: &CancellationToken,
) -> Result<bool> {
    ensure!(
        !cancel.is_cancelled(),
        "owned restore verification cancelled"
    );
    let entry = active_entry(
        session
            .item_details(&saved.source.imported.drivewsid)
            .await?,
        saved,
    )?;
    let parent = active_parent(session, saved, &entry).await?;
    // Both returned target and untouched source must keep their root-relative
    // native semantics. A provider-selected restored name is observed, not set.
    for selected in [&entry, &saved.source.plan.source] {
        let file = anonymous_staging(staging)?;
        let mut sink = Sink(tokio::fs::File::from_std(file.try_clone()?));
        let receipt = session
            .download_package(
                &saved.source.plan.parent,
                selected,
                MAX_ARCHIVE,
                &mut sink,
                cancel,
            )
            .await?;
        sink.0.flush().await?;
        drop(sink);
        verify_archive(
            file,
            receipt,
            selected.display_name(),
            saved.source.semantic.clone(),
            cancel,
        )
        .await?;
    }
    let after = active_entry(session.item_details(&entry.drivewsid).await?, saved)?;
    ensure!(
        after == entry && active_parent(session, saved, &entry).await? == parent,
        "owned restore metadata changed during verification"
    );
    ensure!(
        !cancel.is_cancelled(),
        "owned restore verification cancelled"
    );
    Ok(entry.display_name() == saved.source.renamed_name())
}

#[cfg(test)]
mod inventory_tests {
    use super::*;
    fn entry(id: &str, name: &str) -> DriveEntry {
        serde_json::from_value(serde_json::json!({
            "drivewsid":id,"docwsid":id,"name":name,"extension":"pages",
            "zone":"com.apple.CloudDocs","parentId":"private-parent-id",
            "type":"FILE","etag":"private-revision","size":4
        }))
        .expect("synthetic fixture")
    }
    #[test]
    fn restore_inventory_diagnostic_distinguishes_source_change_from_target_projection_without_values()
     {
        let source = entry("private-source-id", "private-source-name");
        let target = entry("private-target-id", "private-target-name");
        let mut changed = source.clone();
        changed.etag = "new-private-revision".into();
        let mut projected = target.clone();
        projected.name = target.display_name();
        projected.extension.clear();
        projected.item_id = "private-extra-item-id".into();
        let report = inventory_diagnostic(&[changed, projected], &source, &target);
        assert_eq!(report["entry_count"], 2);
        assert_eq!(report["source"]["unique_id_match"], true);
        assert_eq!(report["source"]["exact_matches"], 0);
        assert_eq!(report["source"]["etag_equal"], false);
        assert_eq!(report["source"]["name_equal"], true);
        assert_eq!(report["target"]["display_name_equal"], true);
        assert_eq!(report["target"]["name_equal"], false);
        assert_eq!(report["target"]["extension_equal"], false);
        assert_eq!(report["target"]["item_id_equal"], false);
        assert_eq!(report["target"]["etag_equal"], true);
        assert_eq!(report["target"]["document_id_equal"], true);
        let encoded = serde_json::to_string(&report).expect("synthetic fixture");
        assert!(!encoded.contains("private-"));
        let ambiguous = inventory_diagnostic(&[target.clone(), target.clone()], &source, &target);
        assert_eq!(ambiguous["source"]["id_matches"], 0);
        assert_eq!(ambiguous["target"]["id_matches"], 2);
        assert_eq!(ambiguous["target"]["unique_id_match"], false);
        assert_eq!(ambiguous["target"]["etag_equal"], false);
        let matched = inventory_diagnostic(&[source.clone(), target.clone()], &source, &target);
        assert_eq!(matched["source"]["exact_matches"], 1);
        assert_eq!(matched["target"]["exact_matches"], 1);
    }
}
