#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::upload::{UploadIntent, UploadRepresentation};

#[test]
fn native_final_flat_receipt_rejects_representation_revision_and_identity_substitution() {
    let original = Node {
        id: "FILE::com.apple.CloudDocs::original".into(),
        parent_id: Some("FOLDER::com.apple.CloudDocs::owned".into()),
        name: "Owned.numbers".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 0,
        etag: Some("original-v1".into()),
        content_version: None,
        target: None,
        package: true,
    };
    let semantic = PackageSemanticIdentity {
        version: 2,
        sha256: "a".repeat(64),
        entries: 2,
        files: 1,
        expanded_bytes: 17,
    };
    let old_semantic = PackageSemanticIdentity {
        sha256: "b".repeat(64),
        ..semantic.clone()
    };
    let request = UploadRequest {
        scope: Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        },
        intent: UploadIntent::Replace {
            item: original.id.clone(),
            expected_etag: original.etag.clone().unwrap(),
        },
        size: 100,
        sha256: "c".repeat(64),
        representation: UploadRepresentation::FlatNumbersReplacementArchive {
            semantic: semantic.clone(),
            original: Box::new(original.clone()),
            original_semantic: old_semantic.clone(),
        },
    };
    request.validate().unwrap();
    let receipt = Receipt {
        original: original.clone(),
        current: Node {
            id: "FILE::com.apple.CloudDocs::current".into(),
            etag: Some("current-v1".into()),
            ..original.clone()
        },
        backup: Node {
            parent_id: Some(TRASH.into()),
            etag: Some("trash-v1".into()),
            ..original
        },
        current_semantic: semantic.clone(),
        backup_semantic: old_semantic,
    };
    receipt.check(&request).unwrap();
    for arm in 0..9 {
        let mut changed = receipt.clone();
        match arm {
            0 => changed.original.etag = Some("changed-original".into()),
            1 => changed.current.id = changed.original.id.clone(),
            2 => changed.current.parent_id = Some("FOLDER::com.apple.CloudDocs::foreign".into()),
            3 => changed.backup.id = "FILE::com.apple.CloudDocs::foreign".into(),
            4 => changed.backup.parent_id = changed.original.parent_id.clone(),
            5 => changed.backup.size += 1,
            6 => changed.current_semantic.sha256 = "d".repeat(64),
            7 => changed.backup_semantic.sha256 = "e".repeat(64),
            _ => changed.current.content_version = Some("unbound-content-version".into()),
        }
        assert!(
            changed.check(&request).is_err(),
            "receipt substitution arm {arm}"
        );
    }
    for representation in [
        UploadRepresentation::FileBytes,
        UploadRepresentation::FlatNumbersArchive {
            semantic: semantic.clone(),
        },
        UploadRepresentation::PackageArchive {
            expected_root: "Source.numbers".into(),
            semantic,
        },
    ] {
        let changed = UploadRequest {
            representation,
            ..request.clone()
        };
        assert!(receipt.check(&changed).is_err());
    }
}
