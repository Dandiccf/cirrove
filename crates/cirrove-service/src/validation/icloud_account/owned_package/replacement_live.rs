//! Exact-owned read-only evidence around a separately submitted public replacement.
//! Neither entry point queues, retries, renames, restores, or deletes cloud items.
use super::public_verify::{
    PublicImportBinding, exact_entry, retained_public_import_for_replacement,
};
use super::*;
use crate::journal::{PackagePublicationStatus, UploadRecord};
use cirrove_core::upload::{PackageSemanticIdentity, UploadRepresentation};
use std::os::unix::fs::MetadataExt;

const RUN: &str = "42a313de-1e94-488f-aed8-e7c704944fed";
const IMPORT_RUN: &str = "ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc";
const ACCOUNT: &str = "ca43ba36-9f5c-4784-9ae9-20e5c3cbe319";
const ORIGINAL: &str = "FILE::com.apple.CloudDocs::8A909C8D-32D7-4E39-9202-DF1862E46C5A";
const PUBLIC: &str = "/var/tmp/cirrove-public-native-ec7f82e1";
const DIRECTORY: &str =
    "/var/tmp/cirrove-native-replacement-live-42a313de-1e94-488f-aed8-e7c704944fed";
const ARCHIVE_SHA: &str = "83527e22cb8c56dd07a4d2a82acbc711711ff24d912df46f218eaab4ef62e776";
const ROOT: &str = "Replacement.pages";

