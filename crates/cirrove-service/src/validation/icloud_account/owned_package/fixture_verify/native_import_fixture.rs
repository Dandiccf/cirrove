//! Feature-only exact owned Pages/native-final and selected flat Numbers read observer.
use super::super::public_verify::exact_entry;
use super::*;
use cirrove_core::{Node, NodeKind};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Arm {
    PagesDesktop,
    NativeFinalPreflight,
    NativeFinalPostflight,
    FlatNumbersPreflight,
    FlatNumbersPostflight,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: PathBuf,
    size: u64,
    sha256: String,
    #[serde(deserialize_with = "super::required_source_root")]
    root: Option<String>,
    semantic: PackageSemanticIdentity,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Recovered {
    original: Node,
    backup: Node,
    source_a: Source,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    arm: Arm,
    run: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subject_run: Option<Uuid>,
    account: Uuid,
    label: String,
    session_directory: PathBuf,
    settings_sha256: String,
    parent_creation: Uuid,
    operation: Uuid,
    parent: Node,
    document: Node,
    source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovered: Option<Recovered>,
    started_unix_seconds: u64,
    deadline_unix_seconds: u64,
}
impl Registration {
    fn expected(&self) -> (PathBuf, String, String, String, PathBuf, Option<String>) {
        match self.arm {
            Arm::PagesDesktop => {
                let dir = PathBuf::from(format!("/var/tmp/cirrove-pages-desktop-{}", self.run));
                let name = format!("Cirrove-Pages-Desktop-{}.pages", self.run);
                (
                    dir.clone(),
                    "iCloudPagesDesktopValidation".into(),
                    name.clone(),
                    "pages".into(),
                    dir.join("mounted-import.pages"),
                    Some(name),
                )
            }
            Arm::NativeFinalPreflight | Arm::NativeFinalPostflight => {
                let dir = PathBuf::from(format!("/var/tmp/cirrove-native-final-{}", self.run));
                (
                    dir.clone(),
                    "iCloudNativeFinalValidation".into(),
                    format!("Cirrove-Numbers-Parent-{}.numbers", self.run),
                    "numbers".into(),
                    dir.join(if matches!(self.arm, Arm::NativeFinalPostflight) {
                        "source-b.numbers"
                    } else {
                        "source-a.numbers"
                    }),
                    Some("Source.numbers".into()),
                )
            }
            Arm::FlatNumbersPreflight | Arm::FlatNumbersPostflight => {
                let dir = PathBuf::from(format!(
                    "/var/tmp/cirrove-numbers-flat-replacement-{}",
                    self.run
                ));
                (
                    dir.clone(),
                    "iCloudNumbersFlatReplacementValidation".into(),
                    format!(
                        "Cirrove-Numbers-Editor-{}.numbers",
                        self.subject_run.unwrap_or(self.run)
                    ),
                    "numbers".into(),
                    dir.join(if matches!(self.arm, Arm::FlatNumbersPostflight) {
                        "source-b.numbers"
                    } else {
                        "source-a.numbers"
                    }),
                    None,
                )
            }
        }
    }
    fn validate(&self, now: u64) -> Result<()> {
        let (dir, label, name, _, source, root) = self.expected();
        ensure!(
            match self.arm {
                Arm::FlatNumbersPreflight | Arm::FlatNumbersPostflight => self
                    .subject_run
                    .is_some_and(|subject| !subject.is_nil() && subject != self.run),
                _ => self.subject_run.is_none(),
            },
            "native fixture subject binding refused"
        );
        ensure!(
            self.version == 1
                && !self.run.is_nil()
                && !self.account.is_nil()
                && self.session_directory == dir
                && self.label == label
                && self.document.name == name
                && self.source.path == source
                && self.source.root == root
                && hex_digest(&self.settings_sha256)
                && !self.parent_creation.is_nil()
                && !self.operation.is_nil()
                && self.parent_creation != self.operation,
            "native fixture registration refused"
        );
        ensure!(
            native_id(&self.parent.id, "FOLDER::com.apple.CloudDocs::")
                && self.parent.id != cirrove_icloud::ROOT_ID
                && self.parent.parent_id.as_deref() == Some(cirrove_icloud::ROOT_ID)
                && self.parent.name
                    == format!("Cirrove-Native-{}", self.subject_run.unwrap_or(self.run))
                && self.parent.kind == NodeKind::Folder
                && !self.parent.package
                && self.parent.target.is_none()
                && exact_etag(&self.parent)
                && native_id(&self.document.id, "FILE::com.apple.CloudDocs::")
                && self.document.parent_id.as_ref() == Some(&self.parent.id)
                && self.document.kind == NodeKind::Folder
                && self.document.package
                && self.document.target.is_none()
                && self.document.content_version.is_none()
                && self.document.size > 0
                && exact_etag(&self.document),
            "native fixture receipt identity refused"
        );
        self.source.semantic.validate()?;
        ensure!(
            self.source.size > 0
                && self.source.size <= LIMIT
                && hex_digest(&self.source.sha256)
                && self.source.semantic.version == 2,
            "native fixture source proof refused"
        );
        match (self.arm, self.recovered.as_ref()) {
            (Arm::NativeFinalPostflight | Arm::FlatNumbersPostflight, Some(recovered)) => {
                recovered.validate(self)?
            }
            (Arm::PagesDesktop | Arm::NativeFinalPreflight | Arm::FlatNumbersPreflight, None) => {}
            _ => anyhow::bail!("native fixture recovered inputs refused"),
        }
        ensure!(
            self.started_unix_seconds > 0
                && self.started_unix_seconds <= now
                && now < self.deadline_unix_seconds
                && self
                    .deadline_unix_seconds
                    .checked_sub(self.started_unix_seconds)
                    .is_some_and(|n| n > 0 && n <= 1800),
            "native fixture finite window refused"
        );
        Ok(())
    }
}
impl Recovered {
    fn validate(&self, r: &Registration) -> Result<()> {
        let old = &self.original;
        let backup = &self.backup;
        let a = &self.source_a;
        a.semantic.validate()?;
        ensure!(
            a.path == r.session_directory.join("source-a.numbers")
                && a.root
                    == if matches!(r.arm, Arm::FlatNumbersPostflight) {
                        None
                    } else {
                        Some("Source.numbers".into())
                    }
                && a.size > 0
                && a.size <= LIMIT
                && hex_digest(&a.sha256)
                && a.semantic.version == 2
                && a.sha256 != r.source.sha256
                && a.semantic != r.source.semantic
                && native_id(&old.id, "FILE::com.apple.CloudDocs::")
                && old.id != r.document.id
                && old.parent_id.as_ref() == Some(&r.parent.id)
                && old.name == r.document.name
                && old.kind == NodeKind::Folder
                && old.package
                && old.target.is_none()
                && old.content_version.is_none()
                && old.size > 0
                && exact_etag(old)
                && backup.id == old.id
                && backup.name == old.name
                && backup.size == old.size
                && backup.parent_id.as_deref() == Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT")
                && backup.kind == old.kind
                && backup.package
                && backup.target.is_none()
                && backup.content_version.is_none()
                && exact_etag(backup),
            "native fixture original/Trash/source-A binding refused"
        );
        Ok(())
    }
}
impl Registration {
    fn artifact_stem(&self) -> &'static str {
        match self.arm {
            Arm::FlatNumbersPreflight => "flat-numbers-preflight",
            Arm::FlatNumbersPostflight => "flat-numbers-postflight",
            Arm::NativeFinalPostflight => "native-final-postflight",
            _ => "native-import-fixture",
        }
    }
    fn postflight(&self) -> bool {
        matches!(
            self.arm,
            Arm::NativeFinalPostflight | Arm::FlatNumbersPostflight
        )
    }
}
fn trash_binding(
    backup: &Node,
    source: &Source,
    recovered: &cirrove_icloud::VerifiedPackageTrash,
) -> Result<()> {
    ensure!(
        backup.etag.as_ref() == Some(&recovered.trash_etag)
            && recovered.semantic == source.semantic,
        "native fixture independent Trash proof changed"
    );
    Ok(())
}
fn native_id(id: &str, prefix: &str) -> bool {
    id.starts_with(prefix)
        && id.len() > prefix.len()
        && id.len() <= 4096
        && !id.chars().any(char::is_control)
}
fn exact_etag(node: &Node) -> bool {
    node.etag.as_ref().is_some_and(|t| {
        !t.is_empty() && t.len() <= 4096 && !t.chars().any(|c| c.is_control() || c == '*')
    })
}
fn clock() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn read_private(path: &Path, max: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_private(path, false, max)?
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= max,
        "native fixture private artifact bound refused"
    );
    Ok(bytes)
}
fn registered(bytes: &[u8], digest: &str, now: u64) -> Result<Registration> {
    ensure!(
        bytes.len() <= 32 * 1024
            && hex_digest(digest)
            && hex::encode(Sha256::digest(bytes)) == digest,
        "native fixture registration digest changed"
    );
    let r: Registration = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("native fixture schema refused"))?;
    r.validate(now)?;
    Ok(r)
}
fn source_verified(source: &Source) -> Result<()> {
    let mut file = open_private(&source.path, false, LIMIT)?;
    let receipt = PackageDownload {
        size: file.metadata()?.len(),
        sha256: sha256(&mut file)?,
    };
    ensure!(
        receipt.size == source.size && receipt.sha256 == source.sha256,
        "native fixture source bytes changed"
    );
    ensure!(
        match source.root.as_deref() {
            Some(root) => cirrove_icloud::package_archive_semantic_identity_versioned(
                &file,
                &receipt,
                root,
                2,
                &CancellationToken::new()
            )?,
            None => cirrove_icloud::package_flat_archive_semantic_identity_v2(
                &file,
                &receipt,
                &CancellationToken::new()
            )?,
        } == source.semantic,
        "native fixture source semantics changed"
    );
    Ok(())
}
fn unchanged(path: &Path, digest: &str, r: &Registration, settings_path: &Path) -> Result<()> {
    let _ = registered(&read_private(path, 32 * 1024)?, digest, clock()?)?;
    ensure!(
        hex::encode(Sha256::digest(read_private(settings_path, 256 * 1024)?)) == r.settings_sha256,
        "native fixture settings changed"
    );
    source_verified(&r.source)?;
    if let Some(recovered) = &r.recovered {
        source_verified(&recovered.source_a)?;
    }
    Ok(())
}
fn bound_settings(r: &Registration, bytes: &[u8]) -> Result<(Settings, Account)> {
    ensure!(
        bytes.len() <= 256 * 1024 && hex::encode(Sha256::digest(bytes)) == r.settings_sha256,
        "native fixture settings digest differs"
    );
    let settings: Settings = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("native fixture settings schema refused"))?;
    ensure!(
        settings.version == 2 && settings.accounts.len() == 1,
        "native fixture account inventory differs"
    );
    let account = settings.accounts[0].clone();
    ensure!(
        account.id == r.account.to_string()
            && matches!(account.registration, AppRegistration::ICloud)
            && account.label == r.label
            && account.enabled
            && account.access == cirrove_auth::AccessMode::ReadWrite
            && account.drive.id == "drive"
            && account.drive.drive_type == "icloud_drive"
            && account.root_id == cirrove_icloud::ROOT_ID
            && account.mount_path == r.session_directory.join("mount"),
        "native fixture account binding refused"
    );
    Ok((settings, account))
}
fn exact_parent(entries: &[DriveEntry], r: &Registration) -> Result<DriveEntry> {
    let mut matches = entries.iter().filter(|e| e.drivewsid == r.parent.id);
    let mut parent = matches
        .next()
        .context("native fixture parent absent")?
        .clone();
    ensure!(
        matches.next().is_none()
            && entries
                .iter()
                .filter(|e| e.display_name() == r.parent.name)
                .count()
                == 1
            && parent.kind == "FOLDER"
            && parent.zone == "com.apple.CloudDocs"
            && parent.display_name() == r.parent.name,
        "native fixture parent ambiguous or changed"
    );
    // Preserve actual current parent ETag/size/docwsid and optional item_id;
    // the historical creator Node binds identity/path, not stale parent ETag.
    parent.items.clear();
    parent.number_of_items = None;
    Ok(parent)
}
fn fixture_value(
    r: &Registration,
    parent: &DriveEntry,
    document: &DriveEntry,
) -> serde_json::Value {
    let (_, _, _, format, _, _) = r.expected();
    serde_json::json!({"version":1,"run":r.subject_run.unwrap_or(r.run),"account":r.account,"session_directory":r.session_directory,
        "settings_sha256":r.settings_sha256,"parent":parent,"document":document,"format":format,
        "representation":"package","source":r.source.path,"source_size":r.source.size,"source_sha256":r.source.sha256,
        "source_root":r.source.root,"expected_root":r.document.name,"semantic":r.source.semantic})
}
async fn observe(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let bytes = read_private(path, 32 * 1024)?;
    let r = registered(&bytes, digest, clock()?)?;
    ensure!(
        path == r
            .session_directory
            .join(format!("{}-registration.json", r.artifact_stem())),
        "native fixture registration path refused"
    );
    let guard = open_private(&r.session_directory, true, 0)?;
    let state_guard = open_private(&r.session_directory.join("state"), true, 0)?;
    let settings_path = r.session_directory.join("state/accounts.json");
    let settings_bytes = read_private(&settings_path, 256 * 1024)?;
    ensure!(
        hex::encode(Sha256::digest(&settings_bytes)) == r.settings_sha256,
        "native fixture settings differ"
    );
    let (settings, account) = bound_settings(&r, &settings_bytes)?;
    source_verified(&r.source)?;
    unchanged(path, digest, &r, &settings_path)?;
    // Exclusive attempt is durable before any session/provider access. A failed
    // or expired read is retained, never implicitly retried in this directory.
    let mut attempt_value = serde_json::json!({
        "run":r.run,"account":r.account,"registration_sha256":digest,"started_unix_seconds":r.started_unix_seconds,
        "deadline_unix_seconds":r.deadline_unix_seconds,"provider_mutation":false,"automatic_retry":false});
    if let Some(subject) = r.subject_run {
        attempt_value["subject_run"] = serde_json::to_value(subject)?;
    }
    record(
        &r.session_directory
            .join(format!("{}.attempt.json", r.artifact_stem())),
        &attempt_value,
    )?;
    File::open(&r.session_directory)?.sync_all()?;
    let remaining = r
        .deadline_unix_seconds
        .checked_sub(clock()?)
        .context("native fixture window expired")?;
    ensure!(remaining > 0, "native fixture window expired");
    let proof=tokio::time::timeout(Duration::from_secs(remaining),async {
        let mut remote=session(&r.session_directory,&account).await?;
        let parent=exact_parent(&remote.list_folder(cirrove_icloud::ROOT_ID).await?,&r)?;
        let document=exact_entry(&remote.list_folder(&r.parent.id).await?,&r.document)?;
        let value=fixture_value(&r,&parent,&document);let encoded=serde_json::to_vec(&value)?;
        let fixture:Fixture=serde_json::from_slice(&encoded)?;validate(&fixture)?;
        let _=account_binding(&fixture,&settings.accounts)?;
        same_directory(&r.session_directory,&guard)?;same_directory(&r.session_directory.join("state"),&state_guard)?;
        unchanged(path,digest,&r,&settings_path)?;
        let fixture_path=r.session_directory.join(format!("{}.json", r.artifact_stem()));record(&fixture_path,&value)?;
        let fixture_digest=hex::encode(Sha256::digest(read_private(&fixture_path,32*1024)?));
        let current=icloud_owned_fixture_verify(&fixture_path,&fixture_digest).await?;
        let mut trash_proof = None;
        if let Some(recovered) = &r.recovered {
            let attempt = verification_directory(&r.session_directory)?;
            manifest(&attempt, r.run, if matches!(r.arm, Arm::FlatNumbersPostflight) {
                "flat-numbers-postflight-Trash-read-only"
            } else { "native-final-postflight-Trash-read-only" })?;
            let staging = tempfile::tempfile_in(&attempt)?;
            staging.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            let observed = remote.verify_owned_package_in_trash(cirrove_icloud::OwnedPackageTrashRequest {
                apple_account: account.identity.username.clone(),
                drive_id: recovered.backup.id.clone(),
                document_id: recovered.backup.id.strip_prefix("FILE::com.apple.CloudDocs::").context("native fixture old document identity refused")?.into(),
                expected_root: r.document.name.clone(), semantic: recovered.source_a.semantic.clone(),
            }, staging, &CancellationToken::new()).await?;
            trash_binding(&recovered.backup, &recovered.source_a, &observed)?;
            trash_proof = Some(serde_json::json!({"original":recovered.original,"backup":recovered.backup,
                "item":recovered.backup.id,"etag":observed.trash_etag,"semantic":observed.semantic,
                "source_a":recovered.source_a,"size":observed.archive.size,"sha256":observed.archive.sha256}));
        }
        ensure!(exact_entry(&remote.list_folder(&r.parent.id).await?,&r.document)? == document,
            "native fixture current changed during independent proof");
        ensure!(exact_parent(&remote.list_folder(cirrove_icloud::ROOT_ID).await?,&r)? == parent,
            "native fixture parent changed during independent proof");
        unchanged(path,digest,&r,&settings_path)?;
        same_directory(&r.session_directory,&guard)?;same_directory(&r.session_directory.join("state"),&state_guard)?;
        let executable=std::env::current_exe()?;let mut executable_file=File::open(&executable)?;
        let producer=serde_json::json!({"path":executable,"size":executable_file.metadata()?.len(),"sha256":sha256(&mut executable_file)?});
        unchanged(path,digest,&r,&settings_path)?;
        let mut acquisition=serde_json::json!({"run":r.run,"account":r.account,"operation":r.operation,"operation_kind":if r.postflight(){"replacement"}else{"import"},
            "parent_creation":r.parent_creation,"producer_binary":producer,"source":"actual-ICloudReadSession-metadata-output","arm":r.arm,
            "parent":parent,"document":document,"fixture_registration_sha256":fixture_digest,
            "registration_sha256":digest,"provider_mutation":false});
        if let Some(subject) = r.subject_run { acquisition["subject_run"] = serde_json::to_value(subject)?; }
        record(&r.session_directory.join(format!("{}-acquired.json", r.artifact_stem())),&acquisition)?;
        let mut result=serde_json::json!({"run":r.run,"account":r.account,"registration_sha256":digest,
            "fixture_manifest_sha256":fixture_digest,"registered_operation":r.operation,"operation_kind":if r.postflight(){"replacement"}else{"import"},"registered_parent_creation":r.parent_creation,
            "journal_receipt_independently_verified":false,"source":r.source,"current":current,"original_trash":trash_proof,"arm":r.arm,
            "actual_typed_metadata_acquired":true,"source_semantic_verified":true,
            "gui_fidelity_verified":false,"cloud_mutated":false,"automatic_retry":false});
        if let Some(subject) = r.subject_run { result["subject_run"] = serde_json::to_value(subject)?; }
        record(&r.session_directory.join(format!("{}-verified.json", r.artifact_stem())),&result)?;
        File::open(&r.session_directory)?.sync_all()?;
        same_directory(&r.session_directory,&guard)?;same_directory(&r.session_directory.join("state"),&state_guard)?;
        Ok::<_,anyhow::Error>(result)
    }).await.map_err(|_|anyhow::anyhow!("native fixture overall deadline exceeded"))??;
    Ok(proof)
}
/// Read-only; the caller separately proves stopped ownership and durable receipt.
pub async fn icloud_owned_native_import_fixture_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    observe(path, digest)
        .await
        .map_err(|_| anyhow::anyhow!("owned native fixture verification refused"))
}
#[cfg(test)]
mod tests;
