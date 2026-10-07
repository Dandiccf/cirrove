//! Split owned-native CREATE validation. Source acquisition and verification are
//! read-only; only the separately invoked import command can create one document.
use super::*;
mod mounted;
mod trash;
use cirrove_core::{ProviderError, reads::ReadWindowSink};
use cirrove_icloud::{
    DriveEntry, OwnedPackageCreate, OwnedPackagePlan, PackageAllocationRefusal, PackageDownload,
    compare_package_archives, compare_package_archives_with_roots,
};
pub use mounted::icloud_owned_package_mounted;
use std::{fs::File, io::Read, os::unix::fs::PermissionsExt, path::PathBuf};
use tokio::io::AsyncWriteExt;
pub use trash::{
    icloud_owned_package_restore, icloud_owned_package_restore_inspect,
    icloud_owned_package_restore_shape, icloud_owned_package_trash,
    icloud_owned_package_trash_inspect,
};
const LIMIT: u64 = 64 * 1024 * 1024;
fn base() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local-state")
}
fn directory(run: Uuid) -> PathBuf {
    base().join(format!("icloud-owned-package-create-{run}"))
}
fn sha256(file: &mut File) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn private_file(path: &Path, max: u64) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0 && metadata.len() <= max,
        "invalid private package artifact"
    );
    Ok(file)
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path, max: u64) -> Result<T> {
    let mut bytes = Vec::new();
    private_file(path, max)?
        .take(max.checked_add(1).context("invalid artifact bound")?)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= max, "package artifact exceeds bound");
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid private package artifact"))
}
fn verification_directory(dir: &Path) -> Result<PathBuf> {
    check_directory(dir)?;
    let attempt = dir.join(format!("verify-{}", Uuid::new_v4()));
    std::fs::DirBuilder::new().mode(0o700).create(&attempt)?;
    File::open(dir)?.sync_all()?;
    Ok(attempt)
}
fn check_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
        "package run directory must be private"
    );
    Ok(())
}
fn manifest(dir: &Path, run: Uuid, phase: &str) -> Result<()> {
    manifest_with_duration(dir, run, phase, 900)
}
fn manifest_with_duration(
    dir: &Path,
    run: Uuid,
    phase: &str,
    expected_max_seconds: u64,
) -> Result<()> {
    check_directory(dir)?;
    let output = std::process::Command::new("findmnt")
        .args(["-n", "-o", "FSTYPE", "-T"])
        .arg(dir)
        .output()?;
    ensure!(
        output.status.success() && output.stdout == b"btrfs\n",
        "package evidence must use private btrfs storage"
    );
    let binary = std::env::current_exe()?;
    let digest = sha256(&mut File::open(&binary)?)?;
    record(
        &dir.join(format!("{phase}-manifest.json")),
        &serde_json::json!({"run":run,"phase":phase,"pid":std::process::id(),"binary":binary,"binary_sha256":digest,"filesystem":"btrfs","private_staging":dir,"expected_max_seconds":expected_max_seconds,"started_unix_seconds":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),"cloud_mutation":matches!(phase,"import"|"restore")}),
    )?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
