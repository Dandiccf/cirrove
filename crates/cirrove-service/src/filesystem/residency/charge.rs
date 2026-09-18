//! `resident_bytes.evictable`, to the charge formula ADR 0005 wrote first.
//!
//! The formula was recorded in September, before this existed, so that a
//! counter disagreeing with measurement is a falsified model rather than a
//! formula quietly adjusted until it agrees. It is followed here to the letter,
//! including the two places where the view changed under it (ADR 0014) and the
//! amendment is named rather than absorbed.
//!
//! **Charge each distinct allocation once, not each view that can see it.**
//! `scope`, `alias` and `ancestry` are deliberately shared between siblings, and
//! `residency` is shared differently -- every child holds its parent's through
//! `_parent_residency`. Crediting the first view to insert one and debiting the
//! same value on removal would drift below truth by roughly what the interning
//! saves, and a bound that undercounts is not a bound.
//!
//! **The walk must not allocate while it runs.** A pre-reserved `Vec`, sorted
//! and deduplicated, never a `BTreeSet`: 750,438 views times its payloads is
//! millions of node allocations into the very arenas being measured, which
//! permanently raises `peak_rss_kib`.
//!
//! `#[cfg(test)]` and never on a filesystem path. It takes the namespace lock in
//! front of a deliberately single-threaded dispatcher, so it is head-of-line
//! blocking on the lookup path while it runs -- which is why every sample
//! records the walk's own duration rather than averaging it away.
use super::*;
use cirrove_core::{Node, Scope};

/// What one allocation costs the process, not what Rust asked for.
///
/// glibc adds an eight-byte header and rounds to sixteen, with a thirty-two
/// byte minimum chunk. The comparison this model has to survive is against
/// `Pss_Anon`, which is chunks and not requests, so the rounding is part of the
/// formula rather than a detail.
fn chunk(requested: usize) -> u64 {
    if requested == 0 {
        return 0;
    }
    (requested + 8).next_multiple_of(16).max(32) as u64
}

/// An `Arc<T>`'s own allocation: two atomic counters and the value.
fn arc_of<T>() -> usize {
    2 * size_of::<usize>() + size_of::<T>()
}

/// A `String`'s heap is its CAPACITY, not its length. Charging the length
/// undercharges by whatever slack the last growth left, and a bound that
/// undercounts is not a bound.
fn string(value: &String) -> u64 {
    chunk(value.capacity())
}

fn optional(value: Option<&String>) -> u64 {
    value.map_or(0, string)
}

/// An `Arc<str>` carries the two counters in the same allocation as the bytes,
/// which a `String` does not. Charging it as a bare string loses sixteen bytes
/// per view twice over -- the id and the name.
fn arc_str(value: &Arc<str>) -> u64 {
    chunk(2 * size_of::<usize>() + value.len())
}

fn scope_bytes(scope: &Scope) -> u64 {
    chunk(arc_of::<Scope>())
        + string(&scope.account)
        + string(&scope.provider)
        + string(&scope.collection)
}

fn node_bytes(node: &Node) -> u64 {
    chunk(arc_of::<Node>())
        + string(&node.id)
        + optional(node.parent_id.as_ref())
        + string(&node.name)
        + optional(node.etag.as_ref())
        + optional(node.content_version.as_ref())
        + node.target.as_ref().map_or(0, |target| {
            chunk(size_of::<cirrove_core::RemoteRef>())
                + string(&target.collection)
                + string(&target.item)
        })
}

fn route_bytes(route: &Vec<(String, String)>) -> u64 {
    chunk(arc_of::<Vec<(String, String)>>())
        + chunk(route.capacity() * size_of::<(String, String)>())
        + route
            .iter()
            .map(|(left, right)| string(left) + string(right))
            .sum::<u64>()
}

pub(in crate::filesystem) struct Charge {
    /// Deduplicated allocation bytes the view index holds.
    pub evictable: u64,
    /// What the `entries` map itself costs, inline and counted separately: it
    /// is not an `Arc` and cannot be shared, so it is charged per view.
    pub inline: u64,
    /// What charging every reference instead of every allocation would have
    /// said. Reported so the interning is visible rather than assumed.
    pub undeduplicated: u64,
    /// Distinct allocations charged, after deduplication.
    pub allocations: usize,
    /// Payload references examined, before it.
    pub examined: usize,
    /// How long the walk held the namespace lock.
    pub micros: u64,
}

impl NamespaceViews {
    /// Reserve `scratch` for a walk of this index. Called before the lock is
    /// taken, because reserving inside the walk is the allocation the formula
    /// forbids.
    pub(in crate::filesystem) fn charge_scratch(&self) -> Vec<(usize, u64)> {
        // Nine payload slots a view can carry, plus room for the index to grow
        // between this call and the walk.
        Vec::with_capacity(self.entries.len().saturating_mul(9) + 4096)
    }