#[derive(serde::Deserialize)]
struct Source {
    run: Uuid,
    archive: PathBuf,
    archive_sha256: String,
    expected_root: String,
    replacement_submitted: bool,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Preflight {
    run: Uuid,
    account: String,
    scope: Scope,
    original: Node,
    old_semantic: PackageSemanticIdentity,
    new_semantic: PackageSemanticIdentity,
    archive: PathBuf,
    archive_size: u64,
    archive_sha256: String,
    expected_root: String,
    visible_path: String,
}
fn owned_directory(path: &Path) -> Result<()> {
    check_directory(path)?;
    ensure!(
        std::fs::symlink_metadata(path)?.uid() == std::fs::metadata("/proc/self")?.uid(),
        "replacement evidence directory is not owned"
    );
    ensure!(
        path.canonicalize()? == path,
        "replacement evidence directory is not canonical"
    );
    Ok(())
}
fn bound_run(run: Uuid) -> Result<&'static Path> {
    ensure!(
        run.to_string() == RUN,
        "replacement run is not preregistered"
    );
    let dir = Path::new(DIRECTORY);
    owned_directory(dir)?;
    Ok(dir)
}
fn source_binding(source: &Source, dir: &Path, run: Uuid) -> Result<()> {
    ensure!(
        source.run == run
            && run.to_string() == RUN
            && source.archive == dir.join("replacement-source.zip")
            && source.archive_sha256 == ARCHIVE_SHA
            && source.expected_root == ROOT
            && !source.replacement_submitted,
        "edited archive manifest binding changed"
    );
    Ok(())
}
fn original_binding(binding: &PublicImportBinding) -> Result<()> {
    ensure!(
        binding.account.id == ACCOUNT
            && binding.node.id == ORIGINAL
            && binding.node.parent_id.as_deref() == Some(binding.plan.parent.as_str()),
        "replacement original account or identity changed"
    );
    Ok(())
}
// Native Folder projections use ETag with no content_version. The old
// preflight copied the retained import's exact historical ETag alias before
// changing ETag to the selected revision. Accept only that proven legacy alias.
fn canonical_preflight_original(selected: &Node, retained: &Node) -> Result<Node> {
    let legacy = retained
        .content_version
        .as_ref()
        .filter(|v| Some(*v) == retained.etag.as_ref());
    ensure!(
        selected.kind == NodeKind::Folder
            && selected.package
            && selected.target.is_none()
            && (selected.content_version.is_none() || selected.content_version.as_ref() == legacy),
        "replacement preflight content revision is not the retained alias"
    );
    let mut expected = retained.clone();
    expected.etag = selected.etag.clone();
    expected.content_version = None;
    let mut canonical = selected.clone();
    canonical.content_version = None;
    ensure!(
        canonical == expected,
        "replacement preflight original identity changed"
    );
    Ok(canonical)
}
fn preflight_binding(
    p: &Preflight,
    binding: &PublicImportBinding,
    dir: &Path,
) -> Result<Preflight> {
    original_binding(binding)?;
    let original = canonical_preflight_original(&p.original, &binding.node)?;
    ensure!(
        p.run.to_string() == RUN
            && p.account == ACCOUNT
            && p.scope == scope(&binding.account)
            && p.original.etag.as_ref().is_some_and(|e| !e.is_empty())
            && p.old_semantic == binding.semantic
            && p.new_semantic != p.old_semantic
            && p.archive_sha256 == ARCHIVE_SHA
            && p.archive_size > 0
            && p.archive_size <= LIMIT
            && p.expected_root == ROOT
            && p.visible_path == format!("{}/{}", binding.plan.parent_name, p.original.name)
            && p.archive
                .file_name()
                .is_some_and(|n| n == "replacement.zip")
            && p.archive.parent().and_then(Path::parent) == Some(dir),
        "replacement preflight binding changed"
    );
    p.old_semantic.validate()?;
    p.new_semantic.validate()?;
    let mut canonical = p.clone();
    canonical.original = original;
    Ok(canonical)
}
#[cfg(test)]
fn semantic_file(
    path: &Path,
    receipt: &PackageDownload,
    root: &str,
) -> Result<PackageSemanticIdentity> {
    semantic_file_versioned(
        path,
        receipt,
        root,
        cirrove_icloud::PACKAGE_SEMANTIC_IDENTITY_VERSION,
    )
}
fn semantic_file_versioned(
    path: &Path,
    receipt: &PackageDownload,
    root: &str,
    version: u32,
) -> Result<PackageSemanticIdentity> {
    let file = private_file(path, LIMIT)?;
    ensure!(
        file.metadata()?.uid() == std::fs::metadata("/proc/self")?.uid(),
        "replacement evidence file is not owned"
    );
    Ok(cirrove_icloud::package_archive_semantic_identity_versioned(
        &file,
        receipt,
        root,
        version,
        &CancellationToken::new(),
    )?)
}
async fn active_parent(remote: &ICloudReadSession, binding: &PublicImportBinding) -> Result<()> {
    let provider = cirrove_icloud::ICloudDrive::on_demand_from_session_snapshot(
        scope(&binding.account),
        &binding.account.identity.username,
        &remote.session_snapshot()?,
    )?;
    let parent = cirrove_core::ReadProvider::node(
        &provider,
        &scope(&binding.account),
        &binding.plan.parent,
        &CancellationToken::new(),
    )
    .await?;
    ensure!(
        parent.id == binding.plan.parent
            && parent.kind == NodeKind::Folder
            && !parent.package
            && parent.target.is_none()
            && parent.parent_id.as_deref() == Some(ROOT_ID)
            && parent.name == binding.plan.parent_name,
        "owned active parent changed"
    );
    Ok(())
}
fn prepared_replacement_binding(row: &UploadRecord, p: &Preflight, operation: Uuid) -> Result<()> {
    ensure!(
        row.id == operation
            && !operation.is_nil()
            && row.scope == p.scope
            && row.intent
                == UploadIntent::Replace {
                    item: p.original.id.clone(),
                    expected_etag: p
                        .original
                        .etag
                        .clone()
                        .context("original revision missing")?,
                }
            && row.representation
                == UploadRepresentation::PackageReplacementArchive {
                    expected_root: p.expected_root.clone(),
                    semantic: p.new_semantic.clone(),
                    original: Box::new(p.original.clone()),
                    original_semantic: p.old_semantic.clone(),
                }
            && row.size == p.archive_size
            && row.sha256 == p.archive_sha256
            && row.base.is_none()
            && row.working_file.is_none(),
        "replacement journal is not this exact prepared operation"
    );
    Ok(())
}
fn replacement_binding<'a>(
    row: &'a UploadRecord,
    p: &Preflight,
    operation: Uuid,
) -> Result<(&'a Node, &'a Node)> {
    prepared_replacement_binding(row, p, operation)?;
    let (original, current, backup) = row
        .native_replacement_receipt()
        .context("replacement lacks a verified typed handoff receipt")?;
    ensure!(
        original == &p.original,
        "replacement original receipt changed"
    );
    Ok((current, backup))
}

