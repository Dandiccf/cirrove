//! Exact new owned arm. No enqueue, retry, handoff or provider mutation exists here.
use super::super::public_verify::retained_public_import_after_abandonment;
use super::*;
use crate::journal::UploadState;
use anyhow::bail;
const NEW_RUN: &str = "99aa5d99-abf8-45bf-85fd-366e6ec89365";
const NEW_DIR: &str =
    "/var/tmp/cirrove-native-abandon-v2-live-99aa5d99-abf8-45bf-85fd-366e6ec89365";
const OLD_OPERATION: &str = "82767008-4553-4f8b-99cc-8a521ad9c5ac";
const IMPORT_OPERATION: &str = "b534c269-8b9e-47e0-97a0-a512d0127765";
const STAGE: &str = "FILE::com.apple.CloudDocs::086B85BF-007F-427E-88A6-78333B18DC8A";
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    run: Uuid,
    account: String,
    old_operation: Uuid,
    conflict_sha256: String,
    resolved_sha256: String,
    checkpoint_cipher_sha256: String,
    payload_sha256: String,
    payload_size: u64,
    old_preflight_sha256: String,
}
fn run_directory(run: Uuid) -> Result<&'static Path> {
    ensure!(
        run.to_string() == NEW_RUN,
        "not the preregistered new v2 arm"
    );
    Ok(Path::new(NEW_DIR))
}
fn bound(run: Uuid) -> Result<&'static Path> {
    let dir = run_directory(run)?;
    owned_directory(dir)?;
    Ok(dir)
}
fn missing(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => bail!("owned arm artifact already exists or is unavailable; replay refused"),
    }
}
fn digest(value: &impl serde::Serialize) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}
fn file_hash(path: &Path, max: u64) -> Result<(u64, String)> {
    let mut file = private_file(path, max)?;
    ensure!(
        file.metadata()?.uid() == std::fs::metadata("/proc/self")?.uid(),
        "evidence owner changed"
    );
    let length = file.metadata()?.len();
    Ok((length, sha256(&mut file)?))
}
fn sealed_path() -> PathBuf {
    Path::new(PUBLIC)
        .join("state/accounts")
        .join(ACCOUNT)
        .join("upload-checkpoints")
        .join(OLD_OPERATION)
        .join("checkpoint.sealed")
}
fn payload_path() -> PathBuf {
    Path::new(PUBLIC)
        .join("state/accounts")
        .join(ACCOUNT)
        .join("journal/objects")
        .join(OLD_OPERATION)
}
fn old_proof() -> Result<Preflight> {
    read_json(&Path::new(DIRECTORY).join("preflight.json"), 64 * 1024)
}
fn old_row_shape(row: &UploadRecord, state: UploadState) -> Result<()> {
    let UploadRepresentation::PackageReplacementArchive {
        original,
        semantic,
        original_semantic,
        ..
    } = &row.representation
    else {
        bail!("old representation changed");
    };
    ensure!(
        row.id.to_string() == OLD_OPERATION
            && row.scope.account == ACCOUNT
            && row.scope.provider == "icloud"
            && row.scope.collection == "drive"
            && row.state == state
            && row.attempt.is_none()
            && row.remote.is_none()
            && row.package_completion.is_none()
            && row.base.is_none()
            && row.working_file.is_none()
            && row.session_key == Some(row.id)
            && row.sha256 == ARCHIVE_SHA
            && semantic.version == 1
            && original_semantic.version == 1
            && original.id == ORIGINAL,
        "old operation is not retained exact v1 Stage"
    );
    Ok(())
}
pub(in crate::validation::icloud_account::owned_package) fn exact_inventory(
    rows: &[UploadRecord],
    replacement: Option<Uuid>,
) -> Result<()> {
    ensure!(
        replacement.is_none_or(|id| !id.is_nil()
            && id.to_string() != OLD_OPERATION
            && id.to_string() != IMPORT_OPERATION),
        "new operation reuses retained identity"
    );
    ensure!(
        rows.len() == if replacement.is_some() { 3 } else { 2 },
        "unexpected retained operation count"
    );
    let mut ids = std::collections::HashSet::new();
    for row in rows {
        ensure!(
            ids.insert(row.id)
                && (row.id.to_string() == IMPORT_OPERATION
                    || row.id.to_string() == OLD_OPERATION
                    || Some(row.id) == replacement),
            "unrelated or duplicate operation refused"
        );
    }
    ensure!(
        rows.iter().any(|r| r.id.to_string() == IMPORT_OPERATION)
            && rows.iter().any(|r| r.id.to_string() == OLD_OPERATION)
            && replacement.is_none_or(|id| rows.iter().any(|r| r.id == id)),
        "required operation missing"
    );
    let old = rows
        .iter()
        .find(|r| r.id.to_string() == OLD_OPERATION)
        .context("old operation missing")?;
    old_row_shape(old, UploadState::Resolved)
}
fn baseline_binding(b: &Baseline, run: Uuid) -> Result<()> {
    ensure!(
        b.run == run
            && run.to_string() == NEW_RUN
            && b.account == ACCOUNT
            && b.old_operation.to_string() == OLD_OPERATION
            && b.payload_sha256 == ARCHIVE_SHA
            && b.payload_size > 0
            && b.payload_size <= LIMIT,
        "baseline identity changed"
    );
    for hash in [
        &b.conflict_sha256,
        &b.resolved_sha256,
        &b.checkpoint_cipher_sha256,
        &b.old_preflight_sha256,
    ] {
        ensure!(
            hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid baseline digest"
        );
    }
    Ok(())
}
fn preserved(
    binding: &PublicImportBinding,
    dir: &Path,
    run: Uuid,
) -> Result<cirrove_icloud::NativeReplacementAbandonRecord> {
    let b: Baseline = read_json(&dir.join("baseline.json"), 16 * 1024)?;
    baseline_binding(&b, run)?;
    let old = binding.journal.native_validation_upload(b.old_operation)?;
    old_row_shape(&old, UploadState::Resolved)?;
    ensure!(
        digest(&old)? == b.resolved_sha256,
        "old row changed beyond explicit local resolution"
    );
    ensure!(
        file_hash(&payload_path(), LIMIT)? == (b.payload_size, b.payload_sha256.clone()),
        "old retained bytes changed"
    );
    ensure!(
        file_hash(&sealed_path(), 4 * 1024 * 1024)?.1 == b.checkpoint_cipher_sha256,
        "old encrypted continuation changed"
    );
    ensure!(
        file_hash(&Path::new(DIRECTORY).join("preflight.json"), 64 * 1024)?.1
            == b.old_preflight_sha256,
        "old preflight changed"
    );
    let receipt = binding
        .journal
        .native_stage_abandonment(b.old_operation)?
        .context("explicit abandonment receipt missing")?;
    receipt.validate()?;
    ensure!(
        receipt.operation == old.id
            && receipt.request.scope == old.scope
            && receipt.request.intent == old.intent
            && receipt.request.representation == old.representation
            && receipt.request.size == old.size
            && receipt.request.sha256 == old.sha256
            && receipt.original.id == ORIGINAL
            && receipt.staged.id == STAGE,
        "abandonment receipt does not bind retained Stage"
    );
    Ok(receipt)
}
/// Local-only before abandonment. Refuses rerun; writes only new private evidence.
pub fn icloud_native_v2_baseline(run: Uuid) -> Result<()> {
    let dir = bound(run)?;
    missing(&dir.join("baseline.json"))?;
    missing(&dir.join("replacement-source.zip"))?;
    let binding = retained_public_import_for_replacement(
        Uuid::parse_str(IMPORT_RUN)?,
        Some(Uuid::parse_str(OLD_OPERATION)?),
    )?;
    original_binding(&binding)?;
    let old = binding
        .journal
        .native_validation_upload(Uuid::parse_str(OLD_OPERATION)?)?;
    old_row_shape(&old, UploadState::Conflict)?;
    let p = preflight_binding(&old_proof()?, &binding, Path::new(DIRECTORY))?;
    prepared_replacement_binding(&old, &p, old.id)?;
    let (size, hash) = file_hash(&payload_path(), LIMIT)?;
    ensure!(
        size == old.size && hash == old.sha256,
        "old payload no longer exact"
    );
    let mut resolved = old.clone();
    resolved.state = UploadState::Resolved;
    let b = Baseline {
        run,
        account: ACCOUNT.into(),
        old_operation: old.id,
        conflict_sha256: digest(&old)?,
        resolved_sha256: digest(&resolved)?,
        checkpoint_cipher_sha256: file_hash(&sealed_path(), 4 * 1024 * 1024)?.1,
        payload_sha256: hash,
        payload_size: size,
        old_preflight_sha256: file_hash(&Path::new(DIRECTORY).join("preflight.json"), 64 * 1024)?.1,
    };
    baseline_binding(&b, run)?;
    // Copy only the validated owned edited source; never checkpoint or credentials.
    owned_directory(
        p.archive
            .parent()
            .context("source snapshot parent missing")?,
    )?;
    let mut source = private_file(&p.archive, LIMIT)?;
    ensure!(
        source.metadata()?.len() == size && sha256(&mut source)? == ARCHIVE_SHA,
        "edited source changed"
    );
    std::io::Seek::rewind(&mut source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join("replacement-source.zip"))?;
    ensure!(
        std::io::copy(&mut source, &mut output)? == size,
        "source copy incomplete"
    );
    output.sync_all()?;
    output.set_permissions(std::fs::Permissions::from_mode(0o400))?;
    output.sync_all()?;
    record(&dir.join("baseline.json"), &b)?;
    File::open(dir)?.sync_all()?;
    println!("Exact old v1 operation baseline retained locally; no cloud call or retry.");
    Ok(())
}
async fn stage_present(
    remote: &mut ICloudReadSession,
    binding: &PublicImportBinding,
    record: &cirrove_icloud::NativeReplacementAbandonRecord,
) -> Result<()> {
    let entries = remote.list_folder(&binding.plan.parent).await?;
    exact_entry(&entries, &record.staged)?;
    Ok(())
}
fn fresh_proof(
    p: &Preflight,
    binding: &PublicImportBinding,
    dir: &Path,
    record: &cirrove_icloud::NativeReplacementAbandonRecord,
) -> Result<()> {
    ensure!(
        p.run.to_string() == NEW_RUN
            && p.account == ACCOUNT
            && p.scope == scope(&binding.account)
            && p.original == record.original
            && p.original.content_version.is_none()
            && p.original
                .etag
                .as_ref()
                .is_some_and(|e| !e.is_empty() && !e.contains('*'))
            && p.old_semantic.version == 2
            && p.new_semantic.version == 2
            && p.new_semantic != p.old_semantic
            && p.archive_sha256 == ARCHIVE_SHA
            && p.archive_size > 0
            && p.archive_size <= LIMIT
            && p.expected_root == ROOT
            && p.archive
                .file_name()
                .is_some_and(|n| n == "replacement.zip")
            && p.archive.parent().and_then(Path::parent) == Some(dir)
            && p.visible_path == format!("{}/{}", binding.plan.parent_name, p.original.name),
        "fresh v2 proof changed"
    );
    p.old_semantic.validate()?;
    p.new_semantic.validate()?;
    Ok(())
}
pub async fn icloud_native_v2_preflight(run: Uuid) -> Result<()> {
    let dir = bound(run)?;
    missing(&dir.join("preflight.json"))?;
    let binding = retained_public_import_after_abandonment(None)?;
    original_binding(&binding)?;
    let abandoned = preserved(&binding, dir, run)?;
    let attempt = verification_directory(dir)?;
    manifest(&attempt, run, "new-v2-preflight-read-only")?;
    let source = dir.join("replacement-source.zip");
    let stage = attempt.clone();
    let archive = attempt.join("replacement.zip");
    let output = archive.clone();
    let (new_semantic, archive_size, archive_sha256) =
        tokio::task::spawn_blocking(move || -> Result<_> {
            let capture = crate::native_import::ValidatedPackageArchive::capture(
                &source,
                &stage,
                ROOT,
                &CancellationToken::new(),
            )?;
            let (mut f, representation, size, hash) = capture.into_parts();
            let UploadRepresentation::PackageArchive {
                expected_root,
                semantic,
            } = representation
            else {
                bail!("native archive required")
            };
            ensure!(
                expected_root == ROOT && semantic.version == 2 && hash == ARCHIVE_SHA,
                "fresh v2 capture policy or source changed"
            );
            std::io::Seek::rewind(&mut f)?;
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(output)?;
            ensure!(
                std::io::copy(&mut f, &mut out)? == size,
                "fresh source copy incomplete"
            );
            out.sync_all()?;
            out.set_permissions(std::fs::Permissions::from_mode(0o400))?;
            out.sync_all()?;
            Ok((semantic, size, hash))
        })
        .await??;
    let expected_original = binding.semantic_v2()?;
    let mut remote = session(Path::new(PUBLIC), &binding.account).await?;
    active_parent(&remote, &binding).await?;
    stage_present(&mut remote, &binding, &abandoned).await?;
    let original = abandoned.original.clone();
    let entry = exact_entry(&remote.list_folder(&binding.plan.parent).await?, &original)?;
    let download_receipt = download(
        &mut remote,
        &binding.plan.parent,
        &entry,
        &attempt.join("original.zip"),
    )
    .await?;
    let old_semantic = semantic_file_versioned(
        &attempt.join("original.zip"),
        &download_receipt,
        &original.name,
        2,
    )?;
    ensure!(
        old_semantic == expected_original && old_semantic != new_semantic,
        "fresh original semantics changed or no actual edit"
    );
    ensure!(
        exact_entry(&remote.list_folder(&binding.plan.parent).await?, &original)? == entry,
        "original changed during fresh selection"
    );
    stage_present(&mut remote, &binding, &abandoned).await?;
    active_parent(&remote, &binding).await?;
    preserved(&binding, dir, run)?;
    let p = Preflight {
        run,
        account: ACCOUNT.into(),
        scope: scope(&binding.account),
        original,
        old_semantic,
        new_semantic,
        archive,
        archive_size,
        archive_sha256,
        expected_root: ROOT.into(),
        visible_path: format!("{}/{}", binding.plan.parent_name, abandoned.original.name),
    };
    fresh_proof(&p, &binding, dir, &abandoned)?;
    record(&dir.join("preflight.json"), &p)?;
    File::open(dir)?.sync_all()?;
    println!("Fresh v2 exact original revision selected read-only; no replacement submitted.");
    Ok(())
}
pub async fn icloud_native_v2_verify(run: Uuid, operation: Uuid) -> Result<()> {
    let dir = bound(run)?;
    let binding = retained_public_import_after_abandonment(Some(operation))?;
    original_binding(&binding)?;
    let abandoned = preserved(&binding, dir, run)?;
    let p: Preflight = read_json(&dir.join("preflight.json"), 64 * 1024)?;
    fresh_proof(&p, &binding, dir, &abandoned)?;
    ensure!(
        semantic_file_versioned(
            &p.archive,
            &PackageDownload {
                size: p.archive_size,
                sha256: p.archive_sha256.clone()
            },
            ROOT,
            2
        )? == p.new_semantic,
        "fresh edited archive changed"
    );
    let row = binding.journal.native_validation_upload(operation)?;
    let (current, backup) = replacement_binding(&row, &p, operation)?;
    publication_binding(
        &binding
            .journal
            .native_validation_package_publication(operation)?,
        current,
    )?;
    let attempt = verification_directory(dir)?;
    manifest(&attempt, run, "new-v2-postflight-read-only")?;
    let mut remote = session(Path::new(PUBLIC), &binding.account).await?;
    active_parent(&remote, &binding).await?;
    stage_present(&mut remote, &binding, &abandoned).await?;
    let entry = exact_entry(&remote.list_folder(&binding.plan.parent).await?, current)?;
    let receipt = download(
        &mut remote,
        &binding.plan.parent,
        &entry,
        &attempt.join("current.zip"),
    )
    .await?;
    ensure!(
        semantic_file_versioned(&attempt.join("current.zip"), &receipt, &current.name, 2)?
            == p.new_semantic,
        "new current semantic content differs"
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
                expected_root: p.original.name.clone(),
                semantic: p.old_semantic.clone(),
            },
            staging,
            &CancellationToken::new(),
        )
        .await?;
    ensure!(
        backup.etag.as_ref() == Some(&recovered.trash_etag) && recovered.semantic == p.old_semantic,
        "original Trash recovery changed"
    );
    ensure!(
        exact_entry(&remote.list_folder(&binding.plan.parent).await?, current)? == entry,
        "current revision changed during verification"
    );
    stage_present(&mut remote, &binding, &abandoned).await?;
    active_parent(&remote, &binding).await?;
    preserved(&binding, dir, run)?;
    record(
        &attempt.join("verified.json"),
        &serde_json::json!({"run":run,"operation":operation,"old_operation":OLD_OPERATION,"account":ACCOUNT,"current":current,"backup":backup,"old_stage_retained":true,"old_v1_record_and_payload_preserved":true,"new_semantic":p.new_semantic,"original_semantic":p.old_semantic,"cloud_mutated":false,"mounted_visibility_verified":false,"apple_pages_verified":false}),
    )?;
    File::open(&attempt)?.sync_all()?;
    println!(
        "New v2 replacement and original recovery verified read-only; abandoned v1 stage remains retained."
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    fn old() -> UploadRecord {
        let (_, mut row) = super::super::tests::fixture();
        row.id = Uuid::parse_str(OLD_OPERATION).unwrap();
        row.state = UploadState::Resolved;
        row.remote = None;
        row.package_completion = None;
        row.session_key = Some(row.id);
        row
    }
    #[test]
    fn v2_acceptance_inventory_refuses_unrelated_duplicate_missing_and_replayed_operations() {
        let old = old();
        let mut imported = old.clone();
        imported.id = Uuid::parse_str(IMPORT_OPERATION).unwrap();
        exact_inventory(&[old.clone(), imported.clone()], None).unwrap();
        let mut fresh = old.clone();
        fresh.id = Uuid::new_v4();
        exact_inventory(
            &[old.clone(), imported.clone(), fresh.clone()],
            Some(fresh.id),
        )
        .unwrap();
        for bad in [
            vec![old.clone()],
            vec![old.clone(), old.clone()],
            vec![old.clone(), fresh.clone()],
            vec![old.clone(), imported.clone(), fresh.clone()],
        ] {
            assert!(exact_inventory(&bad, None).is_err());
        }
        assert!(exact_inventory(&[old.clone(), imported.clone()], Some(old.id)).is_err());
        assert!(exact_inventory(&[old.clone(), imported.clone()], Some(imported.id)).is_err());
        assert!(
            exact_inventory(
                &[old.clone(), imported.clone(), fresh],
                Some(Uuid::new_v4())
            )
            .is_err()
        );
        let mut changed = old;
        changed.state = UploadState::Conflict;
        assert!(exact_inventory(&[changed, imported], None).is_err());
    }
    #[test]
    fn v2_artifact_guard_refuses_replay_and_broken_symlink_before_any_account_access() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("preflight.json");
        missing(&path).unwrap();
        std::fs::write(&path, b"retained").unwrap();
        assert!(missing(&path).is_err());
        let link = temp.path().join("baseline.json");
        std::os::unix::fs::symlink(temp.path().join("missing-target"), &link).unwrap();
        assert!(missing(&link).is_err());
    }
    #[test]
    fn v2_baseline_rejects_old_run_and_preserves_every_old_row_field_except_terminal_state() {
        assert!(run_directory(Uuid::parse_str(RUN).unwrap()).is_err());
        assert_eq!(
            run_directory(Uuid::parse_str(NEW_RUN).unwrap()).unwrap(),
            Path::new(NEW_DIR)
        );
        let mut row = old();
        row.state = UploadState::Conflict;
        old_row_shape(&row, UploadState::Conflict).unwrap();
        let original = digest(&row).unwrap();
        row.state = UploadState::Resolved;
        let resolved = digest(&row).unwrap();
        assert_ne!(original, resolved);
        let mut changed = row.clone();
        changed.failed_attempts += 1;
        assert_ne!(digest(&changed).unwrap(), resolved);
        changed = row.clone();
        changed.sha256 = "f".repeat(64);
        assert_ne!(digest(&changed).unwrap(), resolved);
        assert!(old_row_shape(&changed, UploadState::Resolved).is_err());
        changed = row;
        changed.session_key = Some(Uuid::new_v4());
        assert!(old_row_shape(&changed, UploadState::Resolved).is_err());
        let baseline = Baseline {
            run: Uuid::parse_str(RUN).unwrap(),
            account: ACCOUNT.into(),
            old_operation: Uuid::parse_str(OLD_OPERATION).unwrap(),
            conflict_sha256: original,
            resolved_sha256: resolved,
            checkpoint_cipher_sha256: "a".repeat(64),
            payload_sha256: ARCHIVE_SHA.into(),
            payload_size: 123,
            old_preflight_sha256: "b".repeat(64),
        };
        assert!(baseline_binding(&baseline, Uuid::parse_str(NEW_RUN).unwrap()).is_err());
    }
}
