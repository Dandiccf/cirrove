//! A real FUSE create check, confined to a new Cirrove-owned iCloud folder.
//! Neither account settings nor the installed daemon are changed.
#[path = "cirrove-icloud-mounted-write-probe/fixture.rs"]
mod fixture;

use anyhow::{Context, Result, bail, ensure};
use cirrove_auth::{AccessMode, AppRegistration, CredentialVault};
use cirrove_core::mutation::MutationReceipt;
use cirrove_core::upload::UploadIntent;
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_icloud::{
    ICloudDrive, ICloudFileCreate, ICloudFolderCreate, ICloudReadSession, ROOT_ID,
    SealedFolderCheckpointVault, SealedSessionVault, SealedUploadCheckpointVault,
};
use cirrove_service::{
    accounts::Settings,
    engine::Engine,
    journal::{MutationState, UploadJournal, UploadState},
    private_dir,
    writable::WritableSession,
};
use fixture::{Fixture, RemovalContext, restored_owned};
use secrecy::ExposeSecret;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

const APP: &str = r#"
import os, sys
mount = sys.argv[1]
name = sys.argv[2]
path = os.path.join(mount, name)
fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
try:
    os.write(fd, b'Cirrove isolated mounted iCloud validation\n')
    os.fsync(fd)
finally:
    os.close(fd)
os.mkdir(os.path.join(mount, 'Mounted Folder'))
"#;

