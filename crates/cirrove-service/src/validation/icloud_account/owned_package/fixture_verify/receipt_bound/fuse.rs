//! Explicit Numbers, Pages and Keynote canonical PACKAGE FUSE-save proofs.
//! These are archive-copy observations, not native editor-save acceptance.
//! Offline source inspection and read-only receipt proof never submit a write.
use super::*;
use crate::journal::WorkingFile;

// Only explicit entry points select these implemented contracts. No caller wire
// field, extension inference or provider representation override selects an arm.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum FuseFormat {
    #[default]
    Numbers,
    Pages,
    Keynote,
}
impl FuseFormat {
    fn application(self) -> &'static str {
        match self {
            Self::Numbers => "Numbers",
            Self::Pages => "Pages",
            Self::Keynote => "Keynote",
        }
    }
    fn format(self) -> &'static str {
        match self {
            Self::Numbers => "numbers",
            Self::Pages => "pages",
            Self::Keynote => "keynote",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Numbers => "numbers",
            Self::Pages => "pages",
            Self::Keynote => "key",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Numbers => "iCloudNumbersFuseValidation",
            Self::Pages => "iCloudPagesFuseValidation",
            Self::Keynote => "iCloudKeynoteFuseValidation",
        }
    }
    fn source_root(self) -> String {
        format!("Source.{}", self.extension())
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FuseWindow {
    run: Uuid,
    started_unix_ms: u64,
    deadline_unix_ms: u64,
    cleanup_reserve_seconds: u64,
}
impl FuseWindow {
    fn remaining_at(&self, run: Uuid, now_ms: u64) -> Result<std::time::Duration> {
        ensure!(
            self.run == run
                && !run.is_nil()
                && self.cleanup_reserve_seconds == 120
                && self
                    .deadline_unix_ms
                    .checked_sub(self.started_unix_ms)
                    .is_some_and(|span| span > 120_000 && span <= 1_200_000)
                && self.started_unix_ms <= now_ms,
            "owned FUSE original window refused"
        );
        let active = self
            .deadline_unix_ms
            .checked_sub(120_000)
            .context("owned FUSE active deadline refused")?;
        let remaining = active
            .checked_sub(now_ms)
            .filter(|value| *value > 0)
            .context("owned FUSE original deadline expired")?;
        Ok(std::time::Duration::from_millis(remaining))
    }
}
fn fuse_active(end: Option<tokio::time::Instant>) -> Result<()> {
    if let Some(end) = end {
        ensure!(
            tokio::time::Instant::now() < end,
            "owned FUSE original deadline expired"
        );
    }
    Ok(())
}
fn fuse_publish(
    end: Option<tokio::time::Instant>,
    publish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    fuse_active(end)?;
    publish()?;
    fuse_active(end)
}

type Association = (
    Uuid,
    Uuid,
    NamespaceObject,
    NamespaceObject,
    Option<WorkingFile>,
);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Sources {
    #[serde(skip)]
    format: FuseFormat,
    version: u32,
    run: Uuid,
    session_directory: PathBuf,
    source_a: Source,
    source_b_original: Source,
    source_b: Source,
}
impl Sources {
    fn name(&self) -> String {
        format!(
            "Cirrove-{}-Parent-{}.{}",
            self.format.application(),
            self.run,
            self.format.extension()
        )
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1
                && !self.run.is_nil()
                && self.session_directory
                    == format!(
                        "/var/tmp/cirrove-{}-fuse-{}",
                        self.format.format(),
                        self.run
                    ),
            "owned FUSE source scope refused"
        );
        for (source, name, root) in [
            (
                &self.source_a,
                format!("source-a.{}", self.format.extension()),
                self.format.source_root(),
            ),
            (
                &self.source_b_original,
                format!("source-b-original.{}", self.format.extension()),
                self.format.source_root(),
            ),
            (
                &self.source_b,
                format!("source-b-fuse.{}", self.format.extension()),
                self.name(),
            ),
        ] {
            source.semantic.validate()?;
            ensure!(
                source.path == self.session_directory.join(name)
                    && source.root == root
                    && source.size > 0
                    && source.size <= LIMIT
                    && hex_digest(&source.sha256)
                    && source.semantic.version == 2,
                "owned FUSE source registration refused"
            );
        }
        ensure!(
            self.source_a.semantic != self.source_b_original.semantic
                && self.source_a.sha256 != self.source_b_original.sha256
                && self.source_b_original.semantic == self.source_b.semantic,
            "owned FUSE rewrite content binding refused"
        );
        Ok(())
    }
    fn verify(&self) -> Result<()> {
        self.validate()?;
        source_verified(&self.source_a)?;
        source_verified(&self.source_b_original)?;
        source_verified(&self.source_b)?;
        Ok(())
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FuseRegistration {
    #[serde(skip)]
    format: FuseFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_window: Option<FuseWindow>,
    version: u32,
    run: Uuid,
    account: Uuid,
    label: String,
    session_directory: PathBuf,
    settings_sha256: String,
    parent_creation: Uuid,
    import: Uuid,
    save: Option<Uuid>,
    working: Option<Uuid>,
    owner: Option<Uuid>,
    source_a: Source,
    source_b_original: Source,
    source_b: Source,
}
impl FuseRegistration {
    fn sources(&self) -> Sources {
        Sources {
            format: self.format,
            version: self.version,
            run: self.run,
            session_directory: self.session_directory.clone(),
            source_a: self.source_a.clone(),
            source_b_original: self.source_b_original.clone(),
            source_b: self.source_b.clone(),
        }
    }
    fn validate(&self) -> Result<()> {
        self.sources().validate()?;
        ensure!(
            !self.account.is_nil()
                && self.label == self.format.label()
                && hex_digest(&self.settings_sha256)
                && !self.parent_creation.is_nil()
                && !self.import.is_nil()
                && self.parent_creation != self.import
                && match (self.save, self.working, self.owner) {
                    (None, None, None) => true,
                    (Some(save), Some(working), Some(owner)) =>
                        !save.is_nil()
                            && !working.is_nil()
                            && !owner.is_nil()
                            && save != self.import
                            && save != self.parent_creation
                            && working != owner,
                    _ => false,
                },
            "owned FUSE receipt registration refused"
        );
        ensure!(
            self.format == FuseFormat::Numbers || self.original_window.is_some(),
            "owned FUSE original window missing"
        );
        if let Some(window) = &self.original_window {
            window.remaining_at(self.run, window.started_unix_ms)?;
        }
        Ok(())
    }
    fn active_end(&self) -> Result<Option<tokio::time::Instant>> {
        match &self.original_window {
            None => Ok(None),
            Some(window) => {
                // Monotonic sample precedes wall sample; no renewed phase budget.
                let mono = tokio::time::Instant::now();
                let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
                let ms = u64::try_from(wall.as_millis())?;
                window.remaining_at(self.run, ms)?;
                let active = window
                    .deadline_unix_ms
                    .checked_sub(120_000)
                    .context("owned FUSE active deadline refused")?;
                let remaining = std::time::Duration::from_millis(active)
                    .checked_sub(wall)
                    .filter(|value| !value.is_zero())
                    .context("owned FUSE original deadline expired")?;
                Ok(Some(mono + remaining))
            }
        }
    }
    // Reuse unchanged exact receipt validators, never the legacy scope validator.
    // The public legacy wrapper still requires Source.numbers for both sources.
    fn receipt_plan(&self) -> Registration {
        Registration {
            version: self.version,
            run: self.run,
            account: self.account,
            label: self.label.clone(),
            session_directory: self.session_directory.clone(),
            settings_sha256: self.settings_sha256.clone(),
            parent_creation: self.parent_creation,
            import: self.import,
            replacement: self.save,
            source_a: self.source_a.clone(),
            source_b: self.source_b.clone(),
        }
    }
}
fn registered_fuse_for(bytes: &[u8], digest: &str, format: FuseFormat) -> Result<FuseRegistration> {
    ensure!(
        bytes.len() <= 32 * 1024
            && hex_digest(digest)
            && hex::encode(Sha256::digest(bytes)) == digest,
        "owned FUSE registration digest changed"
    );
    let mut plan: FuseRegistration = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("owned FUSE registration schema refused"))?;
    if format == FuseFormat::Numbers {
        let wire: serde_json::Value = serde_json::from_slice(bytes)?;
        ensure!(
            wire.get("original_window").is_none(),
            "legacy FUSE window field refused"
        );
    }
    plan.format = format;
    plan.validate()?;
    Ok(plan)
}
#[cfg(test)]
fn registered_fuse(bytes: &[u8], digest: &str) -> Result<FuseRegistration> {
    registered_fuse_for(bytes, digest, FuseFormat::Numbers)
}
fn association_binding(
    plan: &FuseRegistration,
    row: &UploadRecord,
    imported_original: &Node,
    association: &Association,
) -> Result<()> {
    let (working, owner, source, child, file) = association;
    let (original, current, _) = row
        .native_replacement_receipt()
        .context("owned FUSE typed receipt absent")?;
    let role = child
        .native_archive
        .as_ref()
        .context("owned FUSE archive role absent")?;
    ensure!(
        plan.save == Some(row.id)
            && plan.working == Some(*working)
            && plan.owner == Some(*owner)
            && source.id == *owner
            && original == imported_original
            && source.scope == plan.receipt_plan().scope()
            && source.remote.as_ref() == Some(current)
            && source.remote_sequence == row.sequence
            && source.remote_owned
            && !source.unlinked
            && source.native_archive.is_none()
            && source.working_file.is_none()
            && source.node.kind == NodeKind::Folder
            && source.node.package
            && source.node.target.is_none()
            && source.node.name == plan.sources().name()
            && child.id == *working
            && child.scope == source.scope
            && child.valid_native_archive_owner(source)
            && role.working == *working
            && role.source_owner == *owner
            && !role.backed_up
            && role.artifact == format!("icloud-artifact:{}", current.id)
            && child.node.name == plan.sources().name(),
        "owned FUSE working association refused"
    );
    if role.retired {
        ensure!(
            file.is_none()
                && child.valid_retired_native_archive()
                && source.follows_remote
                && source.latest.is_none(),
            "owned FUSE retirement refused"
        );
    } else {
        let file = file.as_ref().context("owned FUSE working bytes absent")?;
        ensure!(
            child.valid_native_archive_shape(file)
                && file.native
                && !file.dirty
                && !file.unlinked
                && file.latest == plan.save
                && !source.follows_remote
                && source.latest == plan.save,
            "owned FUSE working generation unconfirmed"
        );
    }
    Ok(())
}
#[allow(clippy::type_complexity)]
fn fuse_journal_binding(
    plan: &FuseRegistration,
    journal: &RecoveryJournal,
) -> Result<(
    Vec<UploadRecord>,
    Node,
    Node,
    Option<Node>,
    Option<Association>,
    (i64, i64),
)> {
    plan.validate()?;
    let (rows, parent, current, backup) = journal_binding(&plan.receipt_plan(), journal)?;
    let association = if let Some(save) = plan.save {
        let actual = journal
            .native_validation_fuse_association(save)?
            .context("owned FUSE association missing")?;
        let imported_original = rows
            .iter()
            .find(|row| row.id == plan.import)
            .and_then(|row| row.remote.as_ref())
            .context("owned FUSE imported original missing")?;
        let row = rows
            .iter()
            .find(|row| row.id == save)
            .context("owned FUSE saved row missing")?;
        association_binding(plan, row, imported_original, &actual)?;
        Some(actual)
    } else {
        None
    };
    let inventory = journal.native_validation_working_inventory()?;
    if inventory == (2, 1) {
        let actual = association
            .as_ref()
            .context("owned FUSE detached association missing")?;
        ensure!(
            actual.4.is_some(),
            "owned FUSE detached canonical bytes missing"
        );
        journal.native_validation_detached_original(
            plan.save.context("owned FUSE detached save missing")?,
            actual.0,
            actual.1,
        )?;
        working_inventory_binding(association.as_ref(), (inventory.0 - 1, inventory.1 - 1))?;
    } else {
        working_inventory_binding(association.as_ref(), inventory)?;
    }
    Ok((rows, parent, current, backup, association, inventory))
}
fn working_inventory_binding(
    association: Option<&Association>,
    inventory: (i64, i64),
) -> Result<()> {
    let expected = i64::from(association.is_some_and(|actual| actual.4.is_some()));
    ensure!(
        inventory == (expected, 0),
        "owned FUSE working inventory changed"
    );
    Ok(())
}
fn verify_sources(path: &Path, digest: &str, format: FuseFormat) -> Result<serde_json::Value> {
    let bytes = read_private(path, 32 * 1024)?;
    ensure!(
        hex_digest(digest) && hex::encode(Sha256::digest(&bytes)) == digest,
        "owned FUSE source registration digest changed"
    );
    let mut plan: Sources = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("owned FUSE source registration schema refused"))?;
    plan.format = format;
    plan.validate()?;
    ensure!(
        path == plan.session_directory.join("source-registration.json"),
        "owned FUSE source registration location refused"
    );
    let directory = open_private(&plan.session_directory, true, 0)?;
    plan.verify()?;
    ensure!(
        read_private(path, 32 * 1024)? == bytes,
        "owned FUSE source registration changed"
    );
    same_directory(&plan.session_directory, &directory)?;
    let output = serde_json::json!({"run":plan.run,"registration_sha256":digest,
        "source_a":plan.source_a,"source_b_original":plan.source_b_original,"source_b":plan.source_b,
        "source_rewrite_verified":true,"offline_only":true,"cloud_mutated":false});
    record(
        &plan.session_directory.join("source-verified.json"),
        &output,
    )?;
    Ok(output)
}
/// Local source inspection only: no settings, journal, session or keyring opens.
pub fn icloud_owned_fuse_source_verify(path: &Path, digest: &str) -> Result<serde_json::Value> {
    verify_sources(path, digest, FuseFormat::Numbers)
        .map_err(|_| anyhow::anyhow!("owned FUSE source validation refused; no cloud access"))
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CapturePurpose {
    CanonicalA,
    HeldA,
    CurrentB,
    RemountedB,
}
impl CapturePurpose {
    fn stem(self) -> &'static str {
        match self {
            Self::CanonicalA => "canonical-a",
            Self::HeldA => "held-a",
            Self::CurrentB => "current-b",
            Self::RemountedB => "remounted-b",
        }
    }
    fn semantic(self, master: &Sources) -> &PackageSemanticIdentity {
        match self {
            Self::CanonicalA | Self::HeldA => &master.source_a.semantic,
            Self::CurrentB | Self::RemountedB => &master.source_b.semantic,
        }
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRegistration {
    version: u32,
    run: Uuid,
    session_directory: PathBuf,
    source_registration_sha256: String,
    purpose: CapturePurpose,
    source: Source,
}
fn capture_binding(plan: &CaptureRegistration, master: &Sources, path: &Path) -> Result<()> {
    ensure!(
        plan.version == 1
            && plan.run == master.run
            && plan.session_directory == master.session_directory
            && hex_digest(&plan.source_registration_sha256)
            && path
                == master
                    .session_directory
                    .join(format!("{}-registration.json", plan.purpose.stem()))
            && plan.source.path
                == master.session_directory.join(format!(
                    "{}.{}",
                    plan.purpose.stem(),
                    master.format.extension()
                ))
            && plan.source.root == master.name()
            && plan.source.semantic == *plan.purpose.semantic(master)
            && plan.source.size > 0
            && plan.source.size <= LIMIT
            && hex_digest(&plan.source.sha256),
        "owned FUSE capture binding refused"
    );
    source_verified(&plan.source)
}
fn verify_capture(path: &Path, digest: &str, format: FuseFormat) -> Result<serde_json::Value> {
    let bytes = read_private(path, 32 * 1024)?;
    ensure!(
        hex_digest(digest) && hex::encode(Sha256::digest(&bytes)) == digest,
        "owned FUSE capture registration digest changed"
    );
    let plan: CaptureRegistration = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("owned FUSE capture schema refused"))?;
    let master_path = plan.session_directory.join("source-registration.json");
    let master_bytes = read_private(&master_path, 32 * 1024)?;
    ensure!(
        hex_digest(&plan.source_registration_sha256)
            && hex::encode(Sha256::digest(&master_bytes)) == plan.source_registration_sha256,
        "owned FUSE capture master changed"
    );
    let mut master: Sources = serde_json::from_slice(&master_bytes)
        .map_err(|_| anyhow::anyhow!("owned FUSE capture master schema refused"))?;
    master.format = format;
    master.validate()?;
    let directory = open_private(&master.session_directory, true, 0)?;
    master.verify()?;
    capture_binding(&plan, &master, path)?;
    ensure!(
        read_private(path, 32 * 1024)? == bytes
            && read_private(&master_path, 32 * 1024)? == master_bytes,
        "owned FUSE capture registrations changed"
    );
    master.verify()?;
    source_verified(&plan.source)?;
    same_directory(&master.session_directory, &directory)?;
    let output = serde_json::json!({"run":plan.run,"purpose":plan.purpose,"registration_sha256":digest,
        "source_registration_sha256":plan.source_registration_sha256,"source":plan.source,
        "offline_only":true,"cloud_mutated":false});
    record(
        &master
            .session_directory
            .join(format!("{}-verified.json", plan.purpose.stem())),
        &output,
    )?;
    Ok(output)
}
/// Local captured archive proof; never opens settings, credentials or a provider.
pub fn icloud_owned_fuse_capture_verify(path: &Path, digest: &str) -> Result<serde_json::Value> {
    verify_capture(path, digest, FuseFormat::Numbers)
        .map_err(|_| anyhow::anyhow!("owned FUSE capture validation refused; no cloud access"))
}

async fn observe_fuse(path: &Path, digest: &str, format: FuseFormat) -> Result<serde_json::Value> {
    let bytes = read_private(path, 32 * 1024)?;
    let plan = registered_fuse_for(&bytes, digest, format)?;
    let active_end = plan.active_end()?;
    let receipt_plan = plan.receipt_plan();
    ensure!(
        path == plan.session_directory.join(if plan.save.is_some() {
            "fuse-postflight-registration.json"
        } else {
            "fuse-preflight-registration.json"
        }),
        "owned registration location refused"
    );
    let account = settings(&receipt_plan)?;
    plan.sources().verify()?;
    let journal_path = plan
        .session_directory
        .join("state/accounts")
        .join(account.id.as_str())
        .join("journal");
    let _journal_dir = open_private(&journal_path, true, 0)?;
    // Exclusive read-only account-owner lease, not a shared content mutex.
    // Each journal query drops its SQLite statement/transaction before HTTP.
    let journal = RecoveryJournal::open(&journal_path, &account.id)?;
    let binding = fuse_journal_binding(&plan, &journal)?;
    let frozen = serde_json::to_vec(&binding)?;
    let (_, parent, current, backup, _, _) = &binding;
    fuse_active(active_end)?;
    let mut remote = session(&plan.session_directory, &account).await?;
    fuse_active(active_end)?;
    let root = remote.list_folder(cirrove_icloud::ROOT_ID).await?;
    let mut parents = root.iter().filter(|e| e.drivewsid == parent.id);
    let mut parent_entry = parents.next().context("owned parent absent")?.clone();
    ensure!(
        parents.next().is_none()
            && parent_entry.kind == "FOLDER"
            && parent_entry.zone == "com.apple.CloudDocs"
            && parent_entry.name == receipt_plan.parent_name(),
        "owned active parent changed"
    );
    parent_entry.items.clear();
    parent_entry.number_of_items = None;
    fuse_active(active_end)?;
    let document = exact_entry(&remote.list_folder(&parent.id).await?, current)?;
    let source = if backup.is_some() {
        &plan.source_b
    } else {
        &plan.source_a
    };
    let manifest_path = plan.session_directory.join(if backup.is_some() {
        "fuse-postflight-fixture.json"
    } else {
        "fuse-preflight-fixture.json"
    });
    let manifest_value = serde_json::json!({"version":1,"run":plan.run,"account":plan.account,"session_directory":plan.session_directory,
        "settings_sha256":plan.settings_sha256,"parent":parent_entry,"document":document,"format":format.format(),"representation":"package",
        "source":source.path,"source_size":source.size,"source_sha256":source.sha256,"source_root":source.root,"expected_root":current.name,"semantic":source.semantic});
    record(&manifest_path, &manifest_value)?;
    let manifest_digest = hex::encode(Sha256::digest(read_private(&manifest_path, 32 * 1024)?));
    fuse_active(active_end)?;
    let current_proof = match active_end {
        Some(end) => {
            icloud_owned_fixture_verify_before(&manifest_path, &manifest_digest, end).await?
        }
        None => icloud_owned_fixture_verify(&manifest_path, &manifest_digest).await?,
    };
    let mut trash_proof = None;
    if let Some(backup) = backup.as_ref() {
        let attempt = verification_directory(&plan.session_directory)?;
        manifest(&attempt, plan.run, "owned-fuse-receipt-trash-read-only")?;
        let staging = tempfile::tempfile_in(&attempt)?;
        staging.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        fuse_active(active_end)?;
        let recovered = remote
            .verify_owned_package_in_trash(
                cirrove_icloud::OwnedPackageTrashRequest {
                    apple_account: account.identity.username.clone(),
                    drive_id: backup.id.clone(),
                    document_id: backup
                        .id
                        .strip_prefix("FILE::com.apple.CloudDocs::")
                        .context("owned backup identity invalid")?
                        .into(),
                    expected_root: receipt_plan.name(),
                    semantic: plan.source_a.semantic.clone(),
                },
                staging,
                &CancellationToken::new(),
            )
            .await?;
        trash_binding(backup, &plan.source_a.semantic, &recovered)?;
        trash_proof = Some(
            serde_json::json!({"item":backup.id,"etag":recovered.trash_etag,"semantic":recovered.semantic,"size":recovered.archive.size,"sha256":recovered.archive.sha256}),
        );
    }
    fuse_active(active_end)?;
    ensure!(
        exact_entry(&remote.list_folder(&parent.id).await?, current)? == document,
        "owned current revision changed during proof"
    );
    fuse_active(active_end)?;
    let end_root = remote.list_folder(cirrove_icloud::ROOT_ID).await?;
    let mut end_parents = end_root.iter().filter(|e| e.drivewsid == parent.id);
    let mut end_parent = end_parents
        .next()
        .context("owned parent disappeared")?
        .clone();
    end_parent.items.clear();
    end_parent.number_of_items = None;
    ensure!(
        end_parents.next().is_none() && end_parent == parent_entry,
        "owned parent changed during proof"
    );
    ensure!(
        read_private(path, 32 * 1024)? == bytes,
        "owned registration changed during proof"
    );
    settings(&receipt_plan)?;
    plan.sources().verify()?;
    let end_binding = fuse_journal_binding(&plan, &journal)?;
    ensure!(
        serde_json::to_vec(&end_binding)? == frozen,
        "owned FUSE journal receipt changed during proof"
    );
    let output = serde_json::json!({"run":plan.run,"account":plan.account,"import":plan.import,"save":plan.save,"working":plan.working,"owner":plan.owner,"canonical_fuse_archive":true,"native_fuse_save_verified":plan.save.is_some(),"registration_sha256":digest,
        "fixture_manifest_sha256":manifest_digest,"current":current_proof,"original_trash":trash_proof,"cloud_mutated":false,"gui_fidelity_verified":false});
    fuse_publish(active_end, || {
        record(
            &plan.session_directory.join(if backup.is_some() {
                "fuse-postflight-verified.json"
            } else {
                "fuse-preflight-verified.json"
            }),
            &output,
        )
    })?;
    Ok(output)
}
/// Static privacy boundary; observations can only refuse or prove, never replay.
pub async fn icloud_owned_fuse_receipt_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    tokio::time::timeout(
        std::time::Duration::from_secs(900),
        observe_fuse(path, digest, FuseFormat::Numbers),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!("owned FUSE receipt observation timed out; no mutation submitted")
    })?
    .map_err(|_| anyhow::anyhow!("owned FUSE receipt observation refused; no mutation submitted"))
}
#[cfg(test)]
mod tests;

