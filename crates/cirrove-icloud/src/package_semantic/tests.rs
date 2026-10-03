#![allow(clippy::unwrap_used)]
use super::*;
use std::io::Write;
fn archive(reverse: bool, year: u16, deflate: bool, changed: bool) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(if deflate {
            zip::CompressionMethod::Deflated
        } else {
            zip::CompressionMethod::Stored
        })
        .last_modified_time(zip::DateTime::from_date_and_time(year, 1, 1, 0, 0, 0).unwrap());
    let entries = if reverse {
        vec![
            ("Metadata/two", b"body-two".as_slice()),
            ("Metadata/one", b"body-one".as_slice()),
        ]
    } else {
        vec![
            ("Metadata/one", b"body-one".as_slice()),
            ("Metadata/two", b"body-two".as_slice()),
        ]
    };
    if !reverse {
        zip.add_directory("Metadata/", options).unwrap();
    }
    for (name, bytes) in entries {
        zip.start_file(name, options).unwrap();
        zip.write_all(if changed && name.ends_with("one") {
            b"bad-data"
        } else {
            bytes
        })
        .unwrap();
    }
    if reverse {
        zip.add_directory("Metadata/", options).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn staged(bytes: &[u8], directory: &std::path::Path) -> (File, PackageDownload) {
    let file = tempfile::tempfile_in(directory).unwrap();
    file.write_all_at(bytes, 0).unwrap();
    (
        file,
        PackageDownload {
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        },
    )
}
fn rejected(bytes: &[u8]) {
    let dir = tempfile::tempdir().unwrap();
    let (file, receipt) = staged(bytes, dir.path());
    let result = fingerprint(&file, &receipt, &CancellationToken::new(), MAX_EXPANDED);
    assert!(result.is_err());
    let message = result.err().unwrap().to_string();
    assert!(!message.contains("Metadata"));
    assert!(!message.contains("body-one"));
}
fn replace_name(bytes: &mut [u8], from: &[u8], to: &[u8]) {
    assert_eq!(from.len(), to.len());
    let offsets: Vec<_> = bytes
        .windows(from.len())
        .enumerate()
        .filter_map(|(i, v)| (v == from).then_some(i))
        .collect();
    assert_eq!(offsets.len(), 2);
    for at in offsets {
        bytes[at..at + to.len()].copy_from_slice(to);
    }
}
fn central(bytes: &[u8]) -> usize {
    let end = bytes.len() - 22;
    u32_at(bytes, end + 16).unwrap()
}
#[test]
fn package_semantic_reordering_timestamps_and_compression_preserve_content() {
    let dir = tempfile::tempdir().unwrap();
    let (a, ar) = staged(&archive(false, 2025, false, false), dir.path());
    let (b, br) = staged(&archive(true, 2026, true, false), dir.path());
    let result = compare_package_archives(&a, &ar, &b, &br, &CancellationToken::new()).unwrap();
    assert_eq!(result.entries, 3);
    assert_eq!(result.files, 2);
    assert_eq!(result.expanded_bytes, 16);
    assert!(!result.raw_archives_equal);
    assert_eq!(result.source_archive_sha256, ar.sha256);
    assert_eq!(result.imported_archive_sha256, br.sha256);
}
#[test]
fn package_semantic_valid_but_changed_file_is_not_equal() {
    let dir = tempfile::tempdir().unwrap();
    let (a, ar) = staged(&archive(false, 2025, true, false), dir.path());
    let (b, br) = staged(&archive(false, 2025, true, true), dir.path());
    assert!(compare_package_archives(&a, &ar, &b, &br, &CancellationToken::new()).is_err());
}
#[test]
fn package_semantic_crc_corruption_duplicate_and_unsafe_paths_are_rejected() {
    let mut bytes = archive(false, 2025, false, false);
    let at = bytes.windows(8).position(|v| v == b"body-one").unwrap();
    bytes[at] ^= 1;
    rejected(&bytes);
    let mut bytes = archive(false, 2025, false, false);
    replace_name(&mut bytes, b"Metadata/two", b"Metadata/one");
    rejected(&bytes);
    for target in [
        b"../123456789".as_slice(),
        b"/12345678901",
        b"C:/123456789",
        b"Metadata\\one",
    ] {
        let mut bytes = archive(false, 2025, false, false);
        replace_name(&mut bytes, b"Metadata/one", target);
        rejected(&bytes);
    }
}
#[test]
fn package_semantic_symlinks_encryption_and_unsupported_methods_are_rejected() {
    for arm in 0..3 {
        let mut bytes = archive(false, 2025, false, false);
        let c = central(&bytes);
        // First central entry is the explicit directory; forbidden types and
        // methods must fail even when its declared expanded length is zero.
        let local = u32_at(&bytes, c + 42).unwrap();
        match arm {
            0 => bytes[c + 38..c + 42].copy_from_slice(&(0o120777u32 << 16).to_le_bytes()),
            1 => {
                bytes[c + 8..c + 10].copy_from_slice(&1u16.to_le_bytes());
                bytes[local + 6..local + 8].copy_from_slice(&1u16.to_le_bytes());
            }
            _ => {
                bytes[c + 10..c + 12].copy_from_slice(&12u16.to_le_bytes());
                bytes[local + 8..local + 10].copy_from_slice(&12u16.to_le_bytes());
            }
        }
        rejected(&bytes);
    }
}
#[test]
fn package_semantic_compressed_expansion_and_declared_counts_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "large",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(&[0; 4096]).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    assert!(bytes.len() < 4096);
    let (file, receipt) = staged(&bytes, dir.path());
    assert!(fingerprint(&file, &receipt, &CancellationToken::new(), 1024).is_err());
    let mut bytes = archive(false, 2025, false, false);
    let end = bytes.len() - 22;
    for at in [end + 8, end + 10] {
        bytes[at..at + 2].copy_from_slice(&10001u16.to_le_bytes());
    }
    rejected(&bytes);
    let mut bytes = archive(false, 2025, false, false);
    let c = central(&bytes);
    bytes[c + 24..c + 28].copy_from_slice(&((MAX_EXPANDED + 1) as u32).to_le_bytes());
    rejected(&bytes);
}
#[test]
fn package_semantic_receipt_mismatch_cancellation_and_local_name_mismatch_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = archive(false, 2025, false, false);
    let (file, mut receipt) = staged(&bytes, dir.path());
    receipt.sha256 = "00".repeat(32);
    assert!(matches!(
        fingerprint(&file, &receipt, &CancellationToken::new(), MAX_EXPANDED),
        Err(ProviderError::VersionChanged)
    ));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        fingerprint(&file, &receipt, &cancel, MAX_EXPANDED),
        Err(ProviderError::Cancelled)
    ));
    let mut bytes = bytes;
    let c = central(&bytes);
    let local = u32_at(&bytes, c + 42).unwrap();
    bytes[local + 30] ^= 1;
    rejected(&bytes);
}
fn rooted_archive(
    root: &str,
    reverse: bool,
    entries: &[(&str, &[u8])],
    outside: Option<&str>,
    root_entry: bool,
) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(if reverse {
            zip::CompressionMethod::Deflated
        } else {
            zip::CompressionMethod::Stored
        })
        .last_modified_time(
            zip::DateTime::from_date_and_time(if reverse { 2026 } else { 2025 }, 1, 1, 0, 0, 0)
                .unwrap(),
        );
    if root_entry {
        writer.add_directory(format!("{root}/"), options).unwrap();
    }
    let mut ordered = entries.to_vec();
    if reverse {
        ordered.reverse();
    }
    for (name, content) in ordered {
        writer
            .start_file(format!("{root}/{name}"), options)
            .unwrap();
        writer.write_all(content).unwrap();
    }
    if let Some(name) = outside {
        writer.start_file(name, options).unwrap();
        writer.write_all(b"outside").unwrap();
    }
    writer.finish().unwrap().into_inner()
}
#[test]
fn package_semantic_bound_roots_allow_only_expected_wrapper_rename_while_strict_fails() {
    let dir = tempfile::tempdir().unwrap();
    let entries = [
        ("Index/Document.iwa", b"document".as_slice()),
        ("Metadata/info", b"metadata".as_slice()),
    ];
    let (a, ar) = staged(
        &rooted_archive("Source.pages", false, &entries, None, true),
        dir.path(),
    );
    let (b, br) = staged(
        &rooted_archive("Import.pages", true, &entries, None, true),
        dir.path(),
    );
    assert!(compare_package_archives(&a, &ar, &b, &br, &CancellationToken::new()).is_err());
    let result = compare_package_archives_with_roots(
        &a,
        &ar,
        "Source.pages",
        &b,
        &br,
        "Import.pages",
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(result.entries, 3);
    assert_eq!(result.files, 2);
    assert_eq!(result.expanded_bytes, 16);
    assert!(!result.raw_archives_equal);
}
#[test]
fn package_semantic_bound_roots_refuse_wrong_or_unsafe_expected_root() {
    let dir = tempfile::tempdir().unwrap();
    let (file, receipt) = staged(
        &rooted_archive("Source.pages", false, &[("file", b"content")], None, true),
        dir.path(),
    );
    for root in [
        "Wrong.pages",
        "Source",
        "Source.pages/",
        "../Source.pages",
        "/Source.pages",
        "Source.pages/extra",
        "",
    ] {
        assert!(
            compare_package_archives_with_roots(
                &file,
                &receipt,
                root,
                &file,
                &receipt,
                "Source.pages",
                &CancellationToken::new()
            )
            .is_err()
        );
    }
}
#[test]
fn package_semantic_bound_roots_refuse_extra_root_or_unwrapped_file() {
    let dir = tempfile::tempdir().unwrap();
    let (source, source_receipt) = staged(
        &rooted_archive("Source.pages", false, &[("file", b"content")], None, true),
        dir.path(),
    );
    for outside in ["Other.pages/file", "unwrapped", "Import.pages-extra/file"] {
        let (imported, receipt) = staged(
            &rooted_archive(
                "Import.pages",
                false,
                &[("file", b"content")],
                Some(outside),
                true,
            ),
            dir.path(),
        );
        assert!(
            compare_package_archives_with_roots(
                &source,
                &source_receipt,
                "Source.pages",
                &imported,
                &receipt,
                "Import.pages",
                &CancellationToken::new()
            )
            .is_err()
        );
    }
}
#[test]
fn package_semantic_bound_roots_do_not_ignore_inner_paths_contents_or_root_entry() {
    let dir = tempfile::tempdir().unwrap();
    let (source, source_receipt) = staged(
        &rooted_archive(
            "Source.pages",
            false,
            &[("Nested/file", b"content")],
            None,
            true,
        ),
        dir.path(),
    );
    for (name, content, root_entry) in [
        ("file", b"content".as_slice(), true),
        ("Nested/file", b"changed", true),
        ("Nested/file", b"content", false),
    ] {
        let (imported, receipt) = staged(
            &rooted_archive("Import.pages", false, &[(name, content)], None, root_entry),
            dir.path(),
        );
        assert!(
            compare_package_archives_with_roots(
                &source,
                &source_receipt,
                "Source.pages",
                &imported,
                &receipt,
                "Import.pages",
                &CancellationToken::new()
            )
            .is_err()
        );
    }
}
#[test]
fn package_semantic_bound_roots_preserve_duplicate_and_traversal_rejection() {
    let dir = tempfile::tempdir().unwrap();
    let original = rooted_archive(
        "Source.pages",
        false,
        &[("Index/one", b"one"), ("Index/two", b"two")],
        None,
        true,
    );
    for (from, to) in [
        (
            b"Source.pages/Index/two".as_slice(),
            b"Source.pages/Index/one".as_slice(),
        ),
        (b"Source.pages/Index/one", b"Source.pages/../secret"),
    ] {
        let mut bytes = original.clone();
        replace_name(&mut bytes, from, to);
        let (file, receipt) = staged(&bytes, dir.path());
        assert!(
            compare_package_archives_with_roots(
                &file,
                &receipt,
                "Source.pages",
                &file,
                &receipt,
                "Source.pages",
                &CancellationToken::new()
            )
            .is_err()
        );
    }
}
#[test]
fn package_semantic_bound_root_must_be_a_directory_not_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "Source.pages",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"file").unwrap();
    let (file, receipt) = staged(&writer.finish().unwrap().into_inner(), dir.path());
    assert!(
        compare_package_archives_with_roots(
            &file,
            &receipt,
            "Source.pages",
            &file,
            &receipt,
            "Source.pages",
            &CancellationToken::new()
        )
        .is_err()
    );
}
#[test]
fn package_identity_is_stable_across_order_compression_timestamps_and_bound_root_rename() {
    let dir = tempfile::tempdir().unwrap();
    let entries = [
        ("Index/Document.iwa", b"document".as_slice()),
        ("Metadata/info", b"metadata".as_slice()),
    ];
    let (a, ar) = staged(
        &rooted_archive("Source.pages", false, &entries, None, true),
        dir.path(),
    );
    let (b, br) = staged(
        &rooted_archive("Imported.pages", true, &entries, None, true),
        dir.path(),
    );
    let first =
        package_archive_semantic_identity(&a, &ar, "Source.pages", &CancellationToken::new())
            .unwrap();
    let second =
        package_archive_semantic_identity(&b, &br, "Imported.pages", &CancellationToken::new())
            .unwrap();
    assert_eq!(first, second);
    assert_eq!(first.version, 1);
    assert_eq!(first.entries, 3);
    assert_eq!(first.files, 2);
    assert_eq!(first.expanded_bytes, 16);
    assert_ne!(ar.sha256, br.sha256);
    let encoded = serde_json::to_string(&first).unwrap();
    assert!(!encoded.contains("Source"));
    assert!(!encoded.contains("Document.iwa"));
    assert!(!encoded.contains("metadata"));
    assert_eq!(
        serde_json::from_str::<PackageSemanticIdentity>(&encoded).unwrap(),
        first
    );
}
#[test]
fn package_identity_changes_with_inner_path_or_content() {
    let dir = tempfile::tempdir().unwrap();
    let (source, receipt) = staged(
        &rooted_archive(
            "Source.pages",
            false,
            &[("Index/Document.iwa", b"document")],
            None,
            true,
        ),
        dir.path(),
    );
    let expected = package_archive_semantic_identity(
        &source,
        &receipt,
        "Source.pages",
        &CancellationToken::new(),
    )
    .unwrap();
    for (path, data) in [
        ("Index/Document.iwa", b"changed!".as_slice()),
        ("Index/Document2.iwa", b"document"),
    ] {
        let (file, receipt) = staged(
            &rooted_archive("Source.pages", false, &[(path, data)], None, true),
            dir.path(),
        );
        assert_ne!(
            package_archive_semantic_identity(
                &file,
                &receipt,
                "Source.pages",
                &CancellationToken::new()
            )
            .unwrap()
            .sha256,
            expected.sha256
        );
    }
}
fn kind_swap_archive(swap: bool) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.add_directory("Root.pages/", options).unwrap();
    zip.start_file("Root.pages/keep", options).unwrap();
    zip.write_all(b"data").unwrap();
    zip.add_directory(
        if swap {
            "Root.pages/a/"
        } else {
            "Root.pages/b/"
        },
        options,
    )
    .unwrap();
    zip.start_file(if swap { "Root.pages/b" } else { "Root.pages/a" }, options)
        .unwrap();
    zip.finish().unwrap().into_inner()
}
#[test]
fn package_identity_commits_entry_kind_even_when_paths_counts_sizes_and_hashes_match() {
    let dir = tempfile::tempdir().unwrap();
    let (a, ar) = staged(&kind_swap_archive(false), dir.path());
    let (b, br) = staged(&kind_swap_archive(true), dir.path());
    let first = package_archive_semantic_identity(&a, &ar, "Root.pages", &CancellationToken::new())
        .unwrap();
    let second =
        package_archive_semantic_identity(&b, &br, "Root.pages", &CancellationToken::new())
            .unwrap();
    assert_eq!(first.entries, second.entries);
    assert_eq!(first.files, second.files);
    assert_eq!(first.expanded_bytes, second.expanded_bytes);
    assert_ne!(first.sha256, second.sha256);
}
#[test]
fn package_identity_v1_framing_has_fixed_domain_version_lengths_and_binary_digests() {
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let test = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    let mut entries = BTreeMap::new();
    entries.insert(
        String::new(),
        Entry {
            directory: true,
            size: 0,
            sha256: empty.into(),
        },
    );
    entries.insert(
        "abc".into(),
        Entry {
            directory: false,
            size: 4,
            sha256: test.into(),
        },
    );
    let content = Fingerprint {
        entries,
        expanded: 4,
        files: 1,
    };
    // Independent literal v1 frame: version=1, entries=2, files=1, bytes=4;
    // empty root directory, then abc (four bytes). No encoder helper is used.
    let mut frame = b"cirrove.package.semantic.identity\0".to_vec();
    frame.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 4]);
    frame.extend_from_slice(&[b'D', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    frame.extend_from_slice(&hex::decode(empty).unwrap());
    frame.extend_from_slice(&[b'F', 0, 0, 0, 3, b'a', b'b', b'c', 0, 0, 0, 0, 0, 0, 0, 4]);
    frame.extend_from_slice(&hex::decode(test).unwrap());
    let cancel = CancellationToken::new();
    let digest = identity_digest(&content, IDENTITY_DOMAIN, 1, &cancel).unwrap();
    assert_eq!(digest, hex::encode(Sha256::digest(&frame)));
    assert_ne!(
        digest,
        identity_digest(&content, b"different-domain\0", 1, &cancel).unwrap()
    );
    assert_ne!(
        digest,
        identity_digest(&content, IDENTITY_DOMAIN, 2, &cancel).unwrap()
    );
}
#[test]
fn package_identity_rejects_unknown_versions_malformed_receipts_wrong_root_and_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let (file, mut receipt) = staged(
        &rooted_archive("Source.pages", false, &[("file", b"content")], None, true),
        dir.path(),
    );
    let cancel = CancellationToken::new();
    let identity =
        package_archive_semantic_identity(&file, &receipt, "Source.pages", &cancel).unwrap();
    let base = serde_json::to_value(&identity).unwrap();
    for (field, value) in [
        ("version", serde_json::json!(3)),
        ("sha256", serde_json::json!("not-a-digest")),
        ("entries", serde_json::json!(10001)),
        ("files", serde_json::json!(0)),
        ("expanded_bytes", serde_json::json!(MAX_EXPANDED + 1)),
    ] {
        let mut invalid = base.clone();
        invalid[field] = value;
        assert!(serde_json::from_value::<PackageSemanticIdentity>(invalid).is_err());
    }
    assert!(package_archive_semantic_identity(&file, &receipt, "Other.pages", &cancel).is_err());
    receipt.sha256 = "00".repeat(32);
    assert!(matches!(
        package_archive_semantic_identity(&file, &receipt, "Source.pages", &cancel),
        Err(ProviderError::VersionChanged)
    ));
    cancel.cancel();
    assert!(matches!(
        package_archive_semantic_identity(&file, &receipt, "Source.pages", &cancel),
        Err(ProviderError::Cancelled)
    ));
}