    /// Walk the index and charge each distinct allocation once.
    pub(in crate::filesystem) fn charge(&self, scratch: &mut Vec<(usize, u64)>) -> Charge {
        let started = std::time::Instant::now();
        scratch.clear();
        let mut push = |pointer: *const (), bytes: u64| scratch.push((pointer as usize, bytes));
        for entry in self.entries.values() {
            let view = &entry.view;
            push(
                Arc::as_ptr(&view.residency).cast(),
                chunk(arc_of::<LookupRefs>()),
            );
            if let Some(parent) = &view._parent_residency {
                push(Arc::as_ptr(parent).cast(), chunk(arc_of::<LookupRefs>()));
            }
            push(Arc::as_ptr(&view.scope).cast(), scope_bytes(&view.scope));
            push(Arc::as_ptr(&view.id).cast(), arc_str(&view.id));
            push(Arc::as_ptr(&view.name).cast(), arc_str(&view.name));
            push(Arc::as_ptr(&view.alias).cast(), route_bytes(&view.alias));
            push(
                Arc::as_ptr(&view.ancestry).cast(),
                route_bytes(&view.ancestry),
            );
            if let Some(node) = &view.node {
                push(Arc::as_ptr(node).cast(), node_bytes(node));
            }
            if let Some(entry) = &view.entry {
                push(Arc::as_ptr(entry).cast(), node_bytes(entry));
            }
        }
        let examined = scratch.len();
        let undeduplicated = scratch.iter().map(|(_, bytes)| bytes).sum();
        scratch.sort_unstable_by_key(|(pointer, _)| *pointer);
        scratch.dedup_by_key(|(pointer, _)| *pointer);
        let evictable = scratch.iter().map(|(_, bytes)| bytes).sum();
        Charge {
            evictable,
            undeduplicated,
            // A `BTreeMap` node holds eleven keys and eleven values with its own
            // header; charging the entry and its key with the node's typical
            // fill is the closest this can get without reaching inside std.
            inline: (self.entries.len() as u64) * (size_of::<Entry>() + size_of::<u64>()) as u64,
            allocations: scratch.len(),
            examined,
            micros: started.elapsed().as_micros() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::super::tests::{cache, view};
    use super::*;
    use cirrove_core::NodeKind;

    #[test]
    fn a_shared_payload_is_charged_once_however_many_views_can_see_it() {
        let mut cache = cache();
        let first = cache.insert(view(2, NodeKind::File)).unwrap();
        // A sibling built from the first shares scope, alias and ancestry, which
        // is what `ProjectionIndex::matches` exists to do.
        let mut sibling = first.clone();
        sibling.inode = 3;
        sibling.residency = Arc::default();
        cache.insert(sibling).unwrap();

        let mut scratch = cache.charge_scratch();
        let charge = cache.charge(&mut scratch);
        assert_eq!(cache.len(), 3);
        assert!(
            charge.examined > charge.allocations,
            "nothing was shared, so this fixture cannot prove deduplication: \
             {} references over {} allocations",
            charge.examined,
            charge.allocations
        );
        // Charging every reference instead of every allocation is the mistake
        // the formula was written to forbid. It reads HIGH, never low, which is
        // the safe direction for a bound and the wrong one for a measurement.
        assert!(
            charge.undeduplicated > charge.evictable,
            "deduplication changed nothing: {} against {}",
            charge.undeduplicated,
            charge.evictable
        );
        assert!(charge.evictable > 0);
    }

    #[test]
    fn a_retired_view_stops_being_charged() {
        let mut cache = cache();
        let held = cache.insert(view(2, NodeKind::File)).unwrap();
        let mut scratch = cache.charge_scratch();
        let charged = cache.charge(&mut scratch).evictable;
        assert_eq!(cache.len(), 2);
        // Nothing outside the index may hold the view, or it is not reclaimable.
        drop(held);
        cache.collect(128);
        assert_eq!(cache.len(), 1);
        let after = cache.charge(&mut scratch).evictable;
        assert!(
            after < charged,
            "a reclaimed view is still charged: {charged} then {after}"
        );
    }

    #[test]
    fn the_chunk_size_is_what_the_allocator_hands_out() {
        // glibc: an eight-byte header, rounded to sixteen, minimum thirty-two.
        assert_eq!(chunk(0), 0);
        assert_eq!(chunk(1), 32);
        assert_eq!(chunk(24), 32);
        assert_eq!(chunk(25), 48);
        assert_eq!(chunk(40), 48);
        assert_eq!(chunk(41), 64);
    }

    #[test]
    fn an_arc_str_is_charged_for_its_counters_and_a_string_for_its_capacity() {
        // Sixteen bytes of counters live in the same allocation as the bytes.
        let shared: Arc<str> = Arc::from("0123456789abcdef");
        assert_eq!(arc_str(&shared), chunk(16 + 16));
        // A String that grew and shrank still owns what it reserved.
        let mut grown = String::with_capacity(200);
        grown.push('x');
        assert_eq!(string(&grown), chunk(200));
        assert_ne!(string(&grown), chunk(1));
    }
}