fn publication_binding(status: &PackagePublicationStatus, current: &Node) -> Result<()> {
    ensure!(
        matches!(status, PackagePublicationStatus::Present(node) if node == current),
        "replacement canonical publication is unconfirmed"
    );
    Ok(())
}

pub async fn icloud_public_native_replacement_preflight(run: Uuid) -> Result<()> {
    let dir = bound_run(run)?;
    ensure!(
        !dir.join("preflight.json").exists(),
        "replacement preflight already retained"
    );
    let attempt = verification_directory(dir)?;
    manifest(&attempt, run, "replacement-preflight-read-only")?;
    let source: Source = read_json(&dir.join("source.json"), 32 * 1024)?;
    source_binding(&source, dir, run)?;
    let binding = retained_public_import_for_replacement(Uuid::parse_str(IMPORT_RUN)?, None)?;
    original_binding(&binding)?;
    let source_path = source.archive;
    let stage = attempt.clone();
    let archive = attempt.join("replacement.zip");
    let output = archive.clone();
    let (new_semantic, archive_size, archive_sha256) =
        tokio::task::spawn_blocking(move || -> Result<_> {
            let capture = crate::native_import::ValidatedPackageArchive::capture(
                &source_path,
                &stage,
                ROOT,
                &CancellationToken::new(),
            )?;
            let (mut file, representation, size, hash) = capture.into_parts();
            ensure!(hash == ARCHIVE_SHA, "edited archive raw receipt changed");
            let UploadRepresentation::PackageArchive {
                expected_root,
                semantic,
            } = representation
            else {
                anyhow::bail!("edited archive is not a validated package");
            };
            ensure!(expected_root == ROOT, "edited archive root changed");
            let mut retained = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&output)?;
            std::io::Seek::rewind(&mut file)?;
            ensure!(
                std::io::copy(&mut file, &mut retained)? == size,
                "edited snapshot copy incomplete"
            );
            retained.sync_all()?;
            retained.set_permissions(std::fs::Permissions::from_mode(0o400))?;
            retained.sync_all()?;
            File::open(&stage)?.sync_all()?;
            Ok((semantic, size, hash))
        })
        .await??;
    ensure!(
        new_semantic != binding.semantic,
        "replacement contents are unchanged"
    );
    let mut remote = session(Path::new(PUBLIC), &binding.account).await?;
    active_parent(&remote, &binding).await?;
    let entries = remote.list_folder(&binding.plan.parent).await?;
    let entry = entries
        .iter()
        .find(|e| e.drivewsid == ORIGINAL)
        .context("owned original missing")?;
    let mut original = binding.node.clone();
    original.etag = Some(entry.etag.clone());
    original.content_version = None;
    ensure!(!entry.etag.is_empty(), "owned original revision missing");
    let entry = exact_entry(&entries, &original)?;
    let receipt = download(
        &mut remote,
        &binding.plan.parent,
        &entry,
        &attempt.join("original.zip"),
    )
    .await?;
    let old_semantic = semantic_file_versioned(
        &attempt.join("original.zip"),
        &receipt,
        &original.name,
        binding.semantic.version,
    )?;
    ensure!(
        old_semantic == binding.semantic,
        "owned original semantic content changed"
    );
    ensure!(
        exact_entry(&remote.list_folder(&binding.plan.parent).await?, &original)? == entry,
        "original changed during preflight"
    );
    let proof = Preflight {
        run,
        account: binding.account.id.clone(),
        scope: scope(&binding.account),
        visible_path: format!("{}/{}", binding.plan.parent_name, original.name),
        original,
        old_semantic,
        new_semantic,
        archive,
        archive_size,
        archive_sha256,
        expected_root: ROOT.into(),
    };
    let proof = preflight_binding(&proof, &binding, dir)?;
    record(&dir.join("preflight.json"), &proof)?;
    File::open(dir)?.sync_all()?;
    println!("Owned replacement preflight retained; no replacement submitted.");
    Ok(())
}

