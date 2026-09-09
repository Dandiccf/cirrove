# Pre-registration: malloc_trim(0) probe   (written 2026-09-08 ~19:1x, BEFORE the run)

## What has NOT been tested
All four allocator arms configured GLIBC_TUNABLES. `trim_threshold` governs trimming
the TOP of an arena on free(). `malloc_trim(0)` is a different operation: it walks
every arena and MADV_DONTNEEDs free page ranges WITHIN each heap. Nothing on record
has called it.

## Why it might matter
At the `released` phase, live heap is 17.1-17.6 MiB against 490-560 MB RSS; 96% of
retained RSS is free arena. Growth landed as committed pages inside long-lived
non-main arena heaps, which is precisely what trim_threshold cannot reach and
malloc_trim(0) can.

## Prediction, on the record before the result
Post-trim `released` PSS minus indexed_baseline PSS lands at 40-150 MiB, i.e. G3 as
literally written PASSES.
How this fails: glibc returns only whole free pages. 750k retired small objects can
leave one live object per page, so fragmentation could make almost nothing returnable.

## Decision rule, fixed before the run
- Post-trim < 256 MiB in all three rounds -> G3 is reachable by a shipped idle-gated
  trim. Shedding is demoted from "required for G3" to "required for the peak and for
  survivability". I must then argue for adding a PEAK criterion to ADR 0005, because
  otherwise the gate is passable while the high-water stays at ~500 MB. State that as
  a re-scoping, not bank it as a win.
- Post-trim >= 256 MiB -> shedding is mandatory for G3 itself. The plan stands.

## Method
scripts/heap-probe.c, env-gated with CIRROVE_HEAP_PROBE_TRIM=1: on the `released`
marker, call malloc_trim(0) and append a post-trim /proc/self/smaps_rollup sample.
Frozen release binary .local-state/compact-resident/cirrove-service-compact-release.
TMPDIR on btrfs, asserted not tmpfs. One test per process, machine otherwise idle.
n=1: this decides a DIRECTION cheaply, and a direction is all a 3-hour probe can buy.