#[cfg(feature = "write-probe")]
#[test]
fn diagnostic_directory_delta_never_weakens_strict_semantic_identity() {
    let dir = tempfile::tempdir().unwrap();
    let source = rooted_archive(
        "Source.pages",
        false,
        &[("Metadata/private", b"fixed synthetic content")],
        None,
        false,
    );
    let (a, ar) = staged(&source, dir.path());
    for (root_entry, changed, expected_delta, expected_files) in [
        (true, false, true, true),
        (false, false, false, true),
        (true, true, false, false),
    ] {
        let bytes = rooted_archive(
            "Stage.pages",
            true,
            &[(
                "Metadata/private",
                if changed {
                    b"changed"
                } else {
                    b"fixed synthetic content"
                },
            )],
            None,
            root_entry,
        );
        let (b, br) = staged(&bytes, dir.path());
        let token = CancellationToken::new();
        let observation =
            diagnostic_archive_comparison(&a, &ar, "Source.pages", &b, &br, "Stage.pages", &token)
                .unwrap();
        assert_eq!(observation["directory_only_delta"], expected_delta);
        assert_eq!(
            observation["exact_file_paths_sizes_hashes_equal"],
            expected_files
        );
        assert_eq!(observation["source_directories"], 0);
        assert_eq!(
            observation["downloaded_directories"],
            usize::from(root_entry)
        );
        assert_eq!(
            compare_package_archives_with_roots(
                &a,
                &ar,
                "Source.pages",
                &b,
                &br,
                "Stage.pages",
                &token
            )
            .is_ok(),
            !root_entry && !changed
        );
        let printed = observation.to_string();
        assert!(
            !printed.contains("Metadata")
                && !printed.contains("synthetic")
                && !printed.contains("private")
        );
        assert!(
            diagnostic_archive_comparison(&a, &ar, "Wrong.pages", &b, &br, "Stage.pages", &token)
                .is_err()
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            diagnostic_archive_comparison(
                &a,
                &ar,
                "Source.pages",
                &b,
                &br,
                "Stage.pages",
                &cancelled
            )
            .is_err()
        );
    }
}

#[path = "v2_tests.rs"]
mod v2_tests;
