use super::*;

fn zip_entries(
    root: &str,
    directories: &[&str],
    files: &[(&str, &[u8])],
    deflate: bool,
) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(if deflate {
            zip::CompressionMethod::Deflated
        } else {
            zip::CompressionMethod::Stored
        })
        .last_modified_time(
            zip::DateTime::from_date_and_time(if deflate { 2024 } else { 2020 }, 1, 1, 0, 0, 0)
                .unwrap(),
        );
    for (path, bytes) in files {
        zip.start_file(format!("{root}/{path}"), options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    for path in directories {
        zip.add_directory(
            if path.is_empty() {
                format!("{root}/")
            } else {
                format!("{root}/{path}/")
            },
            options,
        )
        .unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn identity(bytes: &[u8], root: &str, version: u32) -> PackageSemanticIdentity {
    let dir = tempfile::tempdir().unwrap();
    let (file, receipt) = staged(bytes, dir.path());
    package_archive_semantic_identity_versioned(
        &file,
        &receipt,
        root,
        version,
        &CancellationToken::new(),
    )
    .unwrap()
}
#[test]
fn v2_directory_closure_matches_repacked_archive_but_v1_stays_strict() {
    let a = zip_entries(
        "Source.pages",
        &[],
        &[("Index/a", b"one"), ("Metadata/b", b"two")],
        false,
    );
    let b = zip_entries(
        "Imported.pages",
        &["", "Metadata", "Index"],
        &[("Metadata/b", b"two"), ("Index/a", b"one")],
        true,
    );
    let first = identity(&a, "Source.pages", 2);
    assert_eq!(first, identity(&b, "Imported.pages", 2));
    assert_eq!(
        (first.entries, first.files, first.expanded_bytes),
        (5, 2, 6)
    );
    assert_ne!(
        identity(&a, "Source.pages", 1),
        identity(&b, "Imported.pages", 1)
    );
    let dir = tempfile::tempdir().unwrap();
    let (file, receipt) = staged(&a, dir.path());
    assert_eq!(
        package_archive_semantic_identity(
            &file,
            &receipt,
            "Source.pages",
            &CancellationToken::new()
        )
        .unwrap(),
        identity(&a, "Source.pages", 1)
    );
    let (other, other_receipt) = staged(&b, dir.path());
    assert!(
        compare_package_archives_with_roots(
            &file,
            &receipt,
            "Source.pages",
            &other,
            &other_receipt,
            "Imported.pages",
            &CancellationToken::new()
        )
        .is_err()
    );
}
#[test]
fn v2_preserves_empty_directories_and_exact_paths_content_and_kinds() {
    let baseline = identity(
        &zip_entries(
            "R.pages",
            &["empty/deep"],
            &[("Index/file", b"body")],
            false,
        ),
        "R.pages",
        2,
    );
    for archive in [
        zip_entries("R.pages", &[], &[("Index/file", b"body")], false),
        zip_entries(
            "R.pages",
            &["empty/deep"],
            &[("Index/other", b"body")],
            false,
        ),
        zip_entries(
            "R.pages",
            &["empty/deep"],
            &[("Index/file", b"edit")],
            false,
        ),
        zip_entries(
            "R.pages",
            &["empty"],
            &[("Index/file", b"body"), ("empty/deep", b"")],
            false,
        ),
    ] {
        assert_ne!(baseline, identity(&archive, "R.pages", 2));
    }
    assert_eq!(
        baseline,
        identity(
            &zip_entries(
                "R.pages",
                &["", "empty", "empty/deep", "Index"],
                &[("Index/file", b"body")],
                true
            ),
            "R.pages",
            2
        )
    );
}
#[test]
fn v2_refuses_file_ancestors_and_wrong_root_without_disclosing_paths() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = zip_entries(
        "Private.pages",
        &[],
        &[("private", b"body"), ("private/child", b"other")],
        false,
    );
    let (file, receipt) = staged(&bytes, dir.path());
    // The existing ZIP layout check already rejects file ancestors. Both
    // semantic versions must retain that refusal.
    assert!(
        package_archive_semantic_identity_versioned(
            &file,
            &receipt,
            "Private.pages",
            1,
            &CancellationToken::new()
        )
        .is_err()
    );
    let error = package_archive_semantic_identity_versioned(
        &file,
        &receipt,
        "Private.pages",
        2,
        &CancellationToken::new(),
    )
    .err()
    .unwrap();
    assert!(!error.to_string().contains("private"));
    assert!(!error.to_string().contains("body"));
    let valid = zip_entries("Private.pages", &[], &[("file", b"body")], false);
    let (file, receipt) = staged(&valid, dir.path());
    assert!(
        package_archive_semantic_identity_versioned(
            &file,
            &receipt,
            "Other.pages",
            2,
            &CancellationToken::new()
        )
        .is_err()
    );
}
#[test]
fn v2_canonical_budget_counts_implied_directories_and_checks_cancellation() {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for n in 0..5000 {
        zip.start_file(
            format!("R.pages/d{n}/file"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"x").unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let (file, receipt) = staged(&zip.finish().unwrap().into_inner(), dir.path());
    assert_eq!(
        package_archive_semantic_identity_versioned(
            &file,
            &receipt,
            "R.pages",
            1,
            &CancellationToken::new()
        )
        .unwrap()
        .entries,
        5000
    );
    let error = package_archive_semantic_identity_versioned(
        &file,
        &receipt,
        "R.pages",
        2,
        &CancellationToken::new(),
    )
    .err()
    .unwrap();
    assert_eq!(
        error.to_string(),
        ProviderError::Protocol("package canonical entry limit exceeded").to_string()
    );
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        directory_closure(
            Fingerprint {
                entries: BTreeMap::new(),
                expanded: 0,
                files: 0
            },
            &token
        ),
        Err(ProviderError::Cancelled)
    ));
}
#[test]
fn v2_is_explicit_version_bound_and_unknown_versions_stay_closed() {
    let bytes = zip_entries("R.pages", &["", "Index"], &[("Index/file", b"body")], false);
    let first = identity(&bytes, "R.pages", 1);
    let second = identity(&bytes, "R.pages", 2);
    assert_eq!(
        (first.entries, first.files, first.expanded_bytes),
        (second.entries, second.files, second.expanded_bytes)
    );
    assert_ne!(first.sha256, second.sha256);
    assert_eq!(PACKAGE_SEMANTIC_IDENTITY_VERSION, 1);
    for expected in [first, second] {
        assert_eq!(
            serde_json::from_value::<PackageSemanticIdentity>(
                serde_json::to_value(&expected).unwrap()
            )
            .unwrap(),
            expected
        );
        for version in [0, 3, u32::MAX] {
            let mut invalid = serde_json::to_value(&expected).unwrap();
            invalid["version"] = serde_json::json!(version);
            assert!(serde_json::from_value::<PackageSemanticIdentity>(invalid).is_err());
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (file, receipt) = staged(&bytes, dir.path());
    assert!(
        package_archive_semantic_identity_versioned(
            &file,
            &receipt,
            "R.pages",
            3,
            &CancellationToken::new()
        )
        .is_err()
    );
}

#[test]
fn v2_never_discards_extra_roots_or_traversal() {
    for extra in ["Other.pages/file", "R.pages/../outside", "R.pages//file"] {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for path in ["R.pages/file", extra] {
            zip.start_file(path, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"body").unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let (file, receipt) = staged(&zip.finish().unwrap().into_inner(), dir.path());
        assert!(
            package_archive_semantic_identity_versioned(
                &file,
                &receipt,
                "R.pages",
                2,
                &CancellationToken::new()
            )
            .is_err()
        );
    }
}
