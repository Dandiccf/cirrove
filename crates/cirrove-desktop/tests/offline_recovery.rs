//! Real local journal/selection/export tests; no daemon or credentials.
#![allow(clippy::unwrap_used)]
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use cirrove_desktop::recovery::offline::{self, Cursor, Selection};
use cirrove_service::{
    accounts::Settings,
    journal::{UploadIntent, UploadJournal},
};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
struct Fixture {
    _temp: tempfile::TempDir,
    state: PathBuf,
    id: String,
}
fn fixture() -> Fixture {
    fixture_with_saves(1)
}
fn fixture_with_saves(count: usize) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    cirrove_service::private_dir(&state).unwrap();
    let mut settings: Settings =
        serde_json::from_str(include_str!("../fixtures/accounts.json")).unwrap();
    settings.accounts.truncate(1);
    let account = &mut settings.accounts[0];
    account.enabled = false;
    account.mount_path = temp.path().join("mount");
    let id = account.id.clone();
    let scope = Scope {
        account: id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let mut journal = UploadJournal::open(
        &state.join("accounts").join(&id).join("journal"),
        &id,
        1048576,
    )
    .unwrap();
    for _ in 0..count {
        journal
            .enqueue(
                scope.clone(),
                UploadIntent::Create {
                    parent: "root".into(),
                    name: "Saved.txt".into(),
                },
                &b"saved"[..],
            )
            .unwrap();
    }
    let node = Node {
        id: "local".into(),
        parent_id: Some("root".into()),
        name: "Draft.txt".into(),
        kind: NodeKind::File,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    };
    let working = journal.create_working(scope, node, true, &b""[..]).unwrap();
    journal.write_working(working.id, 0, b"draft").unwrap();
    drop(journal);
    fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    fs::set_permissions(
        state.join("accounts.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    Fixture {
        _temp: temp,
        state,
        id,
    }
}
#[test]
fn offline_selection_and_real_exports_preserve_both_source_kinds() {
    let f = fixture();
    let page = offline::load(&f.state, "work", &f.id, Cursor::default()).unwrap();
    assert_eq!(page.selections.len(), 2);
    for (index, selection) in page.selections.iter().enumerate() {
        let destination = f._temp.path().join(format!("copy-{index}"));
        offline::copy(
            &f.state,
            "work",
            &f.id,
            selection,
            &destination,
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();
        assert_eq!(
            fs::read(destination).unwrap(),
            match selection {
                Selection::Saved(_) => b"saved",
                Selection::Working(_) => b"draft",
            }
        );
    }
    assert!(
        page.next.is_none(),
        "a complete short page must not offer an empty next page"
    );
    assert!(offline::load(&f.state, "work", "another-account", Cursor::default()).is_err());
    let destination = f._temp.path().join("foreign");
    assert!(
        offline::copy(
            &f.state,
            "work",
            "another-account",
            &page.selections[0],
            &destination,
            &CancellationToken::new(),
            |_| {}
        )
        .is_err()
    );
    assert!(!destination.exists());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        offline::copy(
            &f.state,
            "work",
            &f.id,
            &page.selections[0],
            &destination,
            &cancelled,
            |_| {}
        )
        .is_err()
    );
    assert!(!destination.exists());
}

#[test]
fn next_page_does_not_omit_or_repeat_either_source_kind() {
    let f = fixture_with_saves(201);
    let page = offline::load(&f.state, "work", &f.id, Cursor::default()).unwrap();
    assert_eq!(page.selections.len(), 201);
    let next = offline::load(&f.state, "work", &f.id, page.next.unwrap()).unwrap();
    assert_eq!(next.selections.len(), 1);
    assert!(next.next.is_none());
    let mut ids = std::collections::HashSet::new();
    for s in page.selections.into_iter().chain(next.selections) {
        match s {
            Selection::Saved(s) => assert!(ids.insert(s.operation.unwrap())),
            Selection::Working(_) => (),
        }
    }
    assert_eq!(ids.len(), 201);
}