/// Explicit Pages canonical PACKAGE source proof; no provider access.
pub fn icloud_owned_pages_fuse_source_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    verify_sources(path, digest, FuseFormat::Pages)
        .map_err(|_| anyhow::anyhow!("owned Pages FUSE source refused; no cloud access"))
}
/// Explicit Pages canonical archive capture; no provider access.
pub fn icloud_owned_pages_fuse_capture_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    verify_capture(path, digest, FuseFormat::Pages)
        .map_err(|_| anyhow::anyhow!("owned Pages FUSE capture refused; no cloud access"))
}
/// Registered Pages FUSE current/Trash proof. Parent retains its original window.
pub async fn icloud_owned_pages_fuse_receipt_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    // The inner cap is subordinate to the registration's ORIGINAL active end.
    tokio::time::timeout(
        std::time::Duration::from_secs(900),
        observe_fuse(path, digest, FuseFormat::Pages),
    )
    .await
    .map_err(|_| anyhow::anyhow!("owned Pages FUSE proof timed out; no mutation submitted"))?
    .map_err(|_| anyhow::anyhow!("owned Pages FUSE proof refused; no mutation submitted"))
}

/// Explicit Keynote canonical PACKAGE source proof; no provider access.
pub fn icloud_owned_keynote_fuse_source_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    verify_sources(path, digest, FuseFormat::Keynote)
        .map_err(|_| anyhow::anyhow!("owned Keynote FUSE source refused; no cloud access"))
}
/// Explicit Keynote canonical archive capture; no provider access.
pub fn icloud_owned_keynote_fuse_capture_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    verify_capture(path, digest, FuseFormat::Keynote)
        .map_err(|_| anyhow::anyhow!("owned Keynote FUSE capture refused; no cloud access"))
}
/// Registered Keynote FUSE current/Trash proof. Parent retains its original window.
pub async fn icloud_owned_keynote_fuse_receipt_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    // The inner cap is subordinate to the registration's ORIGINAL active end.
    tokio::time::timeout(
        std::time::Duration::from_secs(900),
        observe_fuse(path, digest, FuseFormat::Keynote),
    )
    .await
    .map_err(|_| anyhow::anyhow!("owned Keynote FUSE proof timed out; no mutation submitted"))?
    .map_err(|_| anyhow::anyhow!("owned Keynote FUSE proof refused; no mutation submitted"))
}
