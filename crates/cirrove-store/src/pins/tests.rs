use crate::Store;

fn store() -> (tempfile::TempDir, Store) {
    let temp = tempfile::tempdir().expect("fixture");
    let store = Store::open(temp.path().join("metadata.db")).expect("store");
    (temp, store)
}
const GIB: u64 = 1 << 30;

#[test]
fn a_reservation_that_does_not_fit_is_refused_rather_than_accepted_and_evicted() {
    let (_temp, mut store) = store();
    let budget = 4 * GIB;
    assert!(
        store
            .pin("scope", "first", false, 3 * GIB, budget)
            .expect("pin")
            .is_ok()
    );
    // The point of a reservation is that the second caller is told now. Accepting
    // this and letting eviction sort it out later is the behaviour being ruled out.
    let refusal = store
        .pin("scope", "second", false, 2 * GIB, budget)
        .expect("pin")
        .expect_err("second pin must not fit");
    assert_eq!(
        refusal,
        crate::pins::PinRefusal::WouldExceedBudget {
            requested: 2 * GIB,
            available: GIB,
        }
    );
    assert_eq!(store.reserved_bytes().expect("reserved"), 3 * GIB);
    assert_eq!(store.pins().expect("pins").len(), 1);
}
#[test]
fn raising_a_pin_counts_only_the_difference_against_the_budget() {
    let (_temp, mut store) = store();
    let budget = 4 * GIB;
    store
        .pin("scope", "item", false, GIB, budget)
        .expect("pin")
        .expect("fits");
    // Its own current reservation must not be counted against it, or growing a
    // pin would need room for both the old and the new size at once.
    store
        .pin("scope", "item", true, 4 * GIB, budget)
        .expect("pin")
        .expect("raising to the whole budget fits, because the old GiB is released");
    assert_eq!(store.reserved_bytes().expect("reserved"), 4 * GIB);
    let pins = store.pins().expect("pins");
    assert_eq!(pins.len(), 1, "re-pinning replaces rather than duplicates");
    assert!(pins[0].recursive, "the new mode replaces the old");
}
#[test]
fn a_pin_and_its_protected_blocks_survive_reopening_the_store() {
    let temp = tempfile::tempdir().expect("fixture");
    let path = temp.path().join("metadata.db");
    {
        let mut store = Store::open(&path).expect("store");
        store
            .pin("scope", "item", true, GIB, 4 * GIB)
            .expect("pin")
            .expect("fits");
        store
            .protect_blocks("scope", "item", &["aa".into(), "bb".into()])
            .expect("protect");
    }
    // A pin is an instruction, not derived state. Losing it across a restart
    // would take content offline that somebody asked to keep.
    let store = Store::open(&path).expect("reopen");
    assert_eq!(store.reserved_bytes().expect("reserved"), GIB);
    assert_eq!(store.protected_blocks().expect("blocks").len(), 2);
    assert!(store.pins().expect("pins")[0].recursive);
}
#[test]
fn unpinning_releases_both_the_reservation_and_the_protection() {
    let (_temp, mut store) = store();
    store
        .pin("scope", "item", false, GIB, 4 * GIB)
        .expect("pin")
        .expect("fits");
    store
        .protect_blocks("scope", "item", &["aa".into()])
        .expect("protect");
    assert!(store.unpin("scope", "item").expect("unpin"));
    assert_eq!(store.reserved_bytes().expect("reserved"), 0);
    assert!(
        store.protected_blocks().expect("blocks").is_empty(),
        "a released pin must stop protecting its blocks, or the cache keeps paying for it"
    );
    assert!(
        !store.unpin("scope", "item").expect("unpin"),
        "unpinning what is not pinned reports that rather than pretending"
    );
}
