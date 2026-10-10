//! Independent read-only verification of one caller-bound native removal.
//! Registration, durable operation authority and the finite window belong to the driver.
use crate::accounts::Account;
use anyhow::{Context, Result, ensure};
use cirrove_auth::{AppRegistration, CredentialVault};
use cirrove_core::{
    CancellationToken, Node,
    mutation::{MutationIntent, MutationRequest},
    upload::PackageSemanticIdentity,
};
use cirrove_icloud::{
    DriveEntry, ICloudReadSession, OwnedPackageTrashRequest, PackageDownload, ROOT_ID,
    SealedSessionVault,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata, OpenOptions},
    io::Write,
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

const LIMIT: u64 = 64 * 1024 * 1024;
type Stamp = (u64, u64, u64, u32, u32, u64, i64, i64, i64, i64);

fn stamp(meta: &Metadata) -> Stamp {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mode(),
        meta.uid(),
        meta.nlink(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

fn sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn binding<'a>(
    account: &Account,
    request: &'a MutationRequest,
    semantic: &PackageSemanticIdentity,
    source_size: u64,
    source_sha256: &str,
) -> Result<&'a Node> {
    request.validate()?;
    semantic.validate()?;
    ensure!(
        matches!(account.registration, AppRegistration::ICloud)
            && account.enabled
            && !account.identity.username.trim().is_empty()
            && account.drive.id == "drive"
            && account.drive.drive_type == "icloud_drive"
            && account.root_id == ROOT_ID
            && !uuid::Uuid::parse_str(&account.id)?.is_nil()
            && !uuid::Uuid::parse_str(&account.credential_id)?.is_nil()
            && request.scope.account == account.id
            && request.scope.provider == "icloud"
            && request.scope.collection == account.drive.id,
        "native Trash observer account or scope mismatch"
    );
    let MutationIntent::TrashNativeDocument { before } = &request.intent else {
        anyhow::bail!("native Trash observer requires original native removal");
    };
    let document = before
        .id
        .strip_prefix("FILE::com.apple.CloudDocs::")
        .context("native Trash observer original identity invalid")?;
    ensure!(
        !uuid::Uuid::parse_str(document)?.is_nil()
            && before
                .parent_id
                .as_deref()
                .is_some_and(|id| id.starts_with("FOLDER::com.apple.CloudDocs::"))
            && before.name.ends_with(".numbers")
            && before
                .content_version
                .as_ref()
                .is_none_or(|v| Some(v) == before.etag.as_ref())
            && semantic.version == 2
            && semantic.files > 0
            && source_size > 0
            && source_size <= LIMIT
            && sha(source_sha256),
        "native Trash observer source or original metadata mismatch"
    );
    Ok(before)
}

fn private_directory(path: &Path) -> Result<()> {
    let meta = open_read(path, true)?.metadata()?;
    ensure!(
        path.is_absolute()
            && meta.is_dir()
            && meta.mode() & 0o7777 == 0o700
            && meta.uid() == std::fs::metadata("/proc/self")?.uid(),
        "native Trash observer private directory changed"
    );
    Ok(())
}

fn open_read(path: &Path, directory: bool) -> Result<File> {
    ensure!(
        path.is_absolute(),
        "native Trash observer path must be absolute"
    );
    let parts = path
        .components()
        .skip(1)
        .map(|part| match part {
            std::path::Component::Normal(part) => Ok(part),
            _ => anyhow::bail!("native Trash observer path refused"),
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!parts.is_empty(), "native Trash observer path missing");
    let mut file = File::open("/")?;
    for (index, part) in parts.iter().enumerate() {
        use rustix::fs::{Mode, OFlags};
        let flags = OFlags::RDONLY
            | OFlags::CLOEXEC
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | if index + 1 < parts.len() || directory {
                OFlags::DIRECTORY
            } else {
                OFlags::empty()
            };
        file = File::from(rustix::fs::openat(&file, *part, flags, Mode::empty())?);
    }
    Ok(file)
}

fn source_file(path: &Path, size: u64) -> Result<File> {
    private_directory(
        path.parent()
            .context("native Trash source parent missing")?,
    )?;
    let file = open_read(path, false)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.mode() & 0o7777 == 0o400
            && meta.nlink() == 1
            && meta.uid() == std::fs::metadata("/proc/self")?.uid()
            && meta.len() == size
            && size > 0
            && size <= LIMIT
            && stamp(&meta) == stamp(&std::fs::symlink_metadata(path)?),
        "native Trash observer source file changed"
    );
    Ok(file)
}

fn raw_sha(file: &File, size: u64) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0;
    while offset < size {
        let want = usize::try_from((size - offset).min(buffer.len() as u64))?;
        let count = file.read_at(&mut buffer[..want], offset)?;
        ensure!(count > 0, "native Trash observer source truncated");
        digest.update(&buffer[..count]);
        offset += count as u64;
    }
    ensure!(
        file.read_at(&mut buffer[..1], size)? == 0,
        "native Trash observer source grew"
    );
    Ok(hex::encode(digest.finalize()))
}

fn source_unchanged(
    path: &Path,
    file: &File,
    original: Stamp,
    size: u64,
    digest: &str,
) -> Result<()> {
    let current = source_file(path, size)?;
    ensure!(
        stamp(&file.metadata()?) == original
            && stamp(&current.metadata()?) == original
            && raw_sha(file, size)? == digest
            && stamp(&file.metadata()?) == original,
        "native Trash observer source preservation failed"
    );
    Ok(())
}

