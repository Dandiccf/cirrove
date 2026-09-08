#![allow(clippy::unwrap_used)]
use super::*;

#[test]
fn entries_are_ordered_by_age_and_survive_reopening() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("blocks.db");
    {
        let mut index = BlockIndex::open(&path).unwrap();
        for (key, size) in [("a", 10u64), ("b", 20), ("c", 30)] {
            index.touch(key, size).unwrap();
        }
        assert_eq!(
            index.oldest().unwrap(),
            vec![
                ("a".to_owned(), 10),
                ("b".to_owned(), 20),
                ("c".to_owned(), 30)
            ]
        );
        // Touching again moves an entry to the young end and updates its size.
        index.touch("a", 11).unwrap();
        assert_eq!(
            index.oldest().unwrap(),
            vec![
                ("b".to_owned(), 20),
                ("c".to_owned(), 30),
                ("a".to_owned(), 11)
            ]
        );
        index.forget("b").unwrap();
    }
    let index = BlockIndex::open(&path).unwrap();
    assert_eq!(
        index.oldest().unwrap(),
        vec![("c".to_owned(), 30), ("a".to_owned(), 11)]
    );
}

#[test]
fn the_index_lives_apart_from_any_metadata_database() {
    let temp = tempfile::tempdir().unwrap();
    let metadata = temp.path().join("metadata.db");
    let blocks = temp.path().join("blocks.db");
    let store = crate::Store::open(&metadata).unwrap();
    let mut index = BlockIndex::open(&blocks).unwrap();
    index.touch("k", 1).unwrap();
    // A commit here must not touch the metadata database at all, which is what
    // keeps a navigation reader's page cache intact during a download.
    assert!(temp.path().join("blocks.db-wal").exists());
    assert_eq!(index.oldest().unwrap().len(), 1);
    drop(store);
    assert_eq!(index.oldest().unwrap().len(), 1);
}
