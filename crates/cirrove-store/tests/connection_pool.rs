//! `cirrove-service` opens a store once per operation, at 58 call sites. What
//! that costs does not show up in any one of them: SQLite reads and re-parses
//! the whole schema on a connection's first statement and prepares every
//! statement again. Sampling the daemon on 2026-09-16, while a desktop file
//! indexer walked the mount, put thirteen of sixteen computing stacks in
//! opening, parsing and closing a connection rather than in the work asked for.
//!
//! This test lives alone in its own binary on purpose: the counter it reads is
//! process-wide, so a second test building connections beside it would make the
//! measurement mean nothing.
#![allow(clippy::unwrap_used)]
use cirrove_store::Store;

const OPENS: u64 = 200;

#[test]
fn opening_a_store_again_reuses_the_connection_instead_of_building_one() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    // One store stays alive for the whole loop, as `Engine` keeps one for an
    // account's lifetime. Idle connections exist only while a store does: the
    // last one to drop closes them, because before pooling the last store was
    // the last connection, and SQLite retires the write-ahead log when the last
    // connection closes. A pool that outlived its stores would leave a log
    // beside a cleanly closed account.
    let keeper = Store::open(&path).unwrap();

    let before = cirrove_store::connections_built();
    for _ in 0..OPENS {
        let db = Store::open(&path).unwrap();
        // Ask it something, so this measures a connection that gets used and
        // not one the compiler could have reasoned away.
        assert_eq!(db.counts().unwrap().0, 0);
        drop(db);
    }
    let built = cirrove_store::connections_built() - before;
    drop(keeper);
    // Exactly one: the first open finds nothing idle, because the keeper is
    // using its own connection rather than resting it. Every open after that
    // reuses the one the open before it gave back.
    assert!(
        built <= 1,
        "{OPENS} opens of the same database built {built} connections; each one \
         re-reads and re-parses the whole schema, which is what a pool exists to \
         stop"
    );
}
