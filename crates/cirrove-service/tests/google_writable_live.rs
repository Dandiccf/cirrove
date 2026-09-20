//! Explicit live conflict arm for the bounded writable-Google mount benchmark.
//! Ignored by default. It mutates only the exact item supplied by the operator.
#![allow(clippy::unwrap_used)]

use cirrove_core::mutation::{MutationIntent, MutationProvider, MutationReceipt, MutationRequest};
use cirrove_core::{CancellationToken, ReadProvider, Scope};
use cirrove_service::accounts::{Settings, google_provider};
use sha2::{Digest, Sha256};

fn live_account() -> (cirrove_service::accounts::Account, String) {
    let state = std::path::PathBuf::from(std::env::var_os("CIRROVE_GOOGLE_LIVE_STATE").unwrap());
    let label = std::env::var("CIRROVE_GOOGLE_LIVE_LABEL").unwrap();
    let account = Settings::load(&state)
        .unwrap()
        .accounts
        .into_iter()
        .find(|account| account.label == label)
        .unwrap();
    (account, std::env::var("CIRROVE_GOOGLE_LIVE_ITEM").unwrap())
}

#[tokio::test]
#[ignore = "requires explicit live Google mutation authorization and exact fixture identity"]
async fn rename_the_exact_run_owned_item_outside_the_mount() {
    let (account, item) = live_account();
    let name = std::env::var("CIRROVE_GOOGLE_LIVE_NAME").unwrap();
    assert_eq!(account.access, cirrove_auth::AccessMode::ReadWrite);
    let provider = google_provider(&account).unwrap();
    let scope = Scope {
        account: account.id,
        provider: "googledrive".into(),
        collection: account.drive.id,
    };
    let cancel = CancellationToken::new();
    let before = provider.node(&scope, &item, &cancel).await.unwrap();
    let request = MutationRequest {
        scope,
        intent: MutationIntent::Relocate {
            parent: before.parent_id.clone().unwrap(),
            before,
            name: name.clone(),
        },
    };
    let receipt = provider.mutate(&request, &cancel).await.unwrap();
    assert!(request.accepts(&receipt));
    let MutationReceipt::Upsert(node) = receipt else {
        panic!("rename returned a removal receipt")
    };
    assert_eq!(node.name, name);
}

#[tokio::test]
#[ignore = "requires explicit live Google readback of the exact run-owned fixture"]
async fn exact_remote_bytes_match_the_registered_digest() {
    let (account, item) = live_account();
    let expected = std::env::var("CIRROVE_GOOGLE_LIVE_SHA256").unwrap();
    let provider = google_provider(&account).unwrap();
    let scope = Scope {
        account: account.id,
        provider: "googledrive".into(),
        collection: account.drive.id,
    };
    let cancel = CancellationToken::new();
    let node = provider.node(&scope, &item, &cancel).await.unwrap();
    let mut digest = Sha256::new();
    let mut offset = 0;
    while offset < node.size {
        let length = (node.size - offset).min(4 * 1024 * 1024) as u32;
        let bytes = provider
            .read_range(&scope, &node, offset, length, &cancel)
            .await
            .unwrap();
        assert!(!bytes.is_empty(), "Google read ended before the exact item");
        offset += bytes.len() as u64;
        digest.update(bytes);
    }
    assert_eq!(hex::encode(digest.finalize()), expected);
}

#[tokio::test]
#[ignore = "requires explicit live Google mutation authorization and exact run-owned folder identity"]
async fn trash_only_the_exact_run_owned_tree() {
    let (account, root_item) = live_account();
    let expected_name = std::env::var("CIRROVE_GOOGLE_LIVE_NAME").unwrap();
    let provider = google_provider(&account).unwrap();
    let scope = Scope {
        account: account.id,
        provider: "googledrive".into(),
        collection: account.drive.id,
    };
    let cancel = CancellationToken::new();
    let root = provider.node(&scope, &root_item, &cancel).await.unwrap();
    assert!(
        root.name == expected_name || root.name == format!("{expected_name} [{root_item}]"),
        "the exact item is not the registered run-owned folder"
    );
    assert_eq!(root.kind, cirrove_core::NodeKind::Folder);

    let mut folders = vec![root];
    let mut files = Vec::new();
    let mut next_folder = 0;
    while next_folder < folders.len() {
        let parent = folders[next_folder].id.clone();
        next_folder += 1;
        let mut cursor = None;
        loop {
            let page = provider
                .children(&scope, &parent, cursor.as_ref(), &cancel)
                .await
                .unwrap();
            for node in page.nodes {
                match node.kind {
                    cirrove_core::NodeKind::Folder => folders.push(node),
                    cirrove_core::NodeKind::File => files.push(node),
                    cirrove_core::NodeKind::Shortcut => {
                        panic!("the run-owned binary fixture unexpectedly contains a shortcut")
                    }
                }
            }
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert!(
            folders.len() + files.len() <= 128,
            "fixture boundary exceeded"
        );
    }

    for file in files {
        let before = provider.node(&scope, &file.id, &cancel).await.unwrap();
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::RemoveFile { before },
        };
        let receipt = provider.mutate(&request, &cancel).await.unwrap();
        assert!(request.accepts(&receipt));
    }
    for folder in folders.into_iter().rev() {
        let before = provider.node(&scope, &folder.id, &cancel).await.unwrap();
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::RemoveFolder { before },
        };
        let receipt = provider.mutate(&request, &cancel).await.unwrap();
        assert!(request.accepts(&receipt));
    }
}