struct DiskSink(tokio::fs::File);
#[async_trait::async_trait]
impl ReadWindowSink for DiskSink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> std::result::Result<(), ProviderError> {
        if bytes.len() > 64 * 1024 {
            return Err(ProviderError::Protocol("oversized native package chunk"));
        }
        self.0
            .write_all(bytes)
            .await
            .map_err(|_| ProviderError::Unavailable)
    }
}
async fn download(
    session: &mut ICloudReadSession,
    parent: &str,
    entry: &DriveEntry,
    path: &Path,
) -> Result<PackageDownload> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let mut sink = DiskSink(tokio::fs::File::from_std(file));
    let receipt = session
        .download_package(parent, entry, LIMIT, &mut sink, &CancellationToken::new())
        .await?;
    sink.0.flush().await?;
    sink.0.sync_all().await?;
    Ok(receipt)
}
fn scope(account: &Account) -> Scope {
    Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    }
}
fn checked_plan(account: &Account, plan: &OwnedPackagePlan, run: Uuid) -> Result<()> {
    ensure!(
        plan.operation == run
            && plan.scope == scope(account)
            && matches!(account.registration, AppRegistration::ICloud)
            && !account.enabled
            && account.access == AccessMode::ReadOnly
            && account.label == "iCloudOwnedPackageValidation"
            && plan.source.zone == "com.apple.CloudDocs"
            && plan.source.parent_id == plan.parent
            && plan.source.name == format!("Cirrove Package Source {run}")
            && plan.source.extension == "pages"
            && plan.parent_name == format!("Cirrove Package Validation {run}")
            && plan.destination == format!("Cirrove Package Import {run}.pages")
            && plan.archive_size > 0
            && plan.archive_size <= LIMIT,
        "package plan ownership mismatch"
    );
    Ok(())
}
async fn session(dir: &Path, account: &Account) -> Result<ICloudReadSession> {
    let snapshot = SealedSessionVault::new(&dir.join("state"), &account.id)?
        .load(&account.credential_id)
        .await?
        .context("isolated package session unavailable")?;
    ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)
}
fn retained(run: Uuid) -> Result<(PathBuf, Account, OwnedPackagePlan)> {
    let dir = directory(run);
    check_directory(&dir)?;
    let account: Account = read_json(&dir.join("account.json"), 64 * 1024)?;
    let plan: OwnedPackagePlan = read_json(&dir.join("plan.json"), 32 * 1024)?;
    checked_plan(&account, &plan, run)?;
    let ready: serde_json::Value = read_json(&dir.join("source-ready.json"), 4096)?;
    ensure!(
        ready["run"] == run.to_string()
            && ready["source_sha256"] == plan.archive_sha256
            && ready["archive_size"] == plan.archive_size
            && ready["semantic_self_verified"] == true,
        "package source receipt mismatch"
    );
    Ok((dir, account, plan))
}
/// READ ONLY. Select exactly the newly created UUID folder/source, not the first
/// Pages document or an application container. Persist a reviewable local plan.
pub async fn icloud_owned_package_source(run: Uuid) -> Result<()> {
    let dir = directory(run);
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    private_dir(&dir.join("state"))?;
    manifest(&dir, run, "source")?;
    let source = base()
        .join("icloud-gui-connect-validation/state")
        .canonicalize()?;
    let settings = Settings::load(&source)?;
    let accounts: Vec<_> = settings
        .accounts
        .iter()
        .filter(|a| {
            matches!(a.registration, AppRegistration::ICloud) && a.label == "iCloudGuiValidation"
        })
        .collect();
    ensure!(
        accounts.len() == 1,
        "expected exactly one isolated GUI validation account"
    );
    let original = accounts[0];
    let snapshot = SealedSessionVault::new(&source, &original.id)?
        .load(&original.credential_id)
        .await?
        .context("isolated GUI session unavailable")?;
    let mut account = original.clone();
    account.id = Uuid::new_v4().to_string();
    account.credential_id = Uuid::new_v4().to_string();
    account.enabled = false;
    account.access = AccessMode::ReadOnly;
    account.label = "iCloudOwnedPackageValidation".into();
    account.root_id = ROOT_ID.into();
    account.mount_path = dir.join("unused-mount");
    record(&dir.join("account.json"), &account)?;
    SealedSessionVault::new(&dir.join("state"), &account.id)?
        .save(&account.credential_id, snapshot.clone())
        .await?;
    let mut remote =
        ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
    let parent_name = format!("Cirrove Package Validation {run}");
    let parents: Vec<_> = remote
        .list_folder(ROOT_ID)
        .await?
        .into_iter()
        .filter(|e| e.display_name() == parent_name)
        .collect();
    ensure!(
        parents.len() == 1,
        "owned package root folder is not unique"
    );
    let parent = &parents[0];
    ensure!(
        parent.kind == "FOLDER"
            && parent.zone == "com.apple.CloudDocs"
            && parent.parent_id == ROOT_ID
            && parent
                .drivewsid
                .starts_with("FOLDER::com.apple.CloudDocs::"),
        "unsupported owned package folder identity"
    );
    record(&dir.join("owned-folder.json"), parent)?;
    let mut entries = remote.list_folder(&parent.drivewsid).await?;
    ensure!(
        entries.len() == 1,
        "owned source folder must contain exactly one item"
    );
    let entry = entries.remove(0);
    ensure!(
        entry.name == format!("Cirrove Package Source {run}") && entry.extension == "pages",
        "owned source name mismatch"
    );
    record(&dir.join("source-metadata.json"), &entry)?;
    ensure!(
        entry.kind == "FILE"
            && entry.zone == "com.apple.CloudDocs"
            && entry.parent_id == parent.drivewsid
            && entry.name == format!("Cirrove Package Source {run}")
            && entry.extension == "pages"
            && !entry.docwsid.is_empty()
            && entry.drivewsid == format!("FILE::com.apple.CloudDocs::{}", entry.docwsid)
            && !entry.etag.is_empty(),
        "unsupported source identity or zone after UI move"
    );
    // download_package requires an actual Package representation and binds
    // source revision before and after reading; Data cannot pass this gate.
    let receipt = download(
        &mut remote,
        &parent.drivewsid,
        &entry,
        &dir.join("source.zip"),
    )
    .await?;
    let plan = OwnedPackagePlan {
        scope: scope(&account),
        operation: run,
        parent: parent.drivewsid.clone(),
        parent_name,
        source: entry,
        destination: format!("Cirrove Package Import {run}.pages"),
        archive_size: receipt.size,
        archive_sha256: receipt.sha256.clone(),
    };
    checked_plan(&account, &plan, run)?;
    let a = private_file(&dir.join("source.zip"), LIMIT)?;
    let b = a.try_clone()?;
    let a_receipt = PackageDownload {
        size: receipt.size,
        sha256: receipt.sha256.clone(),
    };
    let comparison = tokio::task::spawn_blocking(move || {
        compare_package_archives(&a, &a_receipt, &b, &receipt, &CancellationToken::new())
    })
    .await??;
    record(&dir.join("plan.json"), &plan)?;
    record(
        &dir.join("source-ready.json"),
        &serde_json::json!({"run":run,"archive_size":plan.archive_size,"source_sha256":plan.archive_sha256,"semantic_self_verified":true,"entries":comparison.entries,"files":comparison.files,"expanded_bytes":comparison.expanded_bytes,"read_only":true}),
    )?;
    println!("Owned package source acquired and locally verified; import has not run.");
    Ok(())
}
fn record_allocation_diagnostic(dir: &Path, run: Uuid, error: &anyhow::Error) -> Result<()> {
    if let Some(diagnostic) = error.downcast_ref::<PackageAllocationRefusal>() {
        record(
            &dir.join("allocation-refused.json"),
            &serde_json::json!({"run":run,"category":diagnostic}),
        )?;
    }
    Ok(())
}
/// ONE-SHOT CREATE. Requires the separately retained and reviewable source plan.
pub async fn icloud_owned_package_import(run: Uuid) -> Result<()> {
    let (dir, account, plan) = retained(run)?;
    manifest(&dir, run, "import")?;
    let remote = session(&dir, &account).await?;
    let archive = private_file(&dir.join("source.zip"), LIMIT)?;
    let importer = OwnedPackageCreate::prepare(remote, plan, &dir.join("state")).await?;
    let result = match importer.execute(archive, &CancellationToken::new()).await {
        Ok(result) => result,
        Err(error) => {
            record_allocation_diagnostic(&dir, run, &error)?;
            return Err(error);
        }
    };
    ensure!(
        result.registration_confirmed && result.observed.is_some(),
        "package registration remains uncertain; use read-only verification"
    );
    record(
        &dir.join("registered.json"),
        &serde_json::json!({"run":run,"allocated_document_id":result.allocated_document_id,"observed":result.observed,"content_verified":false}),
    )?;
    println!("Owned package registered; independent content verification is still required.");
    Ok(())
}
/// READ ONLY. Fresh sessions inspect the checkpoint identity, preserve source,
/// download imported Package content, and compare decompressed ZIP semantics.
pub async fn icloud_owned_package_verify(run: Uuid) -> Result<()> {
    let (dir, account, plan) = retained(run)?;
    let attempt = verification_directory(&dir)?;
    manifest(&attempt, run, "verify")?;
    let found =
        OwnedPackageCreate::inspect(session(&dir, &account).await?, &plan, &dir.join("state"))
            .await?;
    let imported = found
        .observed
        .context("allocated package identity not visible; no mutation retried")?;
    let mut fresh = session(&dir, &account).await?;
    let entries = fresh.list_folder(&plan.parent).await?;
    ensure!(
        entries.len() == 2
            && entries.iter().any(|e| e == &plan.source)
            && entries.iter().any(|e| e == &imported),
        "owned package source or inventory changed"
    );
    let receipt = download(
        &mut fresh,
        &plan.parent,
        &imported,
        &attempt.join("imported.zip"),
    )
    .await?;
    let expected_source_root = plan.source.display_name();
    let expected_imported_root = plan.destination.clone();
    let source_receipt = PackageDownload {
        size: plan.archive_size,
        sha256: plan.archive_sha256,
    };
    let a = private_file(&dir.join("source.zip"), LIMIT)?;
    let b = private_file(&attempt.join("imported.zip"), LIMIT)?;
    let comparison = tokio::task::spawn_blocking(move || {
        compare_package_archives_with_roots(
            &a,
            &source_receipt,
            &expected_source_root,
            &b,
            &receipt,
            &expected_imported_root,
            &CancellationToken::new(),
        )
    })
    .await??;
    record(
        &attempt.join("verified.json"),
        &serde_json::json!({"run":run,"allocated_identity_verified":true,"source_unchanged":true,"semantic_content_verified":true,"expected_package_roots_bound":true,"entries":comparison.entries,"files":comparison.files,"expanded_bytes":comparison.expanded_bytes,"raw_archives_equal":comparison.raw_archives_equal,"source_archive_sha256":comparison.source_archive_sha256,"imported_archive_sha256":comparison.imported_archive_sha256,"native_ui_open_verified":false,"mounted_verified":false,"read_only":true}),
    )?;
    println!(
        "Owned package identity and independent decompressed content verified; native UI open remains."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_package_readonly_verification_uses_fresh_retained_attempts() {
        let dir = tempfile::tempdir().expect("private fixture");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("mode");
        let first = verification_directory(dir.path()).expect("first");
        record(
            &first.join("verify-manifest.json"),
            &serde_json::json!({"failed":true}),
        )
        .expect("retain");
        let second = verification_directory(dir.path()).expect("second");
        assert_ne!(first, second);
        assert!(first.join("verify-manifest.json").is_file());
        assert!(!second.join("verify-manifest.json").exists());
    }
    #[test]
    fn owned_package_private_artifacts_refuse_symlinks_and_oversize_json() {
        let dir = tempfile::tempdir().expect("private fixture");
        let path = dir.path().join("data.json");
        record(&path, &serde_json::json!({"test":true})).expect("artifact");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).expect("link");
        assert!(private_file(&link, 1024).is_err());
        assert!(read_json::<serde_json::Value>(&path, 1).is_err());
        assert!(read_json::<serde_json::Value>(&path, 1024).is_ok());
    }
    #[test]
    fn owned_package_records_only_typed_allocation_category_without_error_context() {
        let dir = tempfile::tempdir().expect("fixture");
        let run = Uuid::new_v4();
        record_allocation_diagnostic(dir.path(), run, &anyhow::anyhow!("PRIVATE unrelated error"))
            .expect("untyped ignored");
        assert!(!dir.path().join("allocation-refused.json").exists());
        let error = anyhow::Error::from(PackageAllocationRefusal::OwnerTooLong)
            .context("PRIVATE owner and URL context");
        record_allocation_diagnostic(dir.path(), run, &error).expect("typed category");
        let bytes = std::fs::read(dir.path().join("allocation-refused.json")).expect("artifact");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value["category"], "owner_too_long");
        assert_eq!(value.as_object().expect("object").len(), 2);
        assert!(!String::from_utf8(bytes).expect("UTF8").contains("PRIVATE"));
    }
}

