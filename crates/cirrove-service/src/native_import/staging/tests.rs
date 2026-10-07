#![allow(clippy::unwrap_used)]
use super::*;
use crate::journal::{JournalError, UploadJournal};
use cirrove_core::{Scope, upload::UploadIntent};
use std::os::unix::{fs::FileExt, fs::symlink};

// One stored ZIP member, without another test-only archive dependency.
pub(crate) fn archive(name: &str, bytes: &[u8]) -> Vec<u8> {
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
fn disk_temp() -> tempfile::TempDir {
    // Admission deliberately rejects tmpfs; tests use the registered disk TMPDIR
    // when present, otherwise /var/tmp, never silently relax that boundary.
    let path = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/var/tmp".into());
    let temp = tempfile::tempdir_in(path).unwrap();
    std::fs::set_permissions(temp.path(), Permissions::from_mode(0o700)).unwrap();
    temp
}
fn source(temp: &tempfile::TempDir) -> std::path::PathBuf {
    let path = temp.path().join("source.anything");
    std::fs::write(
        &path,
        archive("Document.pages/Metadata/data", b"owned synthetic data"),
    )
    .unwrap();
    path
}
fn scope() -> Scope {
    Scope {
        account: "owned".into(),
        provider: "icloud".into(),
        collection: "drive".into(),
    }
}
fn intent() -> UploadIntent {
    UploadIntent::Create {
        parent: "root".into(),
        name: "Copy.pages".into(),
    }
}
fn capture(path: &Path, temp: &tempfile::TempDir) -> ValidatedPackageArchive {
    ValidatedPackageArchive::capture(
        path,
        temp.path(),
        "Document.pages",
        &CancellationToken::new(),
    )
    .unwrap()
}
#[test]
fn explicit_flat_numbers_source_is_captured_without_rewriting() {
    let temp = disk_temp();
    let path = temp.path().join("source.numbers");
    let original = archive("Index/Document.iwa", b"owned synthetic Numbers content");
    std::fs::write(&path, &original).unwrap();
    std::fs::set_permissions(&path, Permissions::from_mode(0o400)).unwrap();
    let before = std::fs::metadata(&path).unwrap();
    let result = ValidatedPackageArchive::capture_with_source_layout(
        &path,
        temp.path(),
        PackageSourceLayout::FlatNumbers,
        None,
        &CancellationToken::new(),
    );
    let after = std::fs::metadata(&path).unwrap();
    assert!(same_source(&before, &after));
    assert_eq!(after.mode() & 0o777, 0o400);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!temp.path().join("journal").exists());
    let snapshot = result.expect("explicit flat Numbers admission must succeed");
    assert_eq!(snapshot.file.metadata().unwrap().mode() & 0o777, 0o400);
    assert_eq!(snapshot.file.metadata().unwrap().nlink(), 0);
    assert!(snapshot.file.write_at(b"bad", 0).is_err());
    let (mut file, representation, size, sha256) = snapshot.into_parts();
    let mut captured = Vec::new();
    file.read_to_end(&mut captured).unwrap();
    assert_eq!(captured, original);
    assert_eq!(size, original.len() as u64);
    assert_eq!(sha256, hex::encode(Sha256::digest(&original)));
    representation.validate().unwrap();
    let wire = serde_json::to_value(representation).unwrap();
    assert_eq!(wire["kind"], "flat_numbers_archive");
    assert!(wire.get("expected_root").is_none());
    assert_eq!(wire["semantic"]["version"], 2);
    assert_eq!(wire["semantic"]["files"], 1);
    assert_eq!(wire["semantic"]["entries"], 3);
}
// Exact Apple-style local-only classic-size mirror, no central ZIP64 or sentinel.
fn local_size_mirror(mut bytes: Vec<u8>) -> Vec<u8> {
    let end = bytes.len() - 22;
    let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap());
    let name = u16::from_le_bytes(bytes[26..28].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(bytes[22..26].try_into().unwrap());
    assert_eq!(u32::from_le_bytes(bytes[18..22].try_into().unwrap()), size);
    let mut extra = vec![1, 0, 16, 0];
    extra.extend(u64::from(size).to_le_bytes());
    extra.extend(u64::from(size).to_le_bytes());
    bytes[28..30].copy_from_slice(&20u16.to_le_bytes());
    bytes.splice(30 + name..30 + name, extra);
    bytes[end + 36..end + 40].copy_from_slice(&(central + 20).to_le_bytes());
    bytes
}
#[test]
fn explicit_flat_pages_source_is_captured_without_rewriting() {
    let retained = disk_temp().keep();
    let path = retained.join("source.pages");
    let original = local_size_mirror(archive(
        "Index/Document.iwa",
        b"owned synthetic Pages content",
    ));
    std::fs::write(&path, &original).unwrap();
    std::fs::set_permissions(&path, Permissions::from_mode(0o400)).unwrap();
    let before = std::fs::metadata(&path).unwrap();
    let result = ValidatedPackageArchive::capture_with_source_layout(
        &path,
        &retained,
        serde_json::from_str::<PackageSourceLayout>("\"flat_pages\"")
            .expect("explicit Pages layout"),
        None,
        &CancellationToken::new(),
    );
    let after = std::fs::metadata(&path).unwrap();
    assert!(same_source(&before, &after));
    assert_eq!(after.mode() & 0o777, 0o400);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!retained.join("journal").exists());
    let snapshot = result.expect("explicit flat Pages admission must succeed");
    assert_eq!(snapshot.file.metadata().unwrap().mode() & 0o777, 0o400);
    assert_eq!(snapshot.file.metadata().unwrap().nlink(), 0);
    assert!(snapshot.file.write_at(b"bad", 0).is_err());
    let (mut file, representation, size, sha256) = snapshot.into_parts();
    let mut captured = Vec::new();
    file.read_to_end(&mut captured).unwrap();
    assert_eq!(captured, original);
    assert_eq!(size, original.len() as u64);
    assert_eq!(sha256, hex::encode(Sha256::digest(&original)));
    representation.validate().unwrap();
    let wire = serde_json::to_value(representation).unwrap();
    assert_eq!(wire["kind"], "flat_pages_archive");
    assert!(wire.get("expected_root").is_none());
    assert_eq!(wire["semantic"]["version"], 2);
    assert_eq!(wire["semantic"]["files"], 1);
    assert_eq!(wire["semantic"]["entries"], 3);
}
#[test]
fn validated_snapshot_is_private_read_only_and_independent_of_original() {
    let temp = disk_temp();
    let path = source(&temp);
    let original = std::fs::read(&path).unwrap();
    let snapshot = capture(&path, &temp);
    assert_eq!(snapshot.file.metadata().unwrap().mode() & 0o777, 0o400);
    assert_eq!(snapshot.file.metadata().unwrap().nlink(), 0);
    assert!(snapshot.file.write_at(b"bad", 0).is_err());
    std::fs::write(path, b"replaced after capture").unwrap();
    let mut journal = UploadJournal::open(&temp.path().join("journal"), "owned", 1 << 20).unwrap();
    let record = journal
        .enqueue_validated_package_archive(scope(), intent(), snapshot, &CancellationToken::new())
        .unwrap();
    let UploadRepresentation::PackageArchive { semantic, .. } = &record.representation else {
        panic!("package representation")
    };
    assert_eq!(semantic.version, 2);
    assert_eq!(semantic.files, 1);
    assert_eq!(
        semantic.entries, 3,
        "root + implied Metadata directory + file"
    );
    assert_eq!(record.sha256, hex::encode(Sha256::digest(&original)));
    let mut actual = Vec::new();
    journal
        .payload(record.id)
        .unwrap()
        .read_to_end(&mut actual)
        .unwrap();
    assert_eq!(actual, original);
}
#[test]
fn suffix_wrong_root_unsafe_members_and_limits_do_not_admit() {
    let temp = disk_temp();
    let path = temp.path().join("looks.pages");
    for bytes in [
        b"not a package".to_vec(),
        archive("Other.pages/x", b"x"),
        archive("Document.pages/../escape", b"x"),
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(
            ValidatedPackageArchive::capture(
                &path,
                temp.path(),
                "Document.pages",
                &CancellationToken::new()
            ),
            Err(ImportAdmissionError::Archive)
        ));
    }
    File::create(&path)
        .unwrap()
        .set_len(MAX_ARCHIVE + 1)
        .unwrap();
    assert!(matches!(
        ValidatedPackageArchive::capture(
            &path,
            temp.path(),
            "Document.pages",
            &CancellationToken::new()
        ),
        Err(ImportAdmissionError::Limit)
    ));
}
#[test]
fn symlink_fifo_directory_and_nonprivate_staging_are_refused() {
    let temp = disk_temp();
    let path = source(&temp);
    let link = temp.path().join("link");
    symlink(&path, &link).unwrap();
    let fifo = temp.path().join("fifo");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
    for source in [&link, &fifo, &temp.path().to_path_buf()] {
        assert!(matches!(
            ValidatedPackageArchive::capture(
                source,
                temp.path(),
                "Document.pages",
                &CancellationToken::new()
            ),
            Err(ImportAdmissionError::Source)
        ));
    }
    let outer = disk_temp();
    let staging_link = outer.path().join("staging-link");
    symlink(temp.path(), &staging_link).unwrap();
    assert!(matches!(
        ValidatedPackageArchive::capture(
            &path,
            &staging_link,
            "Document.pages",
            &CancellationToken::new()
        ),
        Err(ImportAdmissionError::Staging)
    ));
    std::fs::set_permissions(temp.path(), Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        ValidatedPackageArchive::capture(
            &path,
            temp.path(),
            "Document.pages",
            &CancellationToken::new()
        ),
        Err(ImportAdmissionError::Staging)
    ));
}
#[test]
fn mutation_and_cancellation_during_capture_never_return_authority() {
    let temp = disk_temp();
    let path = source(&temp);
    let result = ValidatedPackageArchive::capture_observed(
        &path,
        temp.path(),
        "Document.pages",
        &CancellationToken::new(),
        &[],
        |_| {
            OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(1)
                .unwrap();
        },
    );
    assert!(matches!(result, Err(ImportAdmissionError::Source)));
    let path = source(&temp);
    let cancel = CancellationToken::new();
    let result = ValidatedPackageArchive::capture_observed(
        &path,
        temp.path(),
        "Document.pages",
        &cancel,
        &[],
        |_| cancel.cancel(),
    );
    assert!(matches!(result, Err(ImportAdmissionError::Cancelled)));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
}
#[test]
fn raw_receipt_mismatch_cancellation_and_quota_never_publish_objects_or_rows() {
    let temp = disk_temp();
    let path = source(&temp);
    let mut journal = UploadJournal::open(&temp.path().join("journal"), "owned", 1 << 20).unwrap();
    for corrupt_size in [false, true] {
        let mut snapshot = capture(&path, &temp);
        if corrupt_size {
            snapshot.size += 1;
        } else {
            snapshot.sha256 = "0".repeat(64);
        }
        assert!(matches!(
            journal.enqueue_validated_package_archive(
                scope(),
                intent(),
                snapshot,
                &CancellationToken::new()
            ),
            Err(JournalError::Corrupt)
        ));
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        journal.enqueue_validated_package_archive(
            scope(),
            intent(),
            capture(&path, &temp),
            &cancel
        ),
        Err(JournalError::Stale)
    ));
    assert!(journal.list(0, 100).unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(temp.path().join("journal/objects"))
            .unwrap()
            .count(),
        0
    );
    let mut tiny = UploadJournal::open(&temp.path().join("tiny"), "owned", 1).unwrap();
    assert!(matches!(
        tiny.enqueue_validated_package_archive(
            scope(),
            intent(),
            capture(&path, &temp),
            &CancellationToken::new()
        ),
        Err(JournalError::Quota)
    ));
    assert!(tiny.list(0, 100).unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(temp.path().join("tiny/objects"))
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn storage_errors_remain_secret_safe_and_distinguish_device_full() {
    assert!(matches!(
        storage(std::io::Error::from_raw_os_error(libc::ENOSPC)),
        ImportAdmissionError::DeviceFull
    ));
    assert!(matches!(
        storage(std::io::Error::from_raw_os_error(libc::EDQUOT)),
        ImportAdmissionError::DeviceFull
    ));
    let error = storage(std::io::Error::other("private source or provider details"));
    assert!(!error.to_string().contains("private source"));
}

#[test]
fn injected_device_full_during_capture_leaves_no_snapshot_authority_or_file() {
    let temp = disk_temp();
    let path = source(&temp);
    WRITE_FAILURE.with(|fault| fault.set(Some(libc::ENOSPC)));
    let result = ValidatedPackageArchive::capture(
        &path,
        temp.path(),
        "Document.pages",
        &CancellationToken::new(),
    );
    assert!(matches!(result, Err(ImportAdmissionError::DeviceFull)));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    // An independent subsequent capture succeeds; the fault is local/one-shot.
    let _ = capture(&path, &temp);
}

#[test]
fn descriptor_bound_exclusion_rejects_source_alias_into_private_state() {
    let temp = disk_temp();
    let state = disk_temp();
    let path = source(&state);
    let alias = temp.path().join("alias");
    symlink(state.path(), &alias).unwrap();
    let source_alias = alias.join(path.file_name().unwrap());
    assert!(matches!(
        ValidatedPackageArchive::capture_excluding(
            &source_alias,
            temp.path(),
            "Document.pages",
            &CancellationToken::new(),
            &[state.path().to_owned()]
        ),
        Err(ImportAdmissionError::Source)
    ));
}

#[test]
fn flat_layout_requires_explicit_absent_root_and_preserves_source_on_refusal() {
    let temp = disk_temp();
    let path = temp.path().join("misleading.pages");
    let bytes = archive("Index/Document.iwa", b"owned synthetic content");
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, Permissions::from_mode(0o400)).unwrap();
    let before = std::fs::metadata(&path).unwrap();
    for (layout, root) in [
        (PackageSourceLayout::Wrapped, None),
        (PackageSourceLayout::Wrapped, Some("Document.numbers")),
        (PackageSourceLayout::FlatNumbers, Some("Document.numbers")),
        (PackageSourceLayout::FlatNumbers, Some("")),
        (PackageSourceLayout::FlatPages, Some("Source.pages")),
        (PackageSourceLayout::FlatPages, Some("")),
    ] {
        assert!(matches!(
            ValidatedPackageArchive::capture_with_source_layout(
                &path,
                temp.path(),
                layout,
                root,
                &CancellationToken::new(),
            ),
            Err(ImportAdmissionError::Archive)
        ));
    }
    assert!(same_source(&before, &std::fs::metadata(&path).unwrap()));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    // Explicit layout, rather than the misleading local filename, controls
    // archive parsing. Destination-format validation is a separate admission gate.
    let snapshot = ValidatedPackageArchive::capture_with_source_layout(
        &path,
        temp.path(),
        PackageSourceLayout::FlatNumbers,
        None,
        &CancellationToken::new(),
    )
    .unwrap();
    let (_, representation, _, _) = snapshot.into_parts();
    let mut wire = serde_json::to_value(&representation).unwrap();
    assert_eq!(wire["kind"], "flat_numbers_archive");
    assert!(wire.get("expected_root").is_none());
    wire["expected_root"] = serde_json::json!("Invented.numbers");
    assert!(serde_json::from_value::<UploadRepresentation>(wire).is_err());
    std::fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, archive("../Index/Document.iwa", b"unsafe path")).unwrap();
    std::fs::set_permissions(&path, Permissions::from_mode(0o400)).unwrap();
    assert!(matches!(
        ValidatedPackageArchive::capture_with_source_layout(
            &path,
            temp.path(),
            PackageSourceLayout::FlatNumbers,
            None,
            &CancellationToken::new(),
        ),
        Err(ImportAdmissionError::Archive)
    ));
}
