//! Read-only registered fixture observer. Never an application-fidelity claim.
use super::*;
use anyhow::bail;
use cirrove_core::upload::PackageSemanticIdentity;
use cirrove_icloud::FixtureRepresentation;
use std::os::unix::fs::MetadataExt;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    version: u32,
    run: Uuid,
    account: Uuid,
    session_directory: PathBuf,
    settings_sha256: String,
    parent: DriveEntry,
    document: DriveEntry,
    format: String,
    representation: FixtureRepresentation,
    source: PathBuf,
    source_size: u64,
    source_sha256: String,
    #[serde(deserialize_with = "required_source_root")]
    source_root: Option<String>,
    expected_root: String,
    semantic: Option<PackageSemanticIdentity>,
}
fn required_source_root<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(deserializer)
}
fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
// Descriptor traversal rejects symlinks in EVERY component, including ancestors.
fn open_private(path: &Path, directory: bool, max: u64) -> Result<File> {
    use std::path::Component;
    ensure!(path.is_absolute(), "fixture path must be absolute");
    let parts = path
        .components()
        .skip(1)
        .map(|part| match part {
            Component::Normal(p) => Ok(p),
            _ => bail!("fixture path is not canonical"),
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!parts.is_empty(), "fixture path missing");
    let mut parent = File::open("/")?;
    for (index, part) in parts.iter().enumerate() {
        use rustix::fs::{Mode, OFlags};
        let last = index + 1 == parts.len();
        let flags = OFlags::RDONLY
            | OFlags::CLOEXEC
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | if !last || directory {
                OFlags::DIRECTORY
            } else {
                OFlags::empty()
            };
        parent = File::from(
            rustix::fs::openat(&parent, *part, flags, Mode::empty())
                .map_err(|_| anyhow::anyhow!("fixture private path refused"))?,
        );
    }
    let meta = parent.metadata()?;
    ensure!(
        meta.uid() == std::fs::metadata("/proc/self")?.uid()
            && meta.permissions().mode() & 0o077 == 0
            && if directory {
                meta.is_dir()
            } else {
                meta.is_file() && meta.len() <= max
            },
        "fixture ownership or bounds refused"
    );
    Ok(parent)
}
fn same_directory(path: &Path, held: &File) -> Result<()> {
    let current = open_private(path, true, 0)?.metadata()?;
    let original = held.metadata()?;
    ensure!(
        current.dev() == original.dev() && current.ino() == original.ino(),
        "fixture directory identity changed"
    );
    Ok(())
}
fn decode(bytes: &[u8], expected_hash: &str) -> Result<Fixture> {
    ensure!(
        bytes.len() <= 32 * 1024
            && hex_digest(expected_hash)
            && hex::encode(Sha256::digest(bytes)) == expected_hash,
        "registered fixture manifest changed"
    );
    serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!("fixture schema refused"))
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum FixtureArm {
    Generic,
    NativeTrash,
    FlatPages,
    PagesData,
    KeynoteData,
}
fn validate(f: &Fixture) -> Result<()> {
    validate_for(f, FixtureArm::Generic)
}
fn validate_native_trash(f: &Fixture) -> Result<()> {
    validate_for(f, FixtureArm::NativeTrash)?;
    ensure!(
        f.session_directory == format!("/var/tmp/cirrove-native-trash-{}", f.run)
            && f.source == f.session_directory.join("source.numbers")
            && f.format == "numbers"
            && f.representation == FixtureRepresentation::Package
            && f.source_root.is_none()
            && f.expected_root == format!("Cirrove-Numbers-Trash-{}.numbers", f.run)
            && f.semantic
                .as_ref()
                .is_some_and(|v| v.version == 2 && v.files > 0)
            && f.parent.drivewsid != cirrove_icloud::ROOT_ID
            && !f.parent.drivewsid.ends_with("::TRASH_ROOT"),
        "native Trash preflight scope refused"
    );
    Ok(())
}
fn validate_for(f: &Fixture, arm: FixtureArm) -> Result<()> {
    let parent_name = match arm {
        FixtureArm::Generic
        | FixtureArm::FlatPages
        | FixtureArm::PagesData
        | FixtureArm::KeynoteData => {
            format!("Cirrove-Native-{}", f.run)
        }
        FixtureArm::NativeTrash => format!("Cirrove-Native-Trash-{}", f.run),
    };
    let extension = match f.format.as_str() {
        "pages" => "pages",
        "numbers" => "numbers",
        "keynote" => "key",
        "xlsx" => "xlsx",
        "docx" => "docx",
        "pptx" => "pptx",
        _ => bail!("fixture format unsupported"),
    };
    if f.format == "xlsx" {
        ensure!(
            f.representation == FixtureRepresentation::Data
                && f.source_root.is_none()
                && f.semantic.is_none(),
            "fixture XLSX requires raw DATA without a package root or semantic proof"
        );
    }
    if f.format == "docx" {
        ensure!(
            f.representation == FixtureRepresentation::Data
                && f.source_root.is_none()
                && f.semantic.is_none()
                && f.session_directory == calc_metadata::EditorArm::Writer.editor_root(f.run)
                && f.document.name == format!("Cirrove-Writer-{}", f.run)
                && ["source-a.docx", "source-b.docx"]
                    .iter()
                    .any(|name| f.source == f.session_directory.join(name)),
            "fixture DOCX requires exact Writer DATA route without package semantics"
        );
    }
    if f.format == "pptx" {
        ensure!(
            f.representation == FixtureRepresentation::Data
                && f.source_root.is_none()
                && f.semantic.is_none()
                && f.session_directory == calc_metadata::EditorArm::Impress.editor_root(f.run)
                && f.document.name == format!("Cirrove-Impress-{}", f.run)
                && ["source-a.pptx", "source-b.pptx"]
                    .iter()
                    .any(|name| f.source == f.session_directory.join(name)),
            "fixture PPTX requires exact Impress DATA route without package semantics"
        );
    }
    ensure!(
        f.version == 1
            && !f.run.is_nil()
            && !f.account.is_nil()
            && f.parent.kind == "FOLDER"
            && f.parent.zone == "com.apple.CloudDocs"
            && f.parent.name == parent_name
            && f.parent
                .drivewsid
                .starts_with("FOLDER::com.apple.CloudDocs::")
            && f.document.kind == "FILE"
            && f.document.zone == "com.apple.CloudDocs"
            && f.document.parent_id == f.parent.drivewsid
            && f.document.drivewsid == format!("FILE::com.apple.CloudDocs::{}", f.document.docwsid)
            && !f.document.docwsid.is_empty()
            && !f.document.etag.is_empty()
            && !f.document.etag.contains('*')
            && f.document.extension == extension
            && f.document.name.contains(&f.run.to_string())
            && f.source_root.as_ref().map_or(
                f.format == "numbers"
                    || f.format == "xlsx"
                    || f.format == "docx"
                    || f.format == "pptx"
                    || (matches!(arm, FixtureArm::FlatPages | FixtureArm::PagesData)
                        && f.format == "pages")
                    || (arm == FixtureArm::KeynoteData && f.format == "keynote"),
                |root| root.ends_with(&format!(".{extension}"))
                    && !root.contains(['/', '\\'])
                    && !root.chars().any(char::is_control)
                    && root.len() <= 255
            )
            && f.expected_root.ends_with(&format!(".{extension}"))
            && f.expected_root == format!("{}.{}", f.document.name, extension)
            && !f.expected_root.contains(['/', '\\'])
            && f.expected_root.len() <= 255
            && f.source_size > 0
            && f.source_size <= LIMIT
            && hex_digest(&f.source_sha256)
            && hex_digest(&f.settings_sha256)
            && f.session_directory.starts_with("/var/tmp")
            && f.session_directory.components().count() > 3,
        "fixture ownership registration refused"
    );
    match (&f.representation, &f.semantic) {
        (FixtureRepresentation::Data, None) => {}
        (FixtureRepresentation::Package, Some(proof)) => {
            proof.validate()?;
            ensure!(proof.version == 2, "fixture requires explicit semantic v2");
        }
        _ => bail!("fixture representation proof mismatch"),
    }
    Ok(())
}
fn exact_account(f: &Fixture, actual: &str) -> Result<()> {
    ensure!(
        actual == f.account.to_string(),
        "fixture account identity differs"
    );
    Ok(())
}
fn account_binding(f: &Fixture, accounts: &[Account]) -> Result<Account> {
    ensure!(
        accounts.len() == 1,
        "isolated fixture account inventory changed"
    );
    let a = accounts[0].clone();
    exact_account(f, &a.id)?;
    ensure!(
        matches!(a.registration, AppRegistration::ICloud),
        "fixture account provider differs"
    );
    if f.format == "docx" {
        ensure!(
            a.label == "iCloudWriterValidation"
                && a.enabled
                && a.drive.id == "drive"
                && a.drive.drive_type == "icloud_drive"
                && a.root_id == cirrove_icloud::ROOT_ID
                && a.mount_path == f.session_directory.join("mount")
                && !Uuid::parse_str(&a.credential_id)?.is_nil(),
            "fixture Writer account route refused"
        );
    }
    if f.format == "pptx" {
        ensure!(
            a.label == "iCloudImpressValidation"
                && a.enabled
                && a.drive.id == "drive"
                && a.drive.drive_type == "icloud_drive"
                && a.root_id == cirrove_icloud::ROOT_ID
                && a.mount_path == f.session_directory.join("mount")
                && !Uuid::parse_str(&a.credential_id)?.is_nil(),
            "fixture Impress account route refused"
        );
    }
    Ok(a)
}
fn native_trash_account_binding(f: &Fixture, accounts: &[Account]) -> Result<Account> {
    let a = account_binding(f, accounts)?;
    ensure!(
        a.label == "iCloudNativeTrashLossValidation"
            && a.enabled
            && a.access == cirrove_auth::AccessMode::ReadWrite
            && a.drive.id == "drive"
            && a.drive.drive_type == "icloud_drive"
            && a.root_id == cirrove_icloud::ROOT_ID
            && a.mount_path == f.session_directory.join("mount")
            && !Uuid::parse_str(&a.credential_id)?.is_nil(),
        "native Trash preflight account refused"
    );
    Ok(a)
}
fn native_trash_source_stamp(file: &File) -> Result<(u64, u64, u64, i64, i64, i64, i64)> {
    let m = file.metadata()?;
    ensure!(
        m.is_file() && m.nlink() == 1 && m.mode() & 0o7777 == 0o400,
        "native Trash preflight immutable source refused"
    );
    Ok((
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    ))
}
fn proof(f: &Fixture, file: &File, receipt: &PackageDownload, source: bool) -> Result<()> {
    if source || f.representation == FixtureRepresentation::Data {
        ensure!(
            receipt.size == f.source_size && receipt.sha256 == f.source_sha256,
            "fixture content digest differs"
        );
    }
    if let Some(expected) = &f.semantic {
        let actual = if source && f.source_root.is_none() {
            cirrove_icloud::package_flat_archive_semantic_identity_v2(
                file,
                receipt,
                &CancellationToken::new(),
            )?
        } else {
            cirrove_icloud::package_archive_semantic_identity_versioned(
                file,
                receipt,
                if source {
                    f.source_root
                        .as_deref()
                        .context("fixture source root absent")?
                } else {
                    &f.expected_root
                },
                expected.version,
                &CancellationToken::new(),
            )?
        };
        ensure!(&actual == expected, "fixture semantic content differs");
    }
    Ok(())
}
/// Public only under icloud-write-probe. All output is identity/digest evidence.
pub async fn icloud_owned_fixture_verify(
    path: &Path,
    manifest_sha256: &str,
) -> Result<serde_json::Value> {
    verify_fixture(path, manifest_sha256, FixtureArm::Generic, None).await
}
/// Explicit standalone native Trash preflight; no generic ownership widening.
pub(crate) async fn icloud_owned_native_trash_fixture_verify(
    path: &Path,
    manifest_sha256: &str,
) -> Result<serde_json::Value> {
    verify_fixture(path, manifest_sha256, FixtureArm::NativeTrash, None).await
}
// Explicit registered caller's ORIGINAL monotonic deadline. Public generic
// callers retain their historical behavior; no new standalone clock is created.
async fn icloud_owned_fixture_verify_before(
    path: &Path,
    manifest_sha256: &str,
    active_end: tokio::time::Instant,
) -> Result<serde_json::Value> {
    verify_fixture(path, manifest_sha256, FixtureArm::Generic, Some(active_end)).await
}
// Only the explicit receipt-selected ordinary Pages DATA route selects this
// reader; FlatPages PACKAGE and public generic contracts stay unchanged.
async fn icloud_owned_pages_data_fixture_verify_before(
    path: &Path,
    digest: &str,
    active_end: tokio::time::Instant,
) -> Result<serde_json::Value> {
    verify_fixture(path, digest, FixtureArm::PagesData, Some(active_end)).await
}
fn validate_pages_data(f: &Fixture) -> Result<()> {
    validate_for(f, FixtureArm::PagesData)?;
    ensure!(
        f.format == "pages"
            && f.representation == FixtureRepresentation::Data
            && f.source_root.is_none()
            && f.semantic.is_none()
            && f.session_directory == format!("/var/tmp/cirrove-pages-data-{}", f.run)
            && ["source-a.pages", "source-b.pages"]
                .iter()
                .any(|name| f.source == f.session_directory.join(name))
            && f.expected_root == format!("Cirrove-Pages-Data-{}.pages", f.run),
        "explicit Pages DATA fixture refused"
    );
    Ok(())
}
// Only the explicit receipt-selected ordinary Keynote DATA route selects this
// reader; Keynote PACKAGE and public generic contracts stay unchanged.
async fn icloud_owned_keynote_data_fixture_verify_before(
    path: &Path,
    digest: &str,
    active_end: tokio::time::Instant,
) -> Result<serde_json::Value> {
    verify_fixture(path, digest, FixtureArm::KeynoteData, Some(active_end)).await
}
fn validate_keynote_data(f: &Fixture) -> Result<()> {
    validate_for(f, FixtureArm::KeynoteData)?;
    ensure!(
        f.format == "keynote"
            && f.representation == FixtureRepresentation::Data
            && f.source_root.is_none()
            && f.semantic.is_none()
            && f.session_directory == format!("/var/tmp/cirrove-keynote-data-{}", f.run)
            && ["source-a.key", "source-b.key"]
                .iter()
                .any(|name| f.source == f.session_directory.join(name))
            && f.expected_root == format!("Cirrove-Keynote-Data-{}.key", f.run),
        "explicit Keynote DATA fixture refused"
    );
    Ok(())
}
// Only an explicitly validated Pages FlatPages replacement selects this route.
async fn icloud_owned_flat_pages_fixture_verify_before(
    path: &Path,
    digest: &str,
    active_end: tokio::time::Instant,
) -> Result<serde_json::Value> {
    verify_fixture(path, digest, FixtureArm::FlatPages, Some(active_end)).await
}
fn validate_flat_pages(f: &Fixture) -> Result<()> {
    validate_for(f, FixtureArm::FlatPages)?;
    ensure!(
        f.format == "pages"
            && f.representation == FixtureRepresentation::Package
            && f.source_root.is_none()
            && f.semantic.as_ref().is_some_and(|v| v.version == 2),
        "explicit flat Pages fixture refused"
    );
    Ok(())
}
fn fixture_active(active_end: Option<tokio::time::Instant>) -> Result<()> {
    if let Some(end) = active_end {
        ensure!(
            tokio::time::Instant::now() < end,
            "fixture original deadline expired"
        );
    }
    Ok(())
}
fn fixture_publish(
    active_end: Option<tokio::time::Instant>,
    publish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    fixture_active(active_end)?;
    publish()?;
    fixture_active(active_end)
}
async fn verify_fixture(
    path: &Path,
    manifest_sha256: &str,
    arm: FixtureArm,
    active_end: Option<tokio::time::Instant>,
) -> Result<serde_json::Value> {
    let directory = path.parent().context("fixture directory missing")?;
    let directory_guard = open_private(directory, true, 0)?;
    let mut bytes = Vec::new();
    open_private(path, false, 32 * 1024)?
        .take(32 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    let f = decode(&bytes, manifest_sha256)?;
    match arm {
        FixtureArm::Generic => validate(&f)?,
        FixtureArm::FlatPages => validate_flat_pages(&f)?,
        FixtureArm::PagesData => validate_pages_data(&f)?,
        FixtureArm::KeynoteData => validate_keynote_data(&f)?,
        FixtureArm::NativeTrash => {
            validate_native_trash(&f)?;
            ensure!(
                path == f.session_directory.join("preflight-fixture.json"),
                "native Trash preflight filename refused"
            );
        }
    }
    let _session_directory = open_private(&f.session_directory, true, 0)?;
    let _state = open_private(&f.session_directory.join("state"), true, 0)?;
    let settings_path = f.session_directory.join("state/accounts.json");
    let mut settings_bytes = Vec::new();
    open_private(&settings_path, false, 256 * 1024)?
        .take(256 * 1024 + 1)
        .read_to_end(&mut settings_bytes)?;
    ensure!(
        settings_bytes.len() <= 256 * 1024
            && hex::encode(Sha256::digest(&settings_bytes)) == f.settings_sha256,
        "fixture settings changed"
    );
    let settings: Settings = serde_json::from_slice(&settings_bytes)
        .map_err(|_| anyhow::anyhow!("fixture account settings refused"))?;
    ensure!(settings.version == 2, "fixture account schema unsupported");
    let account = match arm {
        FixtureArm::Generic | FixtureArm::FlatPages => account_binding(&f, &settings.accounts)?,
        FixtureArm::PagesData | FixtureArm::KeynoteData => {
            let a = account_binding(&f, &settings.accounts)?;
            ensure!(
                a.label
                    == if arm == FixtureArm::PagesData {
                        "iCloudPagesDataValidation"
                    } else {
                        "iCloudKeynoteDataValidation"
                    }
                    && a.enabled
                    && a.access == cirrove_auth::AccessMode::ReadWrite
                    && a.drive.id == "drive"
                    && a.drive.drive_type == "icloud_drive"
                    && a.root_id == cirrove_icloud::ROOT_ID
                    && a.mount_path == f.session_directory.join("mount")
                    && !Uuid::parse_str(&a.credential_id)?.is_nil(),
                "Pages DATA fixture account refused"
            );
            a
        }
        FixtureArm::NativeTrash => native_trash_account_binding(&f, &settings.accounts)?,
    };
    let mut source = open_private(&f.source, false, LIMIT)?;
    let source_stamp = if matches!(
        arm,
        FixtureArm::NativeTrash | FixtureArm::PagesData | FixtureArm::KeynoteData
    ) || f.format == "docx"
        || f.format == "pptx"
    {
        Some(native_trash_source_stamp(&source)?)
    } else {
        None
    };
    let source_receipt = PackageDownload {
        size: source.metadata()?.len(),
        sha256: sha256(&mut source)?,
    };
    proof(&f, &source, &source_receipt, true)?;
    same_directory(directory, &directory_guard)?;
    let attempt = verification_directory(directory)?;
    let attempt_guard = open_private(&attempt, true, 0)?;
    match arm {
        FixtureArm::Generic => manifest(&attempt, f.run, "owned-fixture-read-only")?,
        FixtureArm::FlatPages | FixtureArm::PagesData | FixtureArm::KeynoteData => {
            manifest_with_duration(&attempt, f.run, "owned-fixture-read-only", 600)?
        }
        FixtureArm::NativeTrash => {
            manifest_with_duration(&attempt, f.run, "owned-fixture-read-only", 600)?;
        }
    }
    // Sealed saved session only: this helper never calls sign_in/account_login.
    fixture_active(active_end)?;
    let mut remote = session(&f.session_directory, &account)
        .await
        .map_err(|_| anyhow::anyhow!("fixture sealed session unavailable"))?;
    let output = attempt.join("download.bin");
    let staged = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)?;
    let mut sink = DiskSink(tokio::fs::File::from_std(staged.try_clone()?));
    fixture_active(active_end)?;
    let receipt = remote
        .read_owned_fixture(
            &f.parent,
            &f.document,
            f.representation,
            &mut sink,
            &CancellationToken::new(),
        )
        .await?;
    sink.0.flush().await?;
    sink.0.sync_all().await?;
    proof(&f, &staged, &receipt, false)?;
    // Evidence output is withheld if the preregistration or original archive moved.
    let mut end = Vec::new();
    open_private(path, false, 32 * 1024)?
        .take(32 * 1024 + 1)
        .read_to_end(&mut end)?;
    let _ = decode(&end, manifest_sha256)?;
    let mut final_source = open_private(&f.source, false, LIMIT)?;
    ensure!(
        sha256(&mut final_source)? == f.source_sha256,
        "fixture source changed during observation"
    );
    if let Some(expected) = source_stamp {
        ensure!(
            native_trash_source_stamp(&source)? == expected
                && native_trash_source_stamp(&final_source)? == expected,
            "native Trash preflight source stamp changed"
        );
        let mut final_settings = Vec::new();
        open_private(&settings_path, false, 256 * 1024)?
            .take(256 * 1024 + 1)
            .read_to_end(&mut final_settings)?;
        ensure!(
            final_settings == settings_bytes,
            "native Trash preflight settings changed"
        );
    }
    let mut result = serde_json::json!({"run":f.run,"account":f.account,"parent":f.parent.drivewsid,"item":f.document.drivewsid,"etag":f.document.etag,"representation":f.representation,"format":f.format,"size":receipt.size,"sha256":receipt.sha256,"semantic":f.semantic,"manifest_sha256":manifest_sha256,"content_identity_verified":true,"gui_fidelity_verified":false,"cloud_mutated":false});
    if arm == FixtureArm::NativeTrash {
        result["arm"] = serde_json::json!("native_trash_preflight");
    }
    if arm == FixtureArm::FlatPages {
        result["source_layout"] = serde_json::json!("flat_pages");
    }
    same_directory(directory, &directory_guard)?;
    same_directory(&attempt, &attempt_guard)?;
    fixture_publish(active_end, || {
        record(&attempt.join("receipt.json"), &result)
    })?;
    Ok(result)
}

mod retained_numbers_read;
pub use retained_numbers_read::icloud_owned_retained_numbers_read;

mod editor_metadata;
pub use editor_metadata::icloud_owned_editor_metadata;

mod calc_metadata;
pub use calc_metadata::{
    icloud_owned_calc_metadata, icloud_owned_impress_metadata, icloud_owned_writer_metadata,
};

mod calc_trash_original;
pub use calc_trash_original::{
    icloud_owned_calc_trash_original, icloud_owned_impress_trash_original,
    icloud_owned_writer_trash_original,
};

mod editor_source;
pub use editor_source::icloud_owned_editor_source_proof;

mod native_import_fixture;
pub use native_import_fixture::icloud_owned_native_import_fixture_verify;

mod receipt_bound;
pub use receipt_bound::{
    icloud_owned_fuse_capture_verify, icloud_owned_fuse_receipt_verify,
    icloud_owned_fuse_source_verify, icloud_owned_keynote_data_receipt_verify,
    icloud_owned_keynote_data_source_verify, icloud_owned_keynote_fuse_capture_verify,
    icloud_owned_keynote_fuse_receipt_verify, icloud_owned_keynote_fuse_source_verify,
    icloud_owned_keynote_import_receipt_verify, icloud_owned_keynote_replacement_receipt_verify,
    icloud_owned_keynote_source_verify, icloud_owned_numbers_data_receipt_verify,
    icloud_owned_numbers_data_source_verify, icloud_owned_pages_data_receipt_verify,
    icloud_owned_pages_data_source_verify, icloud_owned_pages_fuse_capture_verify,
    icloud_owned_pages_fuse_receipt_verify, icloud_owned_pages_fuse_source_verify,
    icloud_owned_pages_replacement_receipt_verify, icloud_owned_receipt_verify,
};

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    fn fixture() -> Result<(Vec<u8>, Fixture)> {
        let run = Uuid::new_v4();
        let account = Uuid::new_v4();
        let name = format!("Owned-{run}");
        let bytes = serde_json::to_vec(
            &serde_json::json!({"version":1,"run":run,"account":account,
            "session_directory":"/var/tmp/owned-fixture-session","settings_sha256":"a".repeat(64),
            "parent":{"drivewsid":"FOLDER::com.apple.CloudDocs::owned","name":format!("Cirrove-Native-{run}"),"zone":"com.apple.CloudDocs","type":"FOLDER"},
            "document":{"drivewsid":"FILE::com.apple.CloudDocs::doc","docwsid":"doc","parentId":"FOLDER::com.apple.CloudDocs::owned","name":name,"extension":"pages","zone":"com.apple.CloudDocs","type":"FILE","etag":"r1","size":3},
            "format":"pages","representation":"data","source":"/var/tmp/owned-source.pages","source_size":3,"source_sha256":hex::encode(Sha256::digest(b"abc")),"source_root":"Export.pages","expected_root":format!("{name}.pages"),"semantic":null}),
        )?;
        let f = decode(&bytes, &hex::encode(Sha256::digest(&bytes)))?;
        Ok((bytes, f))
    }
    fn writer_fixture() -> Result<Fixture> {
        let (_, mut f) = fixture()?;
        f.format = "docx".into();
        f.session_directory = calc_metadata::EditorArm::Writer.editor_root(f.run);
        f.document.name = format!("Cirrove-Writer-{}", f.run);
        f.document.extension = "docx".into();
        f.source = f.session_directory.join("source-a.docx");
        f.source_root = None;
        f.expected_root = format!("{}.docx", f.document.name);
        Ok(f)
    }
    #[test]
    fn owned_writer_fixture_accepts_raw_docx_and_checks_readback_bytes() -> Result<()> {
        let mut f = writer_fixture()?;
        validate(&f)?;
        f.source = f.session_directory.join("source-b.docx");
        validate(&f)?;
        let retained = tempfile::tempdir()?.keep();
        std::fs::set_permissions(&retained, std::fs::Permissions::from_mode(0o700))?;
        let path = retained.join("readback.docx");
        std::fs::write(&path, b"abc")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
        let file = open_private(&path, false, LIMIT)?;
        proof(
            &f,
            &file,
            &PackageDownload {
                size: 3,
                sha256: hex::encode(Sha256::digest(b"abc")),
            },
            false,
        )?;
        assert!(
            proof(
                &f,
                &file,
                &PackageDownload {
                    size: 3,
                    sha256: hex::encode(Sha256::digest(b"xyz"))
                },
                false
            )
            .is_err(),
            "Writer DATA changed bytes admitted"
        );
        assert!(
            proof(
                &f,
                &file,
                &PackageDownload {
                    size: 4,
                    sha256: f.source_sha256.clone()
                },
                false
            )
            .is_err(),
            "Writer DATA changed size admitted"
        );
        Ok(())
    }
    #[test]
    fn owned_writer_fixture_refuses_cross_arm_package_semantic_and_account() -> Result<()> {
        let original = writer_fixture()?;
        for arm in 0..8 {
            let mut f = writer_fixture()?;
            match arm {
                0 => {
                    f.session_directory = calc_metadata::EditorArm::Calc.editor_root(f.run);
                    f.source = f.session_directory.join("source-a.docx");
                }
                1 => f.source = f.session_directory.join("source-a.xlsx"),
                2 => f.document.name = format!("Cirrove-Calc-{}", f.run),
                3 => f.representation = FixtureRepresentation::Package,
                4 => f.source_root = Some("Source.docx".into()),
                5 => {
                    f.semantic = Some(PackageSemanticIdentity {
                        version: 2,
                        sha256: "a".repeat(64),
                        entries: 2,
                        files: 1,
                        expanded_bytes: 3,
                    })
                }
                6 => f.document.extension = "xlsx".into(),
                _ => f.document.parent_id = "FOLDER::com.apple.CloudDocs::foreign".into(),
            }
            assert!(validate(&f).is_err(), "Writer fixture arm {arm}");
        }
        let a: Account = serde_json::from_value(
            serde_json::json!({"id":original.account,"label":"iCloudWriterValidation",
            "registration":{"provider":"i_cloud"},"identity":{"tenant_id":"icloud","subject":"synthetic",
            "username":"synthetic.invalid","graph_user_id":"synthetic","display_name":"Synthetic"},
            "credential_id":Uuid::new_v4(),"access":"read_only","drive":{"id":"drive","name":"Drive","driveType":"icloud_drive"},
            "root_id":cirrove_icloud::ROOT_ID,"mount_path":original.session_directory.join("mount"),"enabled":true,
            "poll_seconds":3600,"cache_bytes":1048576}),
        )?;
        account_binding(&original, std::slice::from_ref(&a))?;
        for arm in 0..4 {
            let mut bad = a.clone();
            match arm {
                0 => bad.label = "iCloudCalcValidation".into(),
                1 => bad.id = Uuid::new_v4().to_string(),
                2 => bad.drive.id = "foreign".into(),
                _ => bad.mount_path = "/var/tmp/foreign".into(),
            }
            assert!(
                account_binding(&original, &[bad]).is_err(),
                "Writer fixture account arm {arm}"
            );
        }
        Ok(())
    }
    fn impress_fixture() -> Result<Fixture> {
        let (_, mut f) = fixture()?;
        f.format = "pptx".into();
        f.session_directory = calc_metadata::EditorArm::Impress.editor_root(f.run);
        f.document.name = format!("Cirrove-Impress-{}", f.run);
        f.document.extension = "pptx".into();
        f.source = f.session_directory.join("source-a.pptx");
        f.source_root = None;
        f.expected_root = format!("{}.pptx", f.document.name);
        Ok(f)
    }
    #[test]
    fn owned_impress_fixture_accepts_raw_pptx_and_checks_readback_bytes() -> Result<()> {
        let mut f = impress_fixture()?;
        validate(&f)?;
        f.source = f.session_directory.join("source-b.pptx");
        validate(&f)?;
        let retained = tempfile::tempdir()?.keep();
        std::fs::set_permissions(&retained, std::fs::Permissions::from_mode(0o700))?;
        let path = retained.join("readback.pptx");
        std::fs::write(&path, b"abc")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
        let file = open_private(&path, false, LIMIT)?;
        proof(
            &f,
            &file,
            &PackageDownload {
                size: 3,
                sha256: hex::encode(Sha256::digest(b"abc")),
            },
            false,
        )?;
        assert!(
            proof(
                &f,
                &file,
                &PackageDownload {
                    size: 3,
                    sha256: hex::encode(Sha256::digest(b"xyz"))
                },
                false
            )
            .is_err(),
            "Impress DATA changed bytes admitted"
        );
        assert!(
            proof(
                &f,
                &file,
                &PackageDownload {
                    size: 4,
                    sha256: f.source_sha256.clone()
                },
                false
            )
            .is_err(),
            "Impress DATA changed size admitted"
        );
        Ok(())
    }
    #[test]
    fn owned_impress_fixture_refuses_cross_arm_package_semantic_and_account() -> Result<()> {
        let original = impress_fixture()?;
        for arm in 0..9 {
            let mut f = impress_fixture()?;
            match arm {
                0 => {
                    f.session_directory = calc_metadata::EditorArm::Calc.editor_root(f.run);
                    f.source = f.session_directory.join("source-a.pptx");
                }
                1 => f.source = f.session_directory.join("source-a.xlsx"),
                2 => f.document.name = format!("Cirrove-Calc-{}", f.run),
                3 => f.representation = FixtureRepresentation::Package,
                4 => f.source_root = Some("Source.pptx".into()),
                5 => {
                    f.semantic = Some(PackageSemanticIdentity {
                        version: 2,
                        sha256: "a".repeat(64),
                        entries: 2,
                        files: 1,
                        expanded_bytes: 3,
                    })
                }
                6 => f.document.extension = "xlsx".into(),
                7 => f.document.parent_id = "FOLDER::com.apple.CloudDocs::foreign".into(),
                _ => {
                    f.session_directory = calc_metadata::EditorArm::Writer.editor_root(f.run);
                    f.source = f.session_directory.join("source-a.pptx");
                }
            }
            assert!(validate(&f).is_err(), "Impress fixture arm {arm}");
        }
        let a: Account = serde_json::from_value(
            serde_json::json!({"id":original.account,"label":"iCloudImpressValidation",
            "registration":{"provider":"i_cloud"},"identity":{"tenant_id":"icloud","subject":"synthetic",
            "username":"synthetic.invalid","graph_user_id":"synthetic","display_name":"Synthetic"},
            "credential_id":Uuid::new_v4(),"access":"read_only","drive":{"id":"drive","name":"Drive","driveType":"icloud_drive"},
            "root_id":cirrove_icloud::ROOT_ID,"mount_path":original.session_directory.join("mount"),"enabled":true,
            "poll_seconds":3600,"cache_bytes":1048576}),
        )?;
        account_binding(&original, std::slice::from_ref(&a))?;
        for arm in 0..5 {
            let mut bad = a.clone();
            match arm {
                0 => bad.label = "iCloudCalcValidation".into(),
                1 => bad.id = Uuid::new_v4().to_string(),
                2 => bad.drive.id = "foreign".into(),
                3 => bad.mount_path = "/var/tmp/foreign".into(),
                _ => bad.label = "iCloudWriterValidation".into(),
            }
            assert!(
                account_binding(&original, &[bad]).is_err(),
                "Impress fixture account arm {arm}"
            );
        }
        Ok(())
    }
    #[test]
    fn owned_fixture_xlsx_data_accepts_exact_raw_registration() -> Result<()> {
        let (bytes, _) = fixture()?;
        let mut input: serde_json::Value = serde_json::from_slice(&bytes)?;
        input["format"] = "xlsx".into();
        input["document"]["extension"] = "xlsx".into();
        input["source"] = "/var/tmp/owned-source.xlsx".into();
        input["source_root"] = serde_json::Value::Null;
        input["expected_root"] =
            format!("{}.xlsx", input["document"]["name"].as_str().unwrap()).into();
        let registered = serde_json::to_vec(&input)?;
        let f = decode(&registered, &hex::encode(Sha256::digest(&registered)))?;
        assert_eq!(f.representation, FixtureRepresentation::Data);
        assert!(f.semantic.is_none() && f.source_root.is_none());
        validate(&f)?;
        Ok(())
    }
    #[test]
    fn owned_fixture_xlsx_refuses_package_semantic_wrapped_root_and_extension() -> Result<()> {
        let (_, mut f) = fixture()?;
        f.format = "xlsx".into();
        f.document.extension = "xlsx".into();
        f.source = "/var/tmp/owned-source.xlsx".into();
        f.source_root = None;
        f.expected_root = format!("{}.xlsx", f.document.name);
        let semantic = PackageSemanticIdentity {
            version: 2,
            sha256: "a".repeat(64),
            entries: 2,
            files: 1,
            expanded_bytes: 3,
        };
        semantic.validate()?;
        f.representation = FixtureRepresentation::Package;
        assert!(
            validate(&f).is_err(),
            "XLSX PACKAGE without semantic accepted"
        );
        f.semantic = Some(semantic);
        assert!(validate(&f).is_err(), "XLSX PACKAGE with semantic accepted");
        f.representation = FixtureRepresentation::Data;
        assert!(
            validate(&f).is_err(),
            "XLSX DATA with package semantic accepted"
        );
        f.semantic = None;
        f.source_root = Some("Export.xlsx".into());
        assert!(validate(&f).is_err(), "XLSX wrapped source root accepted");
        f.source_root = None;
        f.document.extension = "numbers".into();
        assert!(
            validate(&f).is_err(),
            "XLSX document extension mismatch accepted"
        );
        f.document.extension = "xlsx".into();
        f.expected_root = format!("{}.numbers", f.document.name);
        assert!(
            validate(&f).is_err(),
            "XLSX expected extension mismatch accepted"
        );
        Ok(())
    }
    #[test]
    fn owned_fixture_registration_rejects_changed_manifest_account_parent_format() -> Result<()> {
        let (mut bytes, mut f) = fixture()?;
        validate(&f)?;
        let original_hash = hex::encode(Sha256::digest(&bytes));
        bytes.push(b' ');
        assert!(decode(&bytes, &original_hash).is_err());
        exact_account(&f, &f.account.to_string())?;
        assert!(exact_account(&f, &Uuid::new_v4().to_string()).is_err());
        f.document.parent_id = "FOLDER::com.apple.CloudDocs::foreign".into();
        assert!(validate(&f).is_err());
        let (_, mut f) = fixture()?;
        f.format = "pdf".into();
        assert!(validate(&f).is_err());
        let (_, mut f) = fixture()?;
        f.representation = FixtureRepresentation::Package;
        assert!(validate(&f).is_err());
        Ok(())
    }
    #[test]
    fn owned_fixture_private_paths_and_altered_source_fail_closed() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
        let path = root.path().join("source");
        std::fs::write(&path, b"abc")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let file = open_private(&path, false, LIMIT)?;
        let (_, f) = fixture()?;
        proof(
            &f,
            &file,
            &PackageDownload {
                size: 3,
                sha256: hex::encode(Sha256::digest(b"abc")),
            },
            true,
        )?;
        assert!(
            proof(
                &f,
                &file,
                &PackageDownload {
                    size: 3,
                    sha256: hex::encode(Sha256::digest(b"xyz"))
                },
                true
            )
            .is_err()
        );
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(root.path(), &alias)?;
        assert!(open_private(&alias.join("source"), false, LIMIT).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(open_private(&path, false, LIMIT).is_err());
        Ok(())
    }
    fn zip_archive(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        crc = !crc;
        let size = u32::try_from(bytes.len()).unwrap();
        let len = u16::try_from(name.len()).unwrap();
        let mut out = Vec::new();
        out.extend(0x04034b50u32.to_le_bytes());
        for value in [20u16, 0, 0, 0, 33] {
            out.extend(value.to_le_bytes());
        }
        for value in [crc, size, size] {
            out.extend(value.to_le_bytes());
        }
        for value in [len, 0] {
            out.extend(value.to_le_bytes());
        }
        out.extend(name.as_bytes());
        out.extend(bytes);
        let offset = u32::try_from(out.len()).unwrap();
        out.extend(0x02014b50u32.to_le_bytes());
        for value in [20u16, 20, 0, 0, 0, 33] {
            out.extend(value.to_le_bytes());
        }
        for value in [crc, size, size] {
            out.extend(value.to_le_bytes());
        }
        for value in [len, 0, 0, 0, 0] {
            out.extend(value.to_le_bytes());
        }
        for value in [0u32, 0] {
            out.extend(value.to_le_bytes());
        }
        out.extend(name.as_bytes());
        let central = u32::try_from(out.len()).unwrap() - offset;
        out.extend(0x06054b50u32.to_le_bytes());
        for value in [0u16, 0, 1, 1] {
            out.extend(value.to_le_bytes());
        }
        for value in [central, offset] {
            out.extend(value.to_le_bytes());
        }
        out.extend(0u16.to_le_bytes());
        out
    }

    #[test]
    fn owned_fixture_different_source_and_remote_roots_preserve_normalized_proof() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (_, mut f) = fixture()?;
        let source_path = root.path().join("source");
        let remote_path = root.path().join("remote");
        let source = zip_archive(
            &format!("{}/Index/Document.iwa", f.source_root.as_deref().unwrap()),
            b"fixed own content",
        );
        let remote = zip_archive(
            &format!("{}/Index/Document.iwa", f.expected_root),
            b"fixed own content",
        );
        std::fs::write(&source_path, &source)?;
        std::fs::write(&remote_path, &remote)?;
        let a = File::open(&source_path)?;
        let b = File::open(&remote_path)?;
        let ar = PackageDownload {
            size: source.len() as u64,
            sha256: hex::encode(Sha256::digest(&source)),
        };
        let br = PackageDownload {
            size: remote.len() as u64,
            sha256: hex::encode(Sha256::digest(&remote)),
        };
        f.representation = FixtureRepresentation::Package;
        f.source_size = ar.size;
        f.source_sha256 = ar.sha256.clone();
        f.semantic = Some(cirrove_icloud::package_archive_semantic_identity_versioned(
            &a,
            &ar,
            f.source_root.as_deref().unwrap(),
            2,
            &CancellationToken::new(),
        )?);
        validate(&f)?;
        proof(&f, &a, &ar, true)?;
        proof(&f, &b, &br, false)?;
        // Keep the exact remote root and valid archive, alter only content.
        // A digest-only transport success must not bypass semantic comparison.
        let changed = zip_archive(
            &format!("{}/Index/Document.iwa", f.expected_root),
            b"different own content",
        );
        std::fs::write(&remote_path, &changed)?;
        let changed_file = File::open(&remote_path)?;
        let changed_receipt = PackageDownload {
            size: changed.len() as u64,
            sha256: hex::encode(Sha256::digest(&changed)),
        };
        assert!(proof(&f, &changed_file, &changed_receipt, false).is_err());
        f.source_root = Some("Wrong.pages".into());
        assert!(proof(&f, &a, &ar, true).is_err());
        f.expected_root = "Wrong.pages".into();
        assert!(proof(&f, &b, &br, false).is_err());
        Ok(())
    }

    #[test]
    fn owned_fixture_explicit_flat_source_matches_exact_wrapped_current_v2() -> Result<()> {
        let dir = tempfile::tempdir()?.keep();
        let (raw, _) = fixture()?;
        let mut input: serde_json::Value = serde_json::from_slice(&raw)?;
        input["format"] = "numbers".into();
        input["document"]["extension"] = "numbers".into();
        let expected = format!("{}.numbers", input["document"]["name"].as_str().unwrap());
        input["expected_root"] = expected.clone().into();
        input["representation"] = "package".into();
        input["source_root"] = serde_json::Value::Null;
        let flat = zip_archive("Index/Document.iwa", b"fixed content");
        let wrapped = zip_archive(&format!("{expected}/Index/Document.iwa"), b"fixed content");
        let a_path = dir.join("flat.numbers");
        let b_path = dir.join("wrapped.numbers");
        std::fs::write(&a_path, &flat)?;
        std::fs::write(&b_path, &wrapped)?;
        let a = File::open(&a_path)?;
        let b = File::open(&b_path)?;
        let ar = PackageDownload {
            size: flat.len() as u64,
            sha256: hex::encode(Sha256::digest(&flat)),
        };
        let br = PackageDownload {
            size: wrapped.len() as u64,
            sha256: hex::encode(Sha256::digest(&wrapped)),
        };
        input["source_size"] = ar.size.into();
        input["source_sha256"] = ar.sha256.clone().into();
        input["source"] = a_path.to_string_lossy().as_ref().into();
        input["semantic"] =
            serde_json::to_value(cirrove_icloud::package_archive_semantic_identity_versioned(
                &b,
                &br,
                &expected,
                2,
                &CancellationToken::new(),
            )?)?;
        let bytes = serde_json::to_vec(&input)?;
        let result = decode(&bytes, &hex::encode(Sha256::digest(&bytes)));
        assert_eq!(std::fs::read(&a_path)?, flat);
        assert_eq!(std::fs::read(&b_path)?, wrapped);
        let f = result.expect("flat source fixture rejected before actual content proof");
        validate(&f)?;
        proof(&f, &a, &ar, true)?;
        proof(&f, &b, &br, false)?;
        assert_ne!(ar.sha256, br.sha256);
        // Only the source may be flat; provider package read retains its exact root.
        assert!(proof(&f, &a, &ar, false).is_err());
        for (name, body) in [
            ("../Index/Document.iwa", b"fixed content".as_slice()),
            ("/Index/Document.iwa", b"fixed content".as_slice()),
            ("Index\\Document.iwa", b"fixed content".as_slice()),
            ("Index/Other.iwa", b"fixed content".as_slice()),
            ("Index/Document.iwa", b"changed bytes".as_slice()),
        ] {
            let bad = zip_archive(name, body);
            let bad_path = dir.join(format!("negative-{}", hex::encode(Sha256::digest(&bad))));
            std::fs::write(&bad_path, &bad)?;
            let bad_file = File::open(&bad_path)?;
            let bad_receipt = PackageDownload {
                size: bad.len() as u64,
                sha256: hex::encode(Sha256::digest(&bad)),
            };
            // Bind independent raw identity, then exercise flat parser/content proof.
            let mut bad_input = input.clone();
            bad_input["source_size"] = bad_receipt.size.into();
            bad_input["source_sha256"] = bad_receipt.sha256.clone().into();
            let raw = serde_json::to_vec(&bad_input)?;
            let bad_fixture = decode(&raw, &hex::encode(Sha256::digest(&raw)))?;
            assert!(
                proof(&bad_fixture, &bad_file, &bad_receipt, true).is_err(),
                "flat content/path guard"
            );
        }
        input.as_object_mut().unwrap().remove("source_root");
        let missing = serde_json::to_vec(&input)?;
        assert!(decode(&missing, &hex::encode(Sha256::digest(&missing))).is_err());
        input["source_root"] = serde_json::Value::Null;
        input["format"] = "pages".into();
        input["document"]["extension"] = "pages".into();
        input["expected_root"] =
            format!("{}.pages", input["document"]["name"].as_str().unwrap()).into();
        let bytes = serde_json::to_vec(&input)?;
        assert!(validate(&decode(&bytes, &hex::encode(Sha256::digest(&bytes)))?).is_err());
        Ok(())
    }
    #[test]
    fn owned_pages_flat_fixture_requires_explicit_internal_arm_and_exact_wrapped_remote_v2()
    -> Result<()> {
        let dir = tempfile::tempdir()?.keep();
        let (raw, _) = fixture()?;
        let mut input: serde_json::Value = serde_json::from_slice(&raw)?;
        input["format"] = "pages".into();
        input["document"]["extension"] = "pages".into();
        let expected = format!("{}.pages", input["document"]["name"].as_str().unwrap());
        input["expected_root"] = expected.clone().into();
        input["representation"] = "package".into();
        input["source_root"] = serde_json::Value::Null;
        let flat = zip_archive("Index/Document.iwa", b"fixed content");
        let wrapped = zip_archive(&format!("{expected}/Index/Document.iwa"), b"fixed content");
        let a_path = dir.join("flat.pages");
        let b_path = dir.join("wrapped.pages");
        std::fs::write(&a_path, &flat)?;
        std::fs::write(&b_path, &wrapped)?;
        let a = File::open(&a_path)?;
        let b = File::open(&b_path)?;
        let ar = PackageDownload {
            size: flat.len() as u64,
            sha256: hex::encode(Sha256::digest(&flat)),
        };
        let br = PackageDownload {
            size: wrapped.len() as u64,
            sha256: hex::encode(Sha256::digest(&wrapped)),
        };
        input["source_size"] = ar.size.into();
        input["source_sha256"] = ar.sha256.clone().into();
        input["source"] = a_path.to_string_lossy().as_ref().into();
        input["semantic"] =
            serde_json::to_value(cirrove_icloud::package_archive_semantic_identity_versioned(
                &b,
                &br,
                &expected,
                2,
                &CancellationToken::new(),
            )?)?;
        let bytes = serde_json::to_vec(&input)?;
        let result = decode(&bytes, &hex::encode(Sha256::digest(&bytes)));
        assert_eq!(std::fs::read(&a_path)?, flat);
        assert_eq!(std::fs::read(&b_path)?, wrapped);
        let f = result.expect("flat source fixture rejected before actual content proof");
        assert!(
            validate(&f).is_err(),
            "public generic fixture silently inferred flat Pages"
        );
        validate_flat_pages(&f)?;
        proof(&f, &a, &ar, true)?;
        proof(&f, &b, &br, false)?;
        for arm in 0..3 {
            let mut bad = input.clone();
            match arm {
                0 => bad["source_root"] = "Source.pages".into(),
                1 => {
                    bad["format"] = "keynote".into();
                    bad["document"]["extension"] = "key".into();
                }
                _ => bad["representation"] = "data".into(),
            }
            let raw = serde_json::to_vec(&bad)?;
            assert!(
                validate_flat_pages(&decode(&raw, &hex::encode(Sha256::digest(&raw)))?).is_err(),
                "explicit Pages fixture borrowed format/layout arm {arm}"
            );
        }

        assert_ne!(ar.sha256, br.sha256);
        // Only the source may be flat; provider package read retains its exact root.
        assert!(proof(&f, &a, &ar, false).is_err());
        for (name, body) in [
            ("../Index/Document.iwa", b"fixed content".as_slice()),
            ("/Index/Document.iwa", b"fixed content".as_slice()),
            ("Index\\Document.iwa", b"fixed content".as_slice()),
            ("Index/Other.iwa", b"fixed content".as_slice()),
            ("Index/Document.iwa", b"changed bytes".as_slice()),
        ] {
            let bad = zip_archive(name, body);
            let bad_path = dir.join(format!("negative-{}", hex::encode(Sha256::digest(&bad))));
            std::fs::write(&bad_path, &bad)?;
            let bad_file = File::open(&bad_path)?;
            let bad_receipt = PackageDownload {
                size: bad.len() as u64,
                sha256: hex::encode(Sha256::digest(&bad)),
            };
            // Bind independent raw identity, then exercise flat parser/content proof.
            let mut bad_input = input.clone();
            bad_input["source_size"] = bad_receipt.size.into();
            bad_input["source_sha256"] = bad_receipt.sha256.clone().into();
            let raw = serde_json::to_vec(&bad_input)?;
            let bad_fixture = decode(&raw, &hex::encode(Sha256::digest(&raw)))?;
            assert!(
                proof(&bad_fixture, &bad_file, &bad_receipt, true).is_err(),
                "flat content/path guard"
            );
        }
        input.as_object_mut().unwrap().remove("source_root");
        let missing = serde_json::to_vec(&input)?;
        assert!(decode(&missing, &hex::encode(Sha256::digest(&missing))).is_err());
        input["source_root"] = serde_json::Value::Null;
        input["format"] = "pages".into();
        input["document"]["extension"] = "pages".into();
        input["expected_root"] =
            format!("{}.pages", input["document"]["name"].as_str().unwrap()).into();
        let bytes = serde_json::to_vec(&input)?;
        assert!(validate(&decode(&bytes, &hex::encode(Sha256::digest(&bytes)))?).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod native_trash_bridge_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    fn fixture() -> Fixture {
        let run = Uuid::new_v4();
        let root = PathBuf::from(format!("/var/tmp/cirrove-native-trash-{run}"));
        let name = format!("Cirrove-Numbers-Trash-{run}");
        serde_json::from_value(serde_json::json!({"version":1,"run":run,"account":Uuid::new_v4(),
            "session_directory":root,"settings_sha256":"a".repeat(64),
            "parent":{"drivewsid":"FOLDER::com.apple.CloudDocs::owned-parent","name":format!("Cirrove-Native-Trash-{run}"),"zone":"com.apple.CloudDocs","type":"FOLDER"},
            "document":{"drivewsid":"FILE::com.apple.CloudDocs::doc","docwsid":"doc","parentId":"FOLDER::com.apple.CloudDocs::owned-parent","name":name,"extension":"numbers","zone":"com.apple.CloudDocs","type":"FILE","etag":"selected-E1","size":3},
            "format":"numbers","representation":"package","source":root.join("source.numbers"),
            "source_size":3,"source_sha256":"b".repeat(64),"source_root":null,
            "expected_root":format!("{name}.numbers"),
            "semantic":{"version":2,"entries":2,"files":1,"expanded_bytes":3,"sha256":"c".repeat(64)}})).unwrap()
    }
    fn account(f: &Fixture) -> Account {
        Account {
            id: f.account.to_string(),
            label: "iCloudNativeTrashLossValidation".into(),
            registration: AppRegistration::ICloud,
            identity: cirrove_auth::Identity {
                tenant_id: String::new(),
                subject: "synthetic".into(),
                username: "fixture@example.invalid".into(),
                graph_user_id: String::new(),
                display_name: "Fixture".into(),
            },
            credential_id: Uuid::new_v4().to_string(),
            access: cirrove_auth::AccessMode::ReadWrite,
            drive: cirrove_core::CollectionInfo {
                id: "drive".into(),
                name: "Fixture".into(),
                drive_type: "icloud_drive".into(),
                web_url: String::new(),
            },
            root_id: cirrove_icloud::ROOT_ID.into(),
            mount_path: f.session_directory.join("mount"),
            enabled: true,
            poll_seconds: 3600,
            cache_bytes: 1024 * 1024,
        }
    }
    #[test]
    fn native_trash_preflight_bridge_is_explicit_and_generic_remains_strict() {
        let f = fixture();
        validate_native_trash(&f).unwrap();
        assert!(validate(&f).is_err());
        native_trash_account_binding(&f, &[account(&f)]).unwrap();
        for arm in 0..12 {
            let mut f = fixture();
            match arm {
                0 => f.run = Uuid::new_v4(),
                1 => f.session_directory = PathBuf::from("/var/tmp/foreign-root"),
                2 => f.parent.name = format!("Cirrove-Native-{}", f.run),
                3 => f.document.name = format!("Other-{}", f.run),
                4 => f.document.parent_id = "FOLDER::com.apple.CloudDocs::foreign".into(),
                5 => f.representation = FixtureRepresentation::Data,
                6 => f.source_root = Some("Source.numbers".into()),
                7 => f.source = f.session_directory.join("source-a.numbers"),
                8 => f.semantic = None,
                9 => f.semantic.as_mut().unwrap().version = 1,
                10 => f.expected_root = format!("Different-{}.numbers", f.run),
                _ => f.format = "pages".into(),
            }
            assert!(
                validate_native_trash(&f).is_err(),
                "native preflight arm {arm}"
            );
        }
    }
    #[test]
    fn native_trash_preflight_bridge_refuses_foreign_account_and_capability() {
        let f = fixture();
        assert!(native_trash_account_binding(&f, &[]).is_err());
        assert!(native_trash_account_binding(&f, &[account(&f), account(&f)]).is_err());
        for arm in 0..8 {
            let mut a = account(&f);
            match arm {
                0 => a.id = Uuid::new_v4().to_string(),
                1 => a.label = "iCloudNumbersBrowserValidation".into(),
                2 => a.access = cirrove_auth::AccessMode::ReadOnly,
                3 => a.enabled = false,
                4 => a.drive.id = "foreign".into(),
                5 => a.drive.drive_type = "foreign".into(),
                6 => a.mount_path = PathBuf::from("/var/tmp/foreign-mount"),
                _ => a.credential_id = Uuid::nil().to_string(),
            }
            assert!(
                native_trash_account_binding(&f, &[a]).is_err(),
                "native account arm {arm}"
            );
        }
    }
    #[test]
    fn native_trash_preflight_bridge_full_flat_source_and_remote_semantic_proof() -> Result<()> {
        let dir = tempfile::tempdir()?.keep();
        let raw = crate::native_import::synthetic_package_archive("Document", b"owned content");
        let mut f = fixture();
        let wrapped = crate::native_import::synthetic_package_archive(
            &format!("{}/Document", f.expected_root),
            b"owned content",
        );
        let source = dir.join("source.numbers");
        let remote = dir.join("remote.numbers");
        std::fs::write(&source, &raw)?;
        std::fs::write(&remote, &wrapped)?;
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o400))?;
        let a = File::open(&source)?;
        let b = File::open(&remote)?;
        let ar = PackageDownload {
            size: raw.len() as u64,
            sha256: hex::encode(Sha256::digest(&raw)),
        };
        let br = PackageDownload {
            size: wrapped.len() as u64,
            sha256: hex::encode(Sha256::digest(&wrapped)),
        };
        f.source_size = ar.size;
        f.source_sha256 = ar.sha256.clone();
        f.semantic = Some(cirrove_icloud::package_flat_archive_semantic_identity_v2(
            &a,
            &ar,
            &CancellationToken::new(),
        )?);
        validate_native_trash(&f)?;
        native_trash_source_stamp(&a)?;
        proof(&f, &a, &ar, true)?;
        proof(&f, &b, &br, false)?;
        f.semantic.as_mut().unwrap().sha256 = "0".repeat(64);
        assert!(proof(&f, &a, &ar, true).is_err());
        assert!(proof(&f, &b, &br, false).is_err());
        assert_eq!(std::fs::read(&source)?, raw);
        assert_eq!(std::fs::read(&remote)?, wrapped);
        Ok(())
    }
}

mod native_pre_trash_preservation;
pub use native_pre_trash_preservation::{
    icloud_owned_native_after_install_preflight_preservation,
    icloud_owned_native_pre_trash_preservation,
};