fn parent_binding(roots: &[DriveEntry], original: &Node) -> Result<DriveEntry> {
    let parent_id = original
        .parent_id
        .as_deref()
        .context("native Trash original parent absent")?;
    let mut matching = roots.iter().filter(|entry| entry.drivewsid == parent_id);
    let parent = matching
        .next()
        .context("native Trash original parent not visible")?;
    ensure!(
        matching.next().is_none()
            && parent.kind == "FOLDER"
            && parent.zone == "com.apple.CloudDocs"
            && parent.parent_id == ROOT_ID
            && !parent.etag.is_empty()
            && roots
                .iter()
                .filter(|entry| entry.display_name() == parent.display_name())
                .count()
                == 1,
        "native Trash original parent ambiguous or changed"
    );
    // Child counters are not the parent's identity/revision envelope.
    let mut parent = parent.clone();
    parent.items.clear();
    parent.number_of_items = None;
    Ok(parent)
}

fn absent(entries: &[DriveEntry], original: &Node) -> Result<()> {
    ensure!(
        entries.iter().all(|entry| entry.drivewsid != original.id),
        "native Trash original still active"
    );
    Ok(())
}

fn json_sha<T: serde::Serialize>(value: &T) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

// Keep the explicitly registered caller's source/receipt fields separate.
#[allow(clippy::too_many_arguments)]
pub(super) async fn verify_removed(
    state: &Path,
    account: &Account,
    request: &MutationRequest,
    semantic: &PackageSemanticIdentity,
    original_raw_source: &Path,
    source_size: u64,
    source_sha256: &str,
    directory: &Path,
) -> Result<()> {
    let original = binding(account, request, semantic, source_size, source_sha256)?;
    private_directory(directory)?;
    let held_directory = open_read(directory, true)?;
    let directory_before = held_directory.metadata()?;
    ensure!(
        !directory
            .join("native-trash-independent-proof.json")
            .try_exists()?,
        "native Trash observer proof already exists"
    );
    let file = source_file(original_raw_source, source_size)?;
    let initial_stamp = stamp(&file.metadata()?);
    ensure!(
        raw_sha(&file, source_size)? == source_sha256,
        "native Trash observer source raw digest mismatch"
    );
    let cancel = CancellationToken::new();
    let archive = PackageDownload {
        size: source_size,
        sha256: source_sha256.to_owned(),
    };
    // Explicit flat Numbers scanner: no inferred or fabricated ZIP wrapper.
    ensure!(
        cirrove_icloud::package_flat_archive_semantic_identity_v2(&file, &archive, &cancel)?
            == *semantic,
        "native Trash observer flat source semantics mismatch"
    );
    source_unchanged(
        original_raw_source,
        &file,
        initial_stamp,
        source_size,
        source_sha256,
    )?;

    let sealed = SealedSessionVault::new(state, &account.id)?
        .load(&account.credential_id)
        .await?
        .context("native Trash observer sealed session unavailable")?;
    let mut session =
        ICloudReadSession::from_session_snapshot(&sealed, &account.identity.username)?;
    let parent_before = parent_binding(&session.list_root().await?, original)?;
    let parent_id = original
        .parent_id
        .as_deref()
        .context("native Trash original parent absent")?;
    let children_before = session.list_folder(parent_id).await?;
    absent(&children_before, original)?;
    let staging = tempfile::tempfile_in(directory)?;
    staging.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let verified = session
        .verify_owned_package_in_trash(
            OwnedPackageTrashRequest {
                apple_account: account.identity.username.clone(),
                drive_id: original.id.clone(),
                document_id: original
                    .id
                    .strip_prefix("FILE::com.apple.CloudDocs::")
                    .context("native Trash original identity invalid")?
                    .to_owned(),
                expected_root: original.name.clone(),
                semantic: semantic.clone(),
            },
            staging,
            &cancel,
        )
        .await?;
    let children_after = session.list_folder(parent_id).await?;
    absent(&children_after, original)?;
    let parent_after = parent_binding(&session.list_root().await?, original)?;
    ensure!(
        parent_before == parent_after
            && verified.semantic == *semantic
            && verified.archive.size > 0
            && verified.archive.size <= LIMIT
            && sha(&verified.archive.sha256),
        "native Trash observer final binding changed"
    );
    source_unchanged(
        original_raw_source,
        &file,
        initial_stamp,
        source_size,
        source_sha256,
    )?;
    private_directory(directory)?;
    let directory_after = open_read(directory, true)?.metadata()?;
    ensure!(
        directory_after.dev() == directory_before.dev()
            && directory_after.ino() == directory_before.ino(),
        "native Trash observer output directory replaced"
    );
    let proof = serde_json::json!({
        "version":1, "account":account.id, "collection":request.scope.collection,
        "original_item":original.id, "original_node_sha256":json_sha(original)?,
        "source_size":source_size, "source_sha256":source_sha256,
        "source_layout":"flat_numbers", "semantic":verified.semantic,
        "trash_archive_size":verified.archive.size,
        "trash_archive_sha256":verified.archive.sha256,
        "trash_etag_sha256":json_sha(&verified.trash_etag)?,
        "parent_before_sha256":json_sha(&parent_before)?,
        "parent_after_sha256":json_sha(&parent_after)?,
        "children_before_sha256":json_sha(&children_before)?,
        "children_after_sha256":json_sha(&children_after)?,
        "original_id_absent_before_after":true, "source_preserved":true,
        "independent_trash_full_v2":true, "trash_metadata_fenced_by_adapter":true,
        "durable_operation_authority_verified_here":false,
        "downloaded_archive_retained":false, "full_trash_node_returned":false,
        "cloud_mutation":false, "restore_authorized":false
    });
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.join("native-trash-independent-proof.json"))?;
    output.write_all(&serde_json::to_vec_pretty(&proof)?)?;
    output.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests;