mod public_verify;
pub use public_verify::{
    icloud_public_native_manifest, icloud_public_native_readiness,
    icloud_public_native_renewal_readiness, icloud_public_native_verify,
};

mod public_trash;
pub use public_trash::{
    icloud_public_native_restore, icloud_public_native_restore_inspect,
    icloud_public_native_trash_import_verify, icloud_public_native_trash_verify,
};

mod replacement_live;
pub use replacement_live::{
    icloud_native_v2_baseline, icloud_native_v2_preflight, icloud_native_v2_verify,
    icloud_public_native_replacement_diagnose, icloud_public_native_replacement_preflight,
    icloud_public_native_replacement_verify,
};

mod fixture_verify;
pub(super) use fixture_verify::icloud_owned_native_trash_fixture_verify;
pub use fixture_verify::{
    icloud_owned_calc_metadata, icloud_owned_calc_trash_original, icloud_owned_editor_metadata,
    icloud_owned_editor_source_proof, icloud_owned_fixture_verify,
    icloud_owned_fuse_capture_verify, icloud_owned_fuse_receipt_verify,
    icloud_owned_fuse_source_verify, icloud_owned_impress_metadata,
    icloud_owned_impress_trash_original, icloud_owned_keynote_data_receipt_verify,
    icloud_owned_keynote_data_source_verify, icloud_owned_keynote_import_receipt_verify,
    icloud_owned_keynote_replacement_receipt_verify, icloud_owned_keynote_source_verify,
    icloud_owned_native_import_fixture_verify, icloud_owned_native_pre_trash_preservation,
    icloud_owned_numbers_data_receipt_verify, icloud_owned_numbers_data_source_verify,
    icloud_owned_pages_data_receipt_verify, icloud_owned_pages_data_source_verify,
    icloud_owned_pages_replacement_receipt_verify, icloud_owned_receipt_verify,
    icloud_owned_retained_numbers_read, icloud_owned_writer_metadata,
    icloud_owned_writer_trash_original,
};
