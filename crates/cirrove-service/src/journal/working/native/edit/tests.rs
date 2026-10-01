#![allow(clippy::unwrap_used)]
use super::super::tests::{binding, bytes, journal, publish, temp};
#[test]
fn native_path_selection_binds_account_original_and_derived_path() {
    let t = temp();
    let data = bytes(b"original");
    let bound = binding(&t, &data);
    let mut j = journal(&t);
    let selection = j
        .select_native_edit(
            &bound.scope,
            &bound.source,
            &bound.archive,
            Some(&bound.archive),
        )
        .unwrap();
    assert!(selection.working.is_none());
    selection.recheck(&j).unwrap();
    let file = publish(&mut j, bound.clone(), &data);
    assert!(selection.recheck(&j).is_err());
    let child = j.namespace_object(file.id).unwrap();
    let owner = j
        .namespace_object(child.native_archive.as_ref().unwrap().source_owner)
        .unwrap();
    let selected = j
        .select_native_edit(&bound.scope, &owner.node, &child.node, Some(&child.node))
        .unwrap();
    assert_eq!(selected.working, Some(file.id));
    let mut foreign = bound.scope.clone();
    foreign.account = "foreign".into();
    assert!(
        j.select_native_edit(&foreign, &owner.node, &child.node, None)
            .is_err()
    );
    let mut stale = child.node.clone();
    stale.name = "Other.pages".into();
    assert!(
        j.select_native_edit(&bound.scope, &owner.node, &child.node, Some(&stale))
            .is_err()
    );
    let mut revision = bound.source.clone();
    revision.etag = Some("unseen".into());
    assert!(
        j.select_native_edit(&bound.scope, &revision, &bound.archive, None)
            .is_err()
    );
    j.truncate_working(file.id, 0).unwrap();
    assert!(
        selected.recheck(&j).is_err(),
        "selection crossed local generation change"
    );
}