pub async fn icloud_public_native_replacement_verify(run: Uuid, operation: Uuid) -> Result<()> {
    let dir = bound_run(run)?;
    let attempt = verification_directory(dir)?;
    manifest(&attempt, run, "replacement-postflight-read-only")?;
    let binding =
        retained_public_import_for_replacement(Uuid::parse_str(IMPORT_RUN)?, Some(operation))?;
    let proof: Preflight = read_json(&dir.join("preflight.json"), 64 * 1024)?;
    let proof = preflight_binding(&proof, &binding, dir)?;
    owned_directory(proof.archive.parent().context("snapshot parent missing")?)?;
    ensure!(
        semantic_file_versioned(
            &proof.archive,
            &PackageDownload {
                size: proof.archive_size,
                sha256: proof.archive_sha256.clone(),
            },
            &proof.expected_root,
            proof.new_semantic.version
        )? == proof.new_semantic,
        "edited snapshot semantic content changed"
    );
    let row = binding.journal.native_validation_upload(operation)?;
    let (current, backup) = replacement_binding(&row, &proof, operation)?;
    publication_binding(
        &binding
            .journal
            .native_validation_package_publication(operation)?,
        current,
    )?;
    let mut remote = session(Path::new(PUBLIC), &binding.account).await?;
    active_parent(&remote, &binding).await?;
    let entry = exact_entry(&remote.list_folder(&binding.plan.parent).await?, current)?;
    let receipt = download(
        &mut remote,
        &binding.plan.parent,
        &entry,
        &attempt.join("current.zip"),
    )
    .await?;
    ensure!(
        semantic_file_versioned(
            &attempt.join("current.zip"),
            &receipt,
            &current.name,
            proof.new_semantic.version
        )? == proof.new_semantic,
        "active replacement semantic content differs"
    );
    ensure!(
        exact_entry(&remote.list_folder(&binding.plan.parent).await?, current)? == entry,
        "active replacement changed during verification"
    );
    let staging = tempfile::tempfile_in(&attempt)?;
    staging.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let recovered = remote
        .verify_owned_package_in_trash(
            cirrove_icloud::OwnedPackageTrashRequest {
                apple_account: binding.account.identity.username.clone(),
                drive_id: backup.id.clone(),
                document_id: backup
                    .id
                    .strip_prefix("FILE::com.apple.CloudDocs::")
                    .context("backup identity invalid")?
                    .into(),
                expected_root: proof.original.name.clone(),
                semantic: proof.old_semantic.clone(),
            },
            staging,
            &CancellationToken::new(),
        )
        .await?;
    ensure!(
        backup.etag.as_ref() == Some(&recovered.trash_etag)
            && recovered.semantic == proof.old_semantic,
        "original recovery revision or semantics changed"
    );
    ensure!(
        exact_entry(&remote.list_folder(&binding.plan.parent).await?, current)? == entry,
        "active replacement changed during recovery verification"
    );
    active_parent(&remote, &binding).await?;
    record(
        &attempt.join("replacement-verified.json"),
        &serde_json::json!({
            "run":run, "operation":operation, "account":proof.account,
            "original":proof.original, "current":current, "backup":backup,
            "old_semantic":proof.old_semantic, "new_semantic":proof.new_semantic,
            "active_archive_sha256":receipt.sha256, "recovery_archive_sha256":recovered.archive.sha256,
            "typed_receipt_bound":true, "publication_recorded":true,
            "active_semantic_verified":true, "recoverable_original_semantic_verified":true,
            "cloud_mutated":false, "public_mount_verified":false, "apple_pages_open_verified":false,
        }),
    )?;
    File::open(&attempt)?.sync_all()?;
    println!(
        "Exact replacement and recoverable original verified read-only; mount and Apple Pages observations remain separate."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    pub(super) fn fixture() -> (Preflight, UploadRecord) {
        let semantic = |value: &str| PackageSemanticIdentity {
            version: 1,
            sha256: value.repeat(64),
            entries: 1,
            files: 1,
            expanded_bytes: 12,
        };
        let original = Node {
            id: ORIGINAL.into(),
            parent_id: Some("FOLDER::com.apple.CloudDocs::owned".into()),
            name: format!("Cirrove Public Import {IMPORT_RUN}.pages"),
            kind: NodeKind::Folder,
            size: 100,
            modified_unix: 0,
            etag: Some("old-v2".into()),
            content_version: None,
            target: None,
            package: true,
        };
        let proof = Preflight {
            run: Uuid::parse_str(RUN).unwrap(),
            account: ACCOUNT.into(),
            scope: Scope {
                account: ACCOUNT.into(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            original: original.clone(),
            old_semantic: semantic("a"),
            new_semantic: semantic("b"),
            archive: Path::new(DIRECTORY).join("verify-owned/replacement.zip"),
            archive_size: 123,
            archive_sha256: ARCHIVE_SHA.into(),
            expected_root: ROOT.into(),
            visible_path: format!("Owned/{}", original.name),
        };
        let current = Node {
            id: "FILE::com.apple.CloudDocs::new".into(),
            etag: Some("new-v1".into()),
            ..original.clone()
        };
        let backup = Node {
            parent_id: Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()),
            etag: Some("trash-v3".into()),
            ..original.clone()
        };
        let row = serde_json::from_value(serde_json::json!({
            "id":Uuid::new_v4(), "sequence":2, "scope":proof.scope,
            "intent":UploadIntent::Replace { item:original.id.clone(), expected_etag:original.etag.clone().unwrap() },
            "representation":UploadRepresentation::PackageReplacementArchive {
                expected_root:ROOT.into(), semantic:proof.new_semantic.clone(),
                original:Box::new(original.clone()), original_semantic:proof.old_semantic.clone(),
            },
            "package_completion":proof.new_semantic, "state":UploadState::Uploaded,
            "size":proof.archive_size, "sha256":proof.archive_sha256, "attempt":null, "remote":current,
            "identity_handoff": { "recovery_object":Uuid::new_v4(), "old_item":original.id,
                "recovery_name":"recovery.pages", "trash_parent":"FOLDER::com.apple.CloudDocs::TRASH_ROOT", "backup":backup },
        })).unwrap();
        (proof, row)
    }
    #[test]
    fn legacy_preflight_alias_normalizes_only_retained_import_revision() {
        let (mut proof, row) = fixture();
        let mut retained = proof.original.clone();
        retained.etag = Some("historical-import-v1".into());
        retained.content_version = retained.etag.clone();
        proof.original.content_version = retained.content_version.clone();
        let legacy = proof.original.clone();
        // Reproduce the exact former failure before applying the proven bridge.
        assert!(prepared_replacement_binding(&row, &proof, row.id).is_err());
        proof.original = canonical_preflight_original(&proof.original, &retained).unwrap();
        prepared_replacement_binding(&row, &proof, row.id).unwrap();
        replacement_binding(&row, &proof, row.id).unwrap();
        assert!(proof.original.content_version.is_none());
        let mut other = legacy.clone();
        other.content_version = Some("unrelated-alias".into());
        assert!(canonical_preflight_original(&other, &retained).is_err());
        other = legacy.clone();
        other.id.push('x');
        assert!(canonical_preflight_original(&other, &retained).is_err());
        other = legacy;
        other.etag = Some("different-selected-revision".into());
        proof.original = canonical_preflight_original(&other, &retained).unwrap();
        assert!(prepared_replacement_binding(&row, &proof, row.id).is_err());
        assert!(replacement_binding(&row, &proof, row.id).is_err());
    }
    #[test]
    fn native_replacement_live_exact_receipt_and_publication_refusals() {
        let (proof, row) = fixture();
        let (current, _) = replacement_binding(&row, &proof, row.id).unwrap();
        assert!(replacement_binding(&row, &proof, Uuid::new_v4()).is_err());
        publication_binding(&PackagePublicationStatus::Present(current.clone()), current).unwrap();
        assert!(publication_binding(&PackagePublicationStatus::Pending, current).is_err());
        assert!(publication_binding(&PackagePublicationStatus::Absent, current).is_err());
        let mut wrong = current.clone();
        wrong.etag = Some("later".into());
        assert!(publication_binding(&PackagePublicationStatus::Present(wrong), current).is_err());
        for fault in 0..10 {
            let mut bad = row.clone();
            match fault {
                0 => bad.scope.account = Uuid::new_v4().to_string(),
                1 => bad.state = UploadState::VerifyRequired,
                2 => bad.representation = UploadRepresentation::FileBytes,
                3 => bad.sha256 = "f".repeat(64),
                4 => bad.package_completion = None,
                5 => bad.remote.as_mut().unwrap().id = ORIGINAL.into(),
                6 => bad.remote.as_mut().unwrap().name = "foreign.pages".into(),
                7 => bad.identity_handoff = None,
                8 => {
                    let UploadRepresentation::PackageReplacementArchive {
                        original_semantic, ..
                    } = &mut bad.representation
                    else {
                        unreachable!()
                    };
                    original_semantic.sha256 = "c".repeat(64);
                }
                _ => {
                    let UploadIntent::Replace { expected_etag, .. } = &mut bad.intent else {
                        unreachable!()
                    };
                    *expected_etag = "wrong-revision".into();
                }
            }
            assert!(
                replacement_binding(&bad, &proof, row.id).is_err(),
                "fault {fault}"
            );
        }
        let mut bad_backup = serde_json::to_value(&row).unwrap();
        bad_backup["identity_handoff"]["backup"]["id"] =
            serde_json::json!("FILE::com.apple.CloudDocs::foreign");
        assert!(
            replacement_binding(&serde_json::from_value(bad_backup).unwrap(), &proof, row.id)
                .is_err()
        );
    }
    #[test]
    fn native_replacement_live_source_manifest_has_no_path_or_run_authority() {
        let dir = Path::new(DIRECTORY);
        let run = Uuid::parse_str(RUN).unwrap();
        for fault in 0..6 {
            let mut source = Source {
                run,
                archive: dir.join("replacement-source.zip"),
                archive_sha256: ARCHIVE_SHA.into(),
                expected_root: ROOT.into(),
                replacement_submitted: false,
            };
            match fault {
                0 => {
                    source_binding(&source, dir, run).unwrap();
                    continue;
                }
                1 => source.run = Uuid::new_v4(),
                2 => source.archive = PathBuf::from("/var/tmp/foreign.zip"),
                3 => source.archive_sha256 = "e".repeat(64),
                4 => source.expected_root = "Foreign.pages".into(),
                _ => source.replacement_submitted = true,
            }
            assert!(source_binding(&source, dir, run).is_err());
        }
    }
    #[tokio::test]
    async fn native_replacement_live_wrong_run_stops_before_io() {
        assert_eq!(
            icloud_public_native_replacement_preflight(Uuid::nil())
                .await
                .unwrap_err()
                .to_string(),
            "replacement run is not preregistered"
        );
        assert_eq!(
            icloud_public_native_replacement_verify(Uuid::nil(), Uuid::new_v4())
                .await
                .unwrap_err()
                .to_string(),
            "replacement run is not preregistered"
        );
    }
    #[test]
    fn native_replacement_live_semantic_input_refuses_symlink_and_changed_bytes() {
        let root =
            tempfile::tempdir_in(std::env::var_os("TMPDIR").unwrap_or_else(|| "/var/tmp".into()))
                .unwrap();
        let archive = root.path().join("replacement.zip");
        let bytes = crate::native_import::synthetic_package_archive(
            "Replacement.pages/data",
            b"owned synthetic version B",
        );
        std::fs::write(&archive, &bytes).unwrap();
        std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o400)).unwrap();
        let receipt = PackageDownload {
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
        };
        semantic_file(&archive, &receipt, ROOT).unwrap();
        assert!(semantic_file(&archive, &receipt, "Wrong.pages").is_err());
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&archive, &link).unwrap();
        assert!(semantic_file(&link, &receipt, ROOT).is_err());
        let bad = PackageDownload {
            size: receipt.size,
            sha256: "f".repeat(64),
        };
        assert!(semantic_file(&archive, &bad, ROOT).is_err());
        std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(semantic_file(&archive, &receipt, ROOT).is_err());
    }
}

mod diagnostic;
pub use diagnostic::icloud_public_native_replacement_diagnose;

pub(super) mod v2;
pub use v2::{icloud_native_v2_baseline, icloud_native_v2_preflight, icloud_native_v2_verify};