const APP_READ: &str = r#"
import os, sys
mount = sys.argv[1]
with open(os.path.join(mount, 'Mounted Create.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
assert os.path.isdir(os.path.join(mount, 'Mounted Folder'))
"#;

const APP_RENAME_FOLDER: &str = r#"
import os, sys
mount = sys.argv[1]
try:
    os.rename(os.path.join(mount, 'Mounted Folder'), os.path.join(mount, 'Mounted Renamed'))
except OSError as error:
    print(f'rename_errno={error.errno}', file=sys.stderr)
    sys.exit(1)
assert not os.path.exists(os.path.join(mount, 'Mounted Folder'))
assert os.path.isdir(os.path.join(mount, 'Mounted Renamed'))
with open(os.path.join(mount, 'Mounted Create.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
"#;

const APP_READ_RENAMED: &str = r#"
import os, sys
mount = sys.argv[1]
assert not os.path.exists(os.path.join(mount, 'Mounted Folder'))
assert os.path.isdir(os.path.join(mount, 'Mounted Renamed'))
with open(os.path.join(mount, 'Mounted Create.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
"#;

const APP_RENAME_FILE: &str = r#"
import os, sys
mount = sys.argv[1]
try:
    os.rename(os.path.join(mount, 'Mounted Create.txt'), os.path.join(mount, 'Mounted Renamed.txt'))
except OSError as error:
    print(f'rename_errno={error.errno}', file=sys.stderr)
    sys.exit(1)
assert not os.path.exists(os.path.join(mount, 'Mounted Create.txt'))
with open(os.path.join(mount, 'Mounted Renamed.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
assert os.path.isdir(os.path.join(mount, 'Mounted Folder'))
"#;

const APP_READ_RENAMED_FILE: &str = r#"
import os, sys
mount = sys.argv[1]
assert not os.path.exists(os.path.join(mount, 'Mounted Create.txt'))
with open(os.path.join(mount, 'Mounted Renamed.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
assert os.path.isdir(os.path.join(mount, 'Mounted Folder'))
"#;

const APP_RENAME_FILE_AGAIN: &str = r#"
import os, sys
mount = sys.argv[1]
os.rename(os.path.join(mount, 'Mounted Renamed.txt'), os.path.join(mount, 'Mounted Renamed Again.txt'))
assert not os.path.exists(os.path.join(mount, 'Mounted Renamed.txt'))
with open(os.path.join(mount, 'Mounted Renamed Again.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
assert os.path.isdir(os.path.join(mount, 'Mounted Folder'))
"#;

const APP_READ_RENAMED_FILE_AGAIN: &str = r#"
import os, sys
mount = sys.argv[1]
assert not os.path.exists(os.path.join(mount, 'Mounted Renamed.txt'))
with open(os.path.join(mount, 'Mounted Renamed Again.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
assert os.path.isdir(os.path.join(mount, 'Mounted Folder'))
"#;

const APP_MOVE_FILE: &str = r#"
import os, sys
mount = sys.argv[1]
name = 'Mounted Renamed Again.txt'
os.rename(os.path.join(mount, name), os.path.join(mount, 'Mounted Folder', name))
assert not os.path.exists(os.path.join(mount, name))
with open(os.path.join(mount, 'Mounted Folder', name), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
"#;

const APP_READ_MOVED_FILE: &str = r#"
import os, sys
mount = sys.argv[1]
name = 'Mounted Renamed Again.txt'
assert not os.path.exists(os.path.join(mount, name))
with open(os.path.join(mount, 'Mounted Folder', name), 'rb') as f:
    data = f.read()
expected = b'Cirrove isolated mounted iCloud validation\n'
if data != expected:
    import hashlib
    print(f'nested_read_size={len(data)} nested_read_sha256={hashlib.sha256(data).hexdigest()}', file=sys.stderr)
    sys.exit(1)
"#;

const APP_PREPARE_FOLDER_MOVE: &str = r#"
import os, sys
mount = sys.argv[1]
os.mkdir(os.path.join(mount, 'Mounted Move Destination'))
path = os.path.join(mount, 'Mounted Folder', 'Move Child.txt')
with open(path, 'xb', buffering=0) as file:
    file.write(b'Cirrove mounted folder move child\n')
    os.fsync(file.fileno())
"#;

const APP_MOVE_FOLDER: &str = r#"
import os, sys
mount = sys.argv[1]
source = os.path.join(mount, 'Mounted Folder')
destination = os.path.join(mount, 'Mounted Move Destination', 'Mounted Folder')
assert open(os.path.join(source, 'Move Child.txt'), 'rb').read() == b'Cirrove mounted folder move child\n'
os.rename(source, destination)
assert not os.path.exists(source)
assert open(os.path.join(destination, 'Move Child.txt'), 'rb').read() == b'Cirrove mounted folder move child\n'
"#;

const APP_READ_MOVED_FOLDER: &str = r#"
import os, sys
mount = sys.argv[1]
assert not os.path.exists(os.path.join(mount, 'Mounted Folder'))
path = os.path.join(mount, 'Mounted Move Destination', 'Mounted Folder', 'Move Child.txt')
assert open(path, 'rb').read() == b'Cirrove mounted folder move child\n'
"#;

const APP_RENAME_NESTED_FILE: &str = r#"
import os, sys
mount = sys.argv[1]
folder = os.path.join(mount, 'Mounted Folder')
os.rename(os.path.join(folder, 'Mounted Renamed Again.txt'), os.path.join(folder, 'Nested Renamed.txt'))
assert not os.path.exists(os.path.join(folder, 'Mounted Renamed Again.txt'))
with open(os.path.join(folder, 'Nested Renamed.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
"#;

const APP_READ_NESTED_RENAMED_FILE: &str = r#"
import os, sys
mount = sys.argv[1]
folder = os.path.join(mount, 'Mounted Folder')
assert not os.path.exists(os.path.join(folder, 'Mounted Renamed Again.txt'))
with open(os.path.join(folder, 'Nested Renamed.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
"#;

const APP_CREATE_NESTED_FILE: &str = r#"
import os, sys
mount = sys.argv[1]
folder = os.path.join(mount, 'Mounted Folder')
with open(os.path.join(folder, 'Nested Renamed.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
path = os.path.join(folder, 'Nested Created.txt')
fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
try:
    os.write(fd, b'Cirrove nested iCloud create validation\n')
    os.fsync(fd)
finally:
    os.close(fd)
"#;

const APP_READ_NESTED_CREATED_FILE: &str = r#"
import os, sys
mount = sys.argv[1]
folder = os.path.join(mount, 'Mounted Folder')
with open(os.path.join(folder, 'Nested Renamed.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
with open(os.path.join(folder, 'Nested Created.txt'), 'rb') as f:
    assert f.read() == b'Cirrove nested iCloud create validation\n'
"#;

const APP_REMOVE_NESTED_FILE: &str = r#"
import os, sys
mount = sys.argv[1]
folder = os.path.join(mount, 'Mounted Folder')
os.unlink(os.path.join(folder, 'Nested Created.txt'))
assert not os.path.exists(os.path.join(folder, 'Nested Created.txt'))
with open(os.path.join(folder, 'Nested Renamed.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
"#;

const APP_READ_NESTED_REMOVED: &str = r#"
import os, sys
mount = sys.argv[1]
folder = os.path.join(mount, 'Mounted Folder')
assert not os.path.exists(os.path.join(folder, 'Nested Created.txt'))
with open(os.path.join(folder, 'Nested Renamed.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
"#;

const APP_REMOVE_FOLDER: &str = r#"
import os, sys
mount = sys.argv[1]
try:
    os.rmdir(os.path.join(mount, 'Mounted Folder'))
except OSError as error:
    print(f'rmdir_errno={error.errno}', file=sys.stderr)
    sys.exit(1)
with open(os.path.join(mount, 'Mounted Create.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
"#;

const APP_READ_REMOVED: &str = r#"
import os, sys
mount = sys.argv[1]
with open(os.path.join(mount, 'Mounted Create.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted iCloud validation\n'
assert not os.path.exists(os.path.join(mount, 'Mounted Folder'))
"#;

const APP_REMOVE_FILE: &str = r#"
import os, sys
mount = sys.argv[1]
assert not os.path.exists(os.path.join(mount, 'Mounted Folder'))
try:
    os.unlink(os.path.join(mount, 'Mounted Create.txt'))
except OSError as error:
    print(f'unlink_errno={error.errno}', file=sys.stderr)
    sys.exit(1)
"#;

const APP_READ_ALL_REMOVED: &str = r#"
import os, sys
mount = sys.argv[1]
assert not os.path.exists(os.path.join(mount, 'Mounted Folder'))
assert not os.path.exists(os.path.join(mount, 'Mounted Create.txt'))
"#;

const APP_REPLACE: &str = r#"
import os, sys
path = os.path.join(sys.argv[1], 'Mounted Create.txt')
fd = os.open(path, os.O_WRONLY | os.O_TRUNC)
try:
    assert os.write(fd, b'Cirrove isolated mounted replacement\n') == 37
    os.fsync(fd)
finally:
    os.close(fd)
"#;

const APP_READ_REPLACED: &str = r#"
import os, sys
mount = sys.argv[1]
with open(os.path.join(mount, 'Mounted Create.txt'), 'rb') as f:
    assert f.read() == b'Cirrove isolated mounted replacement\n'
assert os.path.isdir(os.path.join(mount, 'Mounted Folder'))
"#;

const APP_CREATE_NESTED_REPLACE_SOURCE: &str = r#"
import os, sys
path = os.path.join(sys.argv[1], 'Mounted Folder', 'Nested Replacement.txt')
fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
try:
    assert os.write(fd, b'Cirrove nested replacement source\n') == 34
    os.fsync(fd)
finally:
    os.close(fd)
"#;

const APP_REPLACE_NESTED_SOURCE: &str = r#"
import os, sys
path = os.path.join(sys.argv[1], 'Mounted Folder', 'Nested Replacement.txt')
fd = os.open(path, os.O_WRONLY | os.O_TRUNC)
try:
    assert os.write(fd, b'Cirrove nested replacement result\n') == 34
    os.fsync(fd)
finally:
    os.close(fd)
"#;

const APP_READ_NESTED_REPLACEMENT: &str = r#"
import os, sys
path = os.path.join(sys.argv[1], 'Mounted Folder', 'Nested Replacement.txt')
with open(path, 'rb') as file:
    assert file.read() == b'Cirrove nested replacement result\n'
"#;

const APP_LARGE_CREATE: &str = r#"
import os, sys
mount = sys.argv[1]
path = os.path.join(mount, 'Mounted Create.txt')
piece = bytes(range(256)) * 256
fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
try:
    for _ in range(80):
        view = memoryview(piece)
        while view:
            view = view[os.write(fd, view):]
    os.fsync(fd)
finally:
    os.close(fd)
os.mkdir(os.path.join(mount, 'Mounted Folder'))
"#;

const APP_LARGE_REPLACE: &str = r#"
import os, sys
path = os.path.join(sys.argv[1], 'Mounted Create.txt')
piece = bytes(reversed(range(256))) * 256
fd = os.open(path, os.O_WRONLY | os.O_TRUNC)
try:
    for _ in range(96):
        view = memoryview(piece)
        while view:
            view = view[os.write(fd, view):]
    os.fsync(fd)
finally:
    os.close(fd)
"#;

const APP_LARGE_READ_REPLACED: &str = r#"
import os, sys
mount = sys.argv[1]
piece = bytes(reversed(range(256))) * 256
with open(os.path.join(mount, 'Mounted Create.txt'), 'rb') as file:
    for _ in range(96):
        assert file.read(len(piece)) == piece
    assert file.read(1) == b''
assert os.path.isdir(os.path.join(mount, 'Mounted Folder'))
"#;

async fn verify_large_bytes(
    session: &mut ICloudReadSession,
    folder_id: &str,
    file_id: &str,
    etag: &str,
    size: u64,
    replaced: bool,
) -> Result<(String, u64)> {
    let count = if replaced { 96 } else { 80 };
    let piece: Vec<u8> = if replaced {
        (0..=255).rev().cycle().take(64 * 1024).collect()
    } else {
        (0..=255).cycle().take(64 * 1024).collect()
    };
    let expected_size = count * piece.len() as u64;
    ensure!(
        size == expected_size,
        "independent iCloud file size differs"
    );
    let mut expected_hash = Sha256::new();
    for _ in 0..count {
        expected_hash.update(&piece);
    }
    let mut actual_hash = Sha256::new();
    let mut offset = 0;
    while offset < size {
        let length = (size - offset).min(4 * 1024 * 1024) as u32;
        let bytes = session
            .read_range_in_folder(folder_id, file_id, offset, length)
            .await?;
        ensure!(
            bytes.len() == length as usize,
            "iCloud range was incomplete"
        );
        actual_hash.update(&bytes);
        offset += u64::from(length);
    }
    let actual_hash = hex::encode(actual_hash.finalize());
    ensure!(
        actual_hash == hex::encode(expected_hash.finalize()),
        "remote file bytes differ from mounted write"
    );
    let streamed_hash = session
        .hash_file_in_folder_for_revision(folder_id, file_id, etag, size)
        .await?;
    ensure!(
        streamed_hash == actual_hash,
        "streamed iCloud verification differs from independent ranges"
    );
    let after = session.list_folder(folder_id).await?;
    ensure!(
        after
            .iter()
            .filter(|entry| entry.drivewsid == file_id)
            .count()
            == 1
            && after.iter().any(|entry| {
                entry.drivewsid == file_id && entry.etag == etag && entry.size == size
            }),
        "iCloud file changed during independent read"
    );
    Ok((actual_hash, size))
}

#[tokio::main]
async fn main() -> Result<()> {
    let started = Instant::now();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let [flag, run] = args.as_slice()
        && flag == "--account-uploads"
    {
        return cirrove_service::validation::icloud_account_uploads(Uuid::parse_str(run)?).await;
    }
    if let [flag, run] = args.as_slice()
        && flag == "--account-namespace"
    {
        return cirrove_service::validation::icloud_account_namespace(Uuid::parse_str(run)?).await;
    }
    if let [flag, run] = args.as_slice()
        && flag == "--account-mounted"
    {
        return cirrove_service::validation::icloud_account_mounted(Uuid::parse_str(run)?).await;
    }
    if let [flag, run] = args.as_slice()
        && flag == "--account-combined"
    {
        return cirrove_service::validation::icloud_account_combined(Uuid::parse_str(run)?).await;
    }
    if let [flag, run] = args.as_slice()
        && flag == "--inspect-account-combined"
    {
        return cirrove_service::validation::icloud_account_combined_inspect(Uuid::parse_str(run)?)
            .await;
    }
    if let [flag, run] = args.as_slice()
        && flag == "--account-empty"
    {
        return cirrove_service::validation::icloud_account_empty(Uuid::parse_str(run)?).await;
    }
    if let [flag, run] = args.as_slice()
        && flag == "--account-empty-read"
    {
        return cirrove_service::validation::icloud_account_empty_read(Uuid::parse_str(run)?).await;
    }
    if let [flag, run] = args.as_slice()
        && flag == "--account-mounted-empty-replace"
    {
        return cirrove_service::validation::icloud_account_mounted_empty_replace(Uuid::parse_str(
            run,
        )?)
        .await;
    }
    if let [flag, run] = args.as_slice()
        && flag == "--account-trash-lookup"
    {
        return cirrove_service::validation::icloud_account_trash_lookup(Uuid::parse_str(run)?)
            .await;
    }
    if let [flag, run] = args.as_slice()
        && flag == "--account-trash-lookup-control"
    {
        return cirrove_service::validation::icloud_account_trash_lookup_control(Uuid::parse_str(
            run,
        )?)
        .await;
    }
    let large_file = args
        .first()
        .is_some_and(|flag| flag.starts_with("--large-"));
    let rename_folder = args.first().is_some_and(|flag| flag == "--rename-folder");
    let after_rename_folder = args
        .first()
        .is_some_and(|flag| flag == "--resume-after-rename");
    let rename_file = args.first().is_some_and(|flag| flag == "--rename-file");
    let after_rename_file = args
        .first()
        .is_some_and(|flag| flag == "--resume-after-file-rename");
    let rename_file_again = args
        .first()
        .is_some_and(|flag| flag == "--rename-file-again");
    let after_rename_file_again = args
        .first()
        .is_some_and(|flag| flag == "--resume-after-file-rename-again");
    let move_file = args.first().is_some_and(|flag| flag == "--move-file");
    let after_move_file = args
        .first()
        .is_some_and(|flag| flag == "--resume-after-file-move");
    let prepare_folder_move = args
        .first()
        .is_some_and(|flag| flag == "--prepare-folder-move");
    let move_folder = args.first().is_some_and(|flag| flag == "--move-folder");
    let after_move_folder = args
        .first()
        .is_some_and(|flag| flag == "--resume-after-folder-move");
    let rename_nested_file = args
        .first()
        .is_some_and(|flag| flag == "--rename-nested-file");
    let after_nested_rename = args
        .first()
        .is_some_and(|flag| flag == "--resume-after-nested-rename");
    let inspect_nested_rename = args
        .first()
        .is_some_and(|flag| flag == "--inspect-nested-rename");
    let inspect_replace = args.first().is_some_and(|flag| flag == "--inspect-replace");
    let inspect_replace_phase = args
        .first()
        .is_some_and(|flag| flag == "--inspect-replace-phase");
    let retry_failed_nested = args
        .first()
        .is_some_and(|flag| flag == "--retry-failed-nested");
    let create_nested_file = args
        .first()
        .is_some_and(|flag| flag == "--create-nested-file");
    let after_nested_create = args
        .first()
        .is_some_and(|flag| flag == "--resume-after-nested-create");
    let remove_nested_file = args
        .first()
        .is_some_and(|flag| flag == "--remove-nested-file");
    let after_nested_remove = args
        .first()
        .is_some_and(|flag| flag == "--resume-after-nested-remove");
    let create_nested_replace_source = args
        .first()
        .is_some_and(|flag| flag == "--create-nested-replace-source");
    let replace_nested_source = args
        .first()
        .is_some_and(|flag| flag == "--replace-nested-source");
    let after_nested_replace = args
        .first()
        .is_some_and(|flag| flag == "--resume-after-nested-replace");
    let finish_replace = args.first().is_some_and(|flag| flag == "--finish-replace");
    let finish_nested_replace = args
        .first()
        .is_some_and(|flag| flag == "--finish-nested-replace");
    let (
        resume,
        remove_folder,
        after_remove,
        remove_file,
        after_file_remove,
        replace_file,
        after_replace,
    ) = match args.as_slice() {
        [] => (None, false, false, false, false, false, false),
        [flag] if flag == "--large-create" => (None, false, false, false, false, false, false),
        [flag, id] if flag == "--resume" => (
            Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
            false,
            false,
            false,
            false,
            false,
            false,
        ),
        [flag, id] if flag == "--rename-folder" || flag == "--resume-after-rename" => (
            Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
            false,
            false,
            false,
            false,
            false,
            false,
        ),
        [flag, id]
            if flag == "--rename-file"
                || flag == "--resume-after-file-rename"
                || flag == "--rename-file-again"
                || flag == "--resume-after-file-rename-again"
                || flag == "--move-file"
                || flag == "--resume-after-file-move"
                || flag == "--prepare-folder-move"
                || flag == "--move-folder"
                || flag == "--resume-after-folder-move"
                || flag == "--rename-nested-file"
                || flag == "--resume-after-nested-rename"
                || flag == "--inspect-nested-rename"
                || flag == "--inspect-replace"
                || flag == "--inspect-replace-phase"
                || flag == "--retry-failed-nested"
                || flag == "--create-nested-file"
                || flag == "--resume-after-nested-create"
                || flag == "--remove-nested-file"
                || flag == "--resume-after-nested-remove"
                || flag == "--create-nested-replace-source"
                || flag == "--replace-nested-source"
                || flag == "--finish-nested-replace"
                || flag == "--resume-after-nested-replace" =>
        {
            (
                Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
                false,
                false,
                false,
                false,
                false,
                false,
            )
        }
        [flag, id] if flag == "--remove-folder" => (
            Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
            true,
            false,
            false,
            false,
            false,
            false,
        ),
        [flag, id] if flag == "--resume-after-remove" => (
            Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
            false,
            true,
            false,
            false,
            false,
            false,
        ),
        [flag, id] if flag == "--remove-file" => (
            Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
            false,
            false,
            true,
            false,
            false,
            false,
        ),
        [flag, id] if flag == "--resume-after-file-remove" => (
            Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
            false,
            false,
            false,
            true,
            false,
            false,
        ),
        [flag, id] if flag == "--replace" || flag == "--finish-replace" => (
            Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
            false,
            false,
            false,
            false,
            true,
            false,
        ),
        [flag, id] if flag == "--large-replace" => (
            Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
            false,
            false,
            false,
            false,
            true,
            false,
        ),
        [flag, id] if flag == "--resume-after-replace" => (
            Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
            false,
            false,
            false,
            false,
            false,
            true,
        ),
        [flag, id] if flag == "--large-resume-after-replace" => (
            Some(Uuid::parse_str(id).context("invalid fixture run ID")?),
            false,
            false,
            false,
            false,
            false,
            true,
        ),
        _ => bail!(
            "usage: cirrove-icloud-mounted-write-probe [--resume RUN_UUID | --prepare-folder-move RUN_UUID | --move-folder RUN_UUID | --resume-after-folder-move RUN_UUID | --rename-folder RUN_UUID | --resume-after-rename RUN_UUID | --rename-file RUN_UUID | --resume-after-file-rename RUN_UUID | --rename-file-again RUN_UUID | --resume-after-file-rename-again RUN_UUID | --move-file RUN_UUID | --resume-after-file-move RUN_UUID | --rename-nested-file RUN_UUID | --resume-after-nested-rename RUN_UUID | --inspect-nested-rename RUN_UUID | --retry-failed-nested RUN_UUID | --create-nested-file RUN_UUID | --resume-after-nested-create RUN_UUID | --remove-nested-file RUN_UUID | --resume-after-nested-remove RUN_UUID | --create-nested-replace-source RUN_UUID | --replace-nested-source RUN_UUID | --finish-nested-replace RUN_UUID | --resume-after-nested-replace RUN_UUID | --remove-folder RUN_UUID | --resume-after-remove RUN_UUID | --remove-file RUN_UUID | --resume-after-file-remove RUN_UUID | --replace RUN_UUID | --finish-replace RUN_UUID | --resume-after-replace RUN_UUID | --large-create | --large-replace RUN_UUID | --large-resume-after-replace RUN_UUID]"
        ),
    };
    let state = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-gui-connect-validation/state")
        .canonicalize()
        .context("isolated iCloud validation state unavailable")?;
    let settings = Settings::load(&state)?;
    let accounts: Vec<_> = settings
        .accounts
        .iter()
        .filter(|account| {
            matches!(account.registration, AppRegistration::ICloud)
                && account.label == "iCloudGuiValidation"
        })
        .collect();
    ensure!(
        accounts.len() == 1,
        "expected exactly one isolated iCloud validation account"
    );
    let account = accounts[0];
    let vault = SealedSessionVault::new(&state, &account.id)?;
    let snapshot = vault
        .load(&account.credential_id)
        .await?
        .context("isolated iCloud session missing; sign in locally")?;
    let apple_id = &account.identity.username;
    let mut creator = ICloudReadSession::from_session_snapshot(&snapshot, apple_id)?;
    let root_entries = creator
        .list_root()
        .await
        .context("isolated iCloud session is not usable")?;

    let run = resume.unwrap_or_else(Uuid::new_v4);
    let run_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../.local-state/icloud-mounted-write-{run}"));
    if resume.is_some() {
        ensure!(
            run_dir.is_dir(),
            "isolated fixture run directory is missing"
        );
    }
    private_dir(&run_dir)?;
    let folder = if resume.is_some() {
        let name = format!("Cirrove Write Validation-{run}");
        let matches: Vec<_> = root_entries
            .iter()
            .filter(|entry| entry.is_folder() && entry.display_name() == name)
            .collect();
        ensure!(
            matches.len() == 1,
            "exact test folder is not uniquely present in iCloud root"
        );
        creator
            .validation_folder_at_root(&matches[0].drivewsid)
            .await
            .context("saved fixture folder identity is no longer valid")?
    } else {
        creator
            .create_validation_folder(&format!("Cirrove Write Validation-{run}"))
            .await
            .context("creating the dedicated iCloud test folder")?
    };
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let root = Node {
        id: folder.id().into(),
        parent_id: None,
        name: folder.name().into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    };
    let journal = UploadJournal::open(&run_dir.join("journal"), &account.id, 64 * 1024 * 1024)?;
    if finish_replace {
        let rows = journal.list(0, 16)?;
        ensure!(
            rows.len() == 2
                && rows[0].state == UploadState::Uploaded
                && matches!(rows[1].intent, UploadIntent::Replace { .. }),
            "finish-replace requires the existing two-upload replacement fixture"
        );
    }
    if inspect_replace_phase {
        return inspect_saved_replace_phase(
            &journal,
            &run_dir,
            &account.id,
            apple_id,
            &snapshot,
            folder.id(),
        )
        .await;
    }
    let owned = restored_owned(&journal, &scope, folder.id())?;
    if resume.is_some() {
        ensure!(
            !owned.is_empty() || after_file_remove,
            "fixture has no confirmed IDs to restore"
        );
    } else {
        ensure!(
            owned.is_empty(),
            "new fixture journal already owns remote IDs"
        );
    }
    let journal = Arc::new(Mutex::new(journal));
    let upload_parent = Node {
        parent_id: Some(ROOT_ID.into()),
        ..root.clone()
    };
    let provider = Arc::new(Fixture::new(
        scope.clone(),
        root,
        ICloudDrive::on_demand_from_session_snapshot(scope.clone(), apple_id, &snapshot)?,
        ICloudFileCreate::from_sealed_session(
            scope.clone(),
            apple_id.clone(),
            account.credential_id.clone(),
            &state,
            upload_parent.clone(),
        )?,
        ICloudFolderCreate::from_sealed_session(
            scope,
            apple_id.clone(),
            account.credential_id.clone(),
            &state,
            upload_parent,
            Arc::new(SealedFolderCheckpointVault::new(&run_dir, &account.id)?),
        )?,
        RemovalContext {
            apple_id: apple_id.clone(),
            credential_id: account.credential_id.clone(),
            state: state.clone(),
            journal: journal.clone(),
            metadata_db: run_dir
                .join("engine")
                .join("accounts")
                .join(&account.id)
                .join("metadata.db"),
        },
        owned,
    ));
    if inspect_nested_rename {
        return provider.inspect_failed_nested_rename().await;
    }
    if inspect_replace {
        return provider.inspect_failed_replace().await;
    }
    if retry_failed_nested {
        provider.retry_failed_nested_rename().await?;
    }
    let mut config = account.clone();
    config.enabled = false;
    config.access = AccessMode::ReadWrite;
    config.root_id = folder.id().into();
    config.mount_path = run_dir.join("mount");
    config.poll_seconds = 3600;
    config.cache_bytes = 64 * 1024 * 1024;
    private_dir(&config.mount_path)?;
    let engine = Engine::new(config, provider.clone(), run_dir.join("engine")).await?;
    let session = WritableSession::mount(
        engine.clone(),
        journal,
        provider,
        Arc::new(SealedUploadCheckpointVault::new(&run_dir, &account.id)?),
    )
    .await?;
    println!(
        "Isolated iCloud test mount active: {}",
        engine.account.mount_path.display()
    );
    if replace_file {
        eprintln!(
            "replacement timing: mount ready {:.1}s",
            started.elapsed().as_secs_f64()
        );
    }
    if prepare_folder_move || move_folder || after_move_folder {
        let result: Result<()> = async {
            let script = if prepare_folder_move {
                APP_PREPARE_FOLDER_MOVE
            } else if move_folder {
                APP_MOVE_FOLDER
            } else {
                APP_READ_MOVED_FOLDER
            };
            let output = tokio::process::Command::new("python3")
                .args(["-c", script])
                .arg(&engine.account.mount_path)
                .kill_on_drop(true)
                .output()
                .await
                .context("running folder move through isolated FUSE mount")?;
            ensure!(
                output.status.success(),
                "mounted folder move application failed ({}); evidence retained",
                String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(512)])
            );
            let expected_mutations = if prepare_folder_move { 2 } else { 3 };
            tokio::time::timeout(Duration::from_secs(120), async {
                loop {
                    let uploads = session.uploads(0, 16).await?;
                    let mutations = session.mutations(0, 16).await?;
                    ensure!(
                        !uploads.iter().any(|r| matches!(r.state, UploadState::Failed | UploadState::Conflict))
                            && !mutations.iter().any(|r| matches!(r.state, MutationState::Failed | MutationState::Conflict | MutationState::NeedsReview)),
                        "isolated folder move worker requires review"
                    );
                    ensure!(uploads.len() <= 2 && mutations.len() <= expected_mutations,
                        "unexpected operation in isolated folder move run");
                    if uploads.len() == 2
                        && uploads.iter().all(|r| r.state == UploadState::Uploaded)
                        && mutations.len() == expected_mutations
                        && mutations.iter().all(|r| r.state == MutationState::Applied)
                    {
                        break Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            })
            .await
            .context("isolated folder move worker timed out")??;

            let mut independent = ICloudReadSession::from_session_snapshot(&snapshot, apple_id)?;
            let root_items = independent.list_folder(folder.id()).await?;
            let destinations: Vec<_> = root_items.iter().filter(|item| {
                item.is_folder() && item.display_name() == "Mounted Move Destination"
            }).collect();
            ensure!(destinations.len() == 1, "exact destination folder is not visible");
            let destination_id = destinations[0].drivewsid.clone();
            let source_items: Vec<_> = if prepare_folder_move {
                root_items.iter().filter(|item| {
                    item.is_folder() && item.display_name() == "Mounted Folder"
                }).collect()
            } else {
                ensure!(!root_items.iter().any(|item| {
                    item.is_folder() && item.display_name() == "Mounted Folder"
                }), "source folder remains in the old parent");
                let inside = independent.list_folder(&destination_id).await?;
                let moved: Vec<_> = inside.into_iter().filter(|item| {
                    item.is_folder() && item.display_name() == "Mounted Folder"
                }).collect();
                ensure!(moved.len() == 1, "moved folder is not uniquely visible");
                let moved_id = moved[0].drivewsid.clone();
                // Keep a local item with the same identity shape for the checks below.
                let child_items = independent.list_folder(&moved_id).await?;
                let children: Vec<_> = child_items.iter().filter(|item| {
                    !item.is_folder() && item.display_name() == "Move Child.txt"
                }).collect();
                ensure!(children.len() == 1, "moved child is not uniquely visible");
                let child = children[0];
                ensure!(child.size <= 4096, "moved child is unexpectedly large");
                let bytes = independent.read_range_in_folder(&moved_id, &child.drivewsid, 0, child.size as u32).await?;
                ensure!(bytes == b"Cirrove mounted folder move child\n", "moved child bytes differ");
                let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(run_dir.join("folder-move-ids.json"))?)?;
                ensure!(saved["folder"] == moved_id && saved["child"] == child.drivewsid
                    && saved["destination"] == destination_id
                    && saved["sha256"] == hex::encode(Sha256::digest(&bytes)),
                    "folder or child identity changed across move");
                println!("Isolated folder move verified: same folder and child IDs, complete bytes, fresh process={after_move_folder}");
                return Ok(());
            };
            ensure!(source_items.len() == 1, "prepared source folder is not uniquely visible");
            let source_id = source_items[0].drivewsid.clone();
            let child_items = independent.list_folder(&source_id).await?;
            let children: Vec<_> = child_items.iter().filter(|item| {
                !item.is_folder() && item.display_name() == "Move Child.txt"
            }).collect();
            ensure!(children.len() == 1, "prepared child is not uniquely visible");
            let child = children[0];
            ensure!(child.size <= 4096, "prepared child is unexpectedly large");
            let bytes = independent.read_range_in_folder(&source_id, &child.drivewsid, 0, child.size as u32).await?;
            ensure!(bytes == b"Cirrove mounted folder move child\n", "prepared child bytes differ");
            std::fs::write(
                run_dir.join("folder-move-ids.json"),
                serde_json::to_vec(&serde_json::json!({
                    "folder": source_id,
                    "destination": destination_id,
                    "child": child.drivewsid,
                    "sha256": hex::encode(Sha256::digest(&bytes)),
                }))?,
            )?;
            println!("Isolated populated folder prepared for move: {run}");
            Ok(())
        }.await;
        let shutdown = session.shutdown().await;
        result?;
        shutdown?;
        return Ok(());
    }
    let check = async {
        if create_nested_replace_source
            || replace_nested_source
            || after_nested_replace
            || finish_nested_replace
        {
            if !finish_nested_replace {
                let script = if create_nested_replace_source {
                    APP_CREATE_NESTED_REPLACE_SOURCE
                } else if replace_nested_source {
                    APP_REPLACE_NESTED_SOURCE
                } else {
                    APP_READ_NESTED_REPLACEMENT
                };
                let output = tokio::process::Command::new("python3")
                    .args(["-c", script])
                    .arg(&engine.account.mount_path)
                    .kill_on_drop(true)
                    .output()
                    .await
                    .context("running nested iCloud replacement application")?;
                ensure!(
                    output.status.success(),
                    "nested replacement application failed ({}); evidence retained",
                    String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(512)])
                );
            }
            let expected_uploads = if create_nested_replace_source { 2 } else { 3 };
            let uploads = loop {
                let rows = session.uploads(0, 16).await?;
                ensure!(
                    !rows.iter().any(|row| matches!(
                        row.state,
                        UploadState::Failed | UploadState::Conflict
                    )),
                    "nested replacement upload requires review"
                );
                if rows.len() == expected_uploads
                    && rows.iter().all(|row| row.state == UploadState::Uploaded)
                {
                    break rows;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            };
            let mutations = loop {
                let rows = session.mutations(0, 16).await?;
                ensure!(
                    !rows.iter().any(|row| matches!(
                        row.state,
                        MutationState::Failed | MutationState::Conflict
                    )),
                    "nested replacement folder requires review"
                );
                if rows.len() == 1 && rows[0].state == MutationState::Applied {
                    break rows;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            };
            let Some(MutationReceipt::Upsert(parent)) = mutations[0].receipt.as_ref() else {
                bail!("nested replacement parent lacks a folder receipt");
            };
            ensure!(
                parent.kind == NodeKind::Folder
                    && parent.name == "Mounted Folder"
                    && parent.parent_id.as_deref() == Some(folder.id()),
                "nested replacement parent is not the confirmed owned folder"
            );
            let mut independent = ICloudReadSession::from_session_snapshot(&snapshot, apple_id)?;
            let root = independent.list_folder(folder.id()).await?;
            ensure!(
                root.iter()
                    .filter(|entry| entry.drivewsid == parent.id
                        && entry.display_name() == parent.name
                        && entry.is_folder())
                    .count()
                    == 1,
                "nested replacement parent differs from independent root listing"
            );
            let source = &uploads[1];
            let old = source
                .remote
                .as_ref()
                .context("nested source lacks exact receipt")?;
            ensure!(
                matches!(&source.intent, UploadIntent::Create { parent: id, name }
                    if id == &parent.id && name == "Nested Replacement.txt")
                    && old.parent_id.as_deref() == Some(parent.id.as_str())
                    && old.name == "Nested Replacement.txt",
                "nested source identity is not journal-confirmed"
            );
            let (current, receipt, expected_bytes) = if create_nested_replace_source {
                (
                    old,
                    source,
                    b"Cirrove nested replacement source\n".as_slice(),
                )
            } else {
                let replacement = &uploads[2];
                let new = replacement
                    .remote
                    .as_ref()
                    .context("nested replacement lacks exact receipt")?;
                ensure!(
                    matches!(&replacement.intent, UploadIntent::Replace { item, expected_etag }
                        if item == &old.id && old.etag.as_deref() == Some(expected_etag))
                        && new.id != old.id
                        && new.parent_id == old.parent_id
                        && new.name == old.name
                        && independent.exact_item_in_trash(&old.id).await?,
                    "nested replacement did not preserve the old exact ID in Trash"
                );
                (
                    new,
                    replacement,
                    b"Cirrove nested replacement result\n".as_slice(),
                )
            };
            let children = independent.list_folder(&parent.id).await?;
            ensure!(
                children
                    .iter()
                    .filter(|entry| entry.drivewsid == current.id
                        && entry.display_name() == "Nested Replacement.txt"
                        && !entry.is_folder())
                    .count()
                    == 1
                    && !children
                        .iter()
                        .any(|entry| entry.drivewsid == old.id && old.id != current.id),
                "nested replacement independent listing differs from journal identity"
            );
            let bytes = independent
                .read_small_file_in_folder(&parent.id, &current.id)
                .await?;
            ensure!(
                bytes == expected_bytes
                    && receipt.size == bytes.len() as u64
                    && receipt.sha256 == hex::encode(Sha256::digest(&bytes)),
                "nested replacement bytes differ from journal receipt"
            );
            println!(
                "Nested mounted fixture verified after {} by independent iCloud listing and journal receipts.",
                if create_nested_replace_source {
                    "source create"
                } else if replace_nested_source || finish_nested_replace {
                    "two-ID replacement"
                } else {
                    "remount after replacement"
                }
            );
            return Ok::<(), anyhow::Error>(());
        }
        let filename = "Mounted Create.txt";
        if !finish_replace {
            let output = tokio::process::Command::new("python3")
                .args([
                    "-c",
                    if remove_folder {
                        APP_REMOVE_FOLDER
                    } else if rename_folder {
                        APP_RENAME_FOLDER
                    } else if after_rename_folder {
                        APP_READ_RENAMED
                    } else if rename_file {
                        APP_RENAME_FILE
                    } else if after_rename_file {
                        APP_READ_RENAMED_FILE
                    } else if rename_file_again {
                        APP_RENAME_FILE_AGAIN
                    } else if after_rename_file_again {
                        APP_READ_RENAMED_FILE_AGAIN
                    } else if move_file {
                        APP_MOVE_FILE
                    } else if after_move_file {
                        APP_READ_MOVED_FILE
                    } else if rename_nested_file {
                        APP_RENAME_NESTED_FILE
                    } else if after_nested_rename || retry_failed_nested {
                        APP_READ_NESTED_RENAMED_FILE
                    } else if create_nested_file {
                        APP_CREATE_NESTED_FILE
                    } else if after_nested_create {
                        APP_READ_NESTED_CREATED_FILE
                    } else if remove_nested_file {
                        APP_REMOVE_NESTED_FILE
                    } else if after_nested_remove {
                        APP_READ_NESTED_REMOVED
                    } else if replace_file {
                        if large_file {
                            APP_LARGE_REPLACE
                        } else {
                            APP_REPLACE
                        }
                    } else if after_replace {
                        if large_file {
                            APP_LARGE_READ_REPLACED
                        } else {
                            APP_READ_REPLACED
                        }
                    } else if remove_file {
                        APP_REMOVE_FILE
                    } else if after_file_remove {
                        APP_READ_ALL_REMOVED
                    } else if after_remove {
                        APP_READ_REMOVED
                    } else if resume.is_some() {
                        APP_READ
                    } else {
                        if large_file { APP_LARGE_CREATE } else { APP }
                    },
                ])
                .arg(&engine.account.mount_path)
                .arg(filename)
                .kill_on_drop(true)
                .output()
                .await
                .context("running separate FUSE write process")?;
            ensure!(
                output.status.success(),
                "FUSE application process failed ({}); evidence retained",
                String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(512)])
            );
        }
        if replace_file && !finish_replace {
            eprintln!(
                "replacement timing: FUSE save returned {:.1}s",
                started.elapsed().as_secs_f64()
            );
        }
        let uploads = loop {
            let rows = session.uploads(0, 16).await?;
            ensure!(
                !rows
                    .iter()
                    .any(|r| matches!(r.state, UploadState::Failed | UploadState::Conflict)),
                "mounted upload requires review"
            );
            let expected = if replace_file
                || after_replace
                || create_nested_file
                || after_nested_create
                || remove_nested_file
                || after_nested_remove
            {
                2
            } else {
                1
            };
            if rows.len() == expected && rows.iter().all(|r| r.state == UploadState::Uploaded) {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        if replace_file {
            eprintln!(
                "replacement timing: uploads confirmed {:.1}s",
                started.elapsed().as_secs_f64()
            );
        }
        let mutations = loop {
            let rows = session.mutations(0, 16).await?;
            ensure!(
                !rows
                    .iter()
                    .any(|r| matches!(r.state, MutationState::Failed | MutationState::Conflict)),
                "mounted folder create requires review"
            );
            let expected = if remove_nested_file || after_nested_remove {
                6
            } else if rename_nested_file
                || after_nested_rename
                || retry_failed_nested
                || create_nested_file
                || after_nested_create
            {
                5
            } else if move_file || after_move_file {
                4
            } else if remove_file
                || after_file_remove
                || rename_file_again
                || after_rename_file_again
            {
                3
            } else if remove_folder
                || after_remove
                || rename_folder
                || after_rename_folder
                || rename_file
                || after_rename_file
            {
                2
            } else {
                1
            };
            if rows.len() == expected && rows.iter().all(|r| r.state == MutationState::Applied) {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        if replace_file {
            eprintln!(
                "replacement timing: mutations confirmed {:.1}s",
                started.elapsed().as_secs_f64()
            );
        }
        let mut independent = ICloudReadSession::from_session_snapshot(&snapshot, apple_id)?;
        let children = independent.list_folder(folder.id()).await?;
        if replace_file {
            eprintln!(
                "replacement timing: independent listing {:.1}s",
                started.elapsed().as_secs_f64()
            );
        }
        let nested_renamed = rename_nested_file
            || after_nested_rename
            || retry_failed_nested
            || create_nested_file
            || after_nested_create
            || remove_nested_file
            || after_nested_remove;
        let moved = move_file || after_move_file || nested_renamed;
        let expected_file_name = if nested_renamed {
            "Nested Renamed.txt"
        } else if moved || rename_file_again || after_rename_file_again {
            "Mounted Renamed Again.txt"
        } else if rename_file || after_rename_file {
            "Mounted Renamed.txt"
        } else {
            filename
        };
        let expected_folder_name = if rename_folder || after_rename_folder {
            "Mounted Renamed"
        } else {
            "Mounted Folder"
        };
        let created_folder = children
            .iter()
            .find(|entry| entry.display_name() == expected_folder_name && entry.is_folder());
        let nested_children = if moved {
            Some(
                independent
                    .list_folder(
                        &created_folder
                            .context("moved file destination is not listed")?
                            .drivewsid,
                    )
                    .await?,
            )
        } else {
            None
        };
        let file = nested_children
            .as_ref()
            .unwrap_or(&children)
            .iter()
            .find(|entry| entry.display_name() == expected_file_name && !entry.is_folder());
        let uploaded = uploads[0]
            .remote
            .as_ref()
            .context("uploaded journal record lacks remote identity")?;
        let current = if replace_file || after_replace {
            let replaced = uploads[1]
                .remote
                .as_ref()
                .context("replacement journal record lacks remote identity")?;
            ensure!(
                matches!(&uploads[1].intent, UploadIntent::Replace { item, expected_etag }
                    if item == &uploaded.id && Some(expected_etag.as_str()) == uploaded.etag.as_deref()),
                "replacement intent is not bound to the original receipt"
            );
            ensure!(
                replaced.id != uploaded.id
                    && replaced.name == uploaded.name
                    && replaced.parent_id == uploaded.parent_id,
                "replacement did not install a new ID at the same name and parent"
            );
            ensure!(
                independent.exact_item_in_trash(&uploaded.id).await?,
                "old replacement ID is not uniquely recoverable in Trash"
            );
            if replace_file {
                eprintln!(
                    "replacement timing: old ID in Trash {:.1}s",
                    started.elapsed().as_secs_f64()
                );
            }
            ensure!(
                !children.iter().any(|entry| {
                    entry.display_name() == format!("staged-by-cirrove-{}.txt", uploads[1].id)
                        || entry.display_name()
                            == format!("recovery-by-cirrove-{}.txt", uploads[1].id)
                }),
                "replacement left a visible staged or recovery item"
            );
            replaced
        } else {
            uploaded
        };
        ensure!(
            uploaded.parent_id.as_deref() == Some(folder.id()),
            "uploaded receipt belongs to another parent"
        );
        if remove_file || after_file_remove {
            ensure!(
                file.is_none(),
                "removed file remains in independent parent listing"
            );
            let Some(MutationReceipt::Removed { item }) = mutations[2].receipt.as_ref() else {
                bail!("file removal lacks a confirmed receipt");
            };
            ensure!(
                item == &uploaded.id,
                "file removal receipt names another item"
            );
        } else {
            ensure!(
                file.is_some_and(|entry| entry.drivewsid == current.id),
                "independent file identity differs from uploaded receipt"
            );
            if moved {
                let Some(MutationReceipt::Upsert(moved_file)) = mutations[3].receipt.as_ref()
                else {
                    bail!("file move lacks a confirmed upsert receipt");
                };
                ensure!(
                    moved_file.id == uploaded.id
                        && moved_file.name == "Mounted Renamed Again.txt"
                        && moved_file.parent_id.as_deref()
                            == created_folder.map(|folder| folder.drivewsid.as_str())
                        && !children.iter().any(|entry| entry.drivewsid == uploaded.id),
                    "moved file lost its exact identity or remains at the source"
                );
                if nested_renamed {
                    let Some(MutationReceipt::Upsert(renamed)) = mutations[4].receipt.as_ref()
                    else {
                        bail!("nested rename lacks a confirmed upsert receipt");
                    };
                    ensure!(
                        renamed.id == uploaded.id
                            && renamed.name == expected_file_name
                            && renamed.parent_id == moved_file.parent_id
                            && nested_children.as_ref().is_some_and(|entries| !entries
                                .iter()
                                .any(|entry| entry.display_name() == "Mounted Renamed Again.txt")),
                        "nested rename lost identity or left its old name"
                    );
                }
            }
            if rename_file || after_rename_file || rename_file_again || after_rename_file_again {
                let rename_index = if rename_file_again || after_rename_file_again {
                    2
                } else {
                    1
                };
                let Some(MutationReceipt::Upsert(renamed)) =
                    mutations[rename_index].receipt.as_ref()
                else {
                    bail!("file rename lacks a confirmed upsert receipt");
                };
                ensure!(
                    renamed.id == uploaded.id
                        && renamed.name == expected_file_name
                        && renamed.parent_id.as_deref() == Some(folder.id())
                        && !children
                            .iter()
                            .any(|entry| entry.display_name() == "Mounted Create.txt")
                        && (!(rename_file_again || after_rename_file_again)
                            || !children
                                .iter()
                                .any(|entry| entry.display_name() == "Mounted Renamed.txt")),
                    "renamed file lost its exact identity or source name remains"
                );
            }
        }
        let Some(MutationReceipt::Upsert(created)) = mutations[0].receipt.as_ref() else {
            anyhow::bail!("folder mutation lacks a remote upsert receipt");
        };
        ensure!(
            created.parent_id.as_deref() == Some(folder.id()),
            "folder creation receipt has a different parent"
        );
        if remove_folder || after_remove || remove_file || after_file_remove {
            ensure!(
                created_folder.is_none(),
                "removed folder remains in independent parent listing"
            );
            let Some(MutationReceipt::Removed { item }) = mutations[1].receipt.as_ref() else {
                bail!("folder removal lacks a confirmed receipt");
            };
            ensure!(
                item == &created.id,
                "folder removal receipt names another item"
            );
        } else if rename_folder || after_rename_folder {
            let listed = created_folder
                .context("independent iCloud listing does not contain renamed folder")?;
            let Some(MutationReceipt::Upsert(renamed)) = mutations[1].receipt.as_ref() else {
                bail!("folder rename lacks a confirmed upsert receipt");
            };
            ensure!(
                created.id == renamed.id
                    && renamed.id == listed.drivewsid
                    && renamed.parent_id.as_deref() == Some(folder.id())
                    && renamed.name == "Mounted Renamed"
                    && !children
                        .iter()
                        .any(|entry| entry.display_name() == "Mounted Folder"),
                "renamed folder lost its exact identity or source name remains"
            );
        } else {
            let listed = created_folder
                .context("independent iCloud listing does not contain mounted folder")?;
            ensure!(
                created.id == listed.drivewsid,
                "independent folder identity differs from mutation receipt"
            );
        }
        if !remove_file && !after_file_remove {
            let file = file.context("independent iCloud listing does not contain mounted file")?;
            let read_parent = if moved {
                created_folder
                    .context("moved file destination is absent")?
                    .drivewsid
                    .as_str()
            } else {
                folder.id()
            };
            let (digest, size) = if large_file {
                verify_large_bytes(
                    &mut independent,
                    read_parent,
                    &file.drivewsid,
                    &file.etag,
                    file.size,
                    replace_file || after_replace,
                )
                .await?
            } else {
                let bytes = independent
                    .read_small_file_in_folder(read_parent, &file.drivewsid)
                    .await?;
                ensure!(
                    bytes
                        == if replace_file || after_replace {
                            b"Cirrove isolated mounted replacement\n".as_slice()
                        } else {
                            b"Cirrove isolated mounted iCloud validation\n".as_slice()
                        },
                    "remote file bytes differ from mounted write"
                );
                (hex::encode(Sha256::digest(&bytes)), bytes.len() as u64)
            };
            let receipt = if replace_file || after_replace {
                &uploads[1]
            } else {
                &uploads[0]
            };
            ensure!(
                receipt.sha256 == digest,
                "journal digest differs from independent read"
            );
            ensure!(
                receipt.size == size,
                "journal size differs from independent read"
            );
            if replace_file {
                eprintln!(
                    "replacement timing: independent bytes {:.1}s",
                    started.elapsed().as_secs_f64()
                );
            }
        }
        if create_nested_file || after_nested_create {
            let parent = created_folder.context("nested upload parent is absent")?;
            let nested = nested_children
                .as_ref()
                .and_then(|entries| {
                    entries.iter().find(|entry| {
                        entry.display_name() == "Nested Created.txt" && !entry.is_folder()
                    })
                })
                .context("nested created file is absent")?;
            let receipt = &uploads[1];
            let uploaded = receipt
                .remote
                .as_ref()
                .context("nested upload lacks exact receipt")?;
            ensure!(
                matches!(&receipt.intent, UploadIntent::Create { parent: id, name }
                    if id == &parent.drivewsid && name == "Nested Created.txt")
                    && uploaded.id == nested.drivewsid
                    && uploaded.parent_id.as_deref() == Some(parent.drivewsid.as_str())
                    && uploaded.name == "Nested Created.txt"
                    && uploaded.etag.as_deref() == Some(nested.etag.as_str()),
                "nested upload receipt differs from independent iCloud listing"
            );
            let bytes = independent
                .read_small_file_in_folder(&parent.drivewsid, &nested.drivewsid)
                .await?;
            ensure!(
                bytes == b"Cirrove nested iCloud create validation\n"
                    && uploaded.size == bytes.len() as u64
                    && receipt.sha256 == hex::encode(Sha256::digest(&bytes)),
                "nested uploaded bytes differ from the mounted write"
            );
        }
        if remove_nested_file || after_nested_remove {
            let parent = created_folder.context("nested removal parent is absent")?;
            let uploaded = uploads[1]
                .remote
                .as_ref()
                .context("nested upload lacks exact receipt")?;
            ensure!(
                matches!(&uploads[1].intent, UploadIntent::Create { parent: id, name }
                    if id == &parent.drivewsid && name == "Nested Created.txt")
                    && uploaded.parent_id.as_deref() == Some(parent.drivewsid.as_str())
                    && nested_children.as_ref().is_some_and(|entries| !entries
                        .iter()
                        .any(|entry| entry.drivewsid == uploaded.id
                            || entry.display_name() == "Nested Created.txt")),
                "nested removed file remains in the independent parent listing"
            );
            let Some(MutationReceipt::Removed { item }) = mutations[5].receipt.as_ref() else {
                bail!("nested file removal lacks a confirmed receipt");
            };
            ensure!(
                item == &uploaded.id && independent.exact_item_in_trash(item).await?,
                "nested removal did not retain its exact ID in recoverable Trash"
            );
        }
        println!(
            "Mounted fixture verified after {} by independent iCloud listing and journal receipts.",
            if remove_folder {
                "recoverable folder removal"
            } else if rename_folder {
                "conditional folder rename"
            } else if after_rename_folder {
                "remount after folder rename"
            } else if rename_file {
                "conditional file rename"
            } else if after_rename_file {
                "remount after file rename"
            } else if rename_file_again {
                "second conditional file rename"
            } else if after_rename_file_again {
                "remount after second file rename"
            } else if move_file {
                "conditional file move into owned folder"
            } else if after_move_file {
                "remount after file move"
            } else if rename_nested_file {
                "conditional rename inside owned folder"
            } else if after_nested_rename {
                "remount after nested file rename"
            } else if retry_failed_nested {
                "retry of unsent nested rename"
            } else if create_nested_file {
                "nested file create"
            } else if after_nested_create {
                "remount after nested file create"
            } else if remove_nested_file {
                "recoverable nested file removal"
            } else if after_nested_remove {
                "remount after nested file removal"
            } else if replace_file {
                "two-ID replacement"
            } else if after_replace {
                "remount after replacement"
            } else if remove_file {
                "recoverable file removal"
            } else if after_file_remove {
                "remount after file removal"
            } else if after_remove {
                "remount after removal"
            } else if resume.is_some() {
                "remount"
            } else {
                "create"
            }
        );
        Ok::<(), anyhow::Error>(())
    };
    let deadline = if replace_file || replace_nested_source || finish_nested_replace {
        900
    } else {
        360
    };
    let result = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(deadline), check) =>
            result.map_err(|_| anyhow::anyhow!("mounted iCloud validation timed out; fixture and journal retained"))?,
        _ = tokio::signal::ctrl_c() => Err(anyhow::anyhow!("mounted iCloud validation interrupted; fixture and journal retained")),
    };
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    println!(
        "Isolated fixture and journal retained at {}",
        run_dir.display()
    );
    Ok(())
}

/// Read only a checkpoint's phase and exact-item presence. Never display the
/// checkpoint, provider IDs, URLs or response bodies.
async fn inspect_saved_replace_phase(
    journal: &UploadJournal,
    run_dir: &Path,
    account_id: &str,
    apple_id: &str,
    snapshot: &secrecy::SecretString,
    folder_id: &str,
) -> Result<()> {
    let rows = journal.list(0, 16)?;
    let row = rows.last().context("no replacement upload")?;
    let UploadIntent::Replace { item, .. } = &row.intent else {
        bail!("last upload is not a replacement");
    };
    ensure!(
        row.state == UploadState::VerifyRequired && row.remote.is_none(),
        "replacement is not awaiting verification"
    );
    let key = row
        .session_key
        .context("replacement has no saved checkpoint")?;
    let saved = SealedUploadCheckpointVault::new(run_dir, account_id)?
        .load(&format!("upload/{key}"))
        .await?
        .context("replacement checkpoint is unavailable")?;
    println!("checkpoint_bytes={}", saved.expose_secret().len());
    let checkpoint: serde_json::Value = serde_json::from_str(saved.expose_secret())
        .context("replacement outer checkpoint is unreadable")?;
    let phase = checkpoint
        .get("phase")
        .and_then(serde_json::Value::as_object)
        .context("replacement checkpoint lacks a phase")?;
    let (phase_name, inner) = if let Some(value) = phase.get("Stage") {
        ("stage", value)
    } else if let Some(value) = phase.get("Handoff") {
        ("handoff", value)
    } else {
        bail!("replacement checkpoint has an unknown phase");
    };
    let inner_text = inner
        .get("inner")
        .and_then(serde_json::Value::as_str)
        .context("replacement inner checkpoint missing")?;
    println!("inner_checkpoint_bytes={}", inner_text.len());
    let nested: serde_json::Value =
        serde_json::from_str(inner_text).context("replacement inner checkpoint is unreadable")?;
    let has_slot = nested.get("slot").is_some_and(|value| !value.is_null());
    let has_receipt = nested.get("receipt").is_some_and(|value| !value.is_null());
    let mut session = ICloudReadSession::from_session_snapshot(snapshot, apple_id)?;
    let entries = session.list_folder(folder_id).await?;
    let original_present = entries
        .iter()
        .filter(|entry| entry.drivewsid == *item)
        .count()
        == 1;
    let stage_name = format!("staged-by-cirrove-{}.txt", row.id);
    let recovery_name = format!("recovery-by-cirrove-{}.txt", row.id);
    let stage_present = entries
        .iter()
        .any(|entry| entry.display_name() == stage_name);
    let recovery_present = entries
        .iter()
        .any(|entry| entry.display_name() == recovery_name);
    let original_in_trash = session.exact_item_in_trash(item).await?;
    println!(
        "phase={phase_name} slot={has_slot} receipt={has_receipt} original_present={original_present} stage_present={stage_present} recovery_present={recovery_present} original_in_trash={original_in_trash}"
    );
    Ok(())
}
