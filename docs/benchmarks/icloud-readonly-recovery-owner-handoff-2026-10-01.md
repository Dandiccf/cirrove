# Read-only recovery owner handoff — preregistered correction

The package full project check ended at 08:50:58 UTC with 249 service library
tests passed and one failed: `read_only_exports_exact_saved_and_dirty_versions_without_changing_journal`
reported the sanitized error `local recovery journal is busy or unavailable`.
No package live acceptance failed. Earlier CLI-process isolation did not remove
all possible causes of transient lock refusal.

Inspection found a concrete lifetime race: the read-only recovery cache stores
only a Weak reference. The last Arc strong count reaches zero before its payload
destructor closes SQLite and then owner.lock. A concurrent recovery request can
see an expired Weak and try to open the same journal while its prior owner is
still closing. A zero strong count is therefore not evidence of released locks.
Concurrent unit-test child process creation is another possible source of brief
inherited lock descriptors; the failing log alone does not distinguish them.

Question: can read-only owner acquisition race the destruction of its previous
in-process owner? Prediction: a controlled last-owner teardown barrier reproduces
the old refusal; serialization that begins before releasing the strong reference
allows the replacement owner only after the prior journal is actually dropped.
A gate acquired only inside the Arc payload destructor is insufficient because
an opener can win between the strong-count transition and that acquisition.

Preserve weak caching, idle release, detached-copy account/journal leases and
exclusive refusal of a truly competing owner. Do not fix with arbitrary sleeps,
blind Busy retries, a permanently held engine cache or relaxed file locks.
Synthetic control must fail without lifetime serialization and pass with it.
Re-run recovery ownership/export tests and the entire scripts/check.sh before
committing. Installed daemon remains unchanged throughout.

The unchanged focused recovery test group passed at 08:53:30–08:53:43 UTC.
This does not disprove the concurrent teardown race or count as its fix. A
deterministic teardown barrier remains required before changing lifetime code.

Controlled last-owner negative run (08:58:26–08:58:49 UTC) failed as expected
when the opener did not take the gate: it completed while the old journal
destructor was deliberately held. Restoring synchronization passed all eight
recovery tests (08:59:23–08:59:35 UTC), including account ownership through
close. Independent review additionally found a cancelled-open publication gap:
the blocking opener must publish its Weak cache entry while retaining the
owned cache guard, even if its async caller is abandoned. That extension and
its separate deterministic test remain in progress before the full recheck.

The cancelled-open regression failed at 09:02:34–09:02:51 UTC when the
blocking opener deliberately released the cache guard before publication:
`cancelled caller exposed an unpublished live journal`. The correction keeps
an owned async cache guard inside the blocking worker, publishes Weak before
releasing it and returns an account-retaining lease. Review found no remaining
lock-order or lease-lifetime blocker. This negative control establishes the
publication requirement separately from the previous close-gate regression.

All nine corrected recovery tests passed 09:03:10–09:03:22 UTC, including
saved/working export without journal changes, idle release, detached blocking
owners, controlled last-owner close and cancelled opener publication. Full
repository check is rerun next; installed acceptance remains separate.

The first full rerun stopped at seven newly added test-hook `unwrap_used`
lints, before tests. Those calls now carry descriptive `expect` messages;
there is no runtime behavior change. The next complete check began at
09:04:14 UTC and passed its format and Clippy stages; final result is pending.

The second full check ended at 09:06:00 UTC with the same export test failing
specifically at working listing. The gate fixes above were insufficient: an
unrelated inherited file descriptor can retain flock after the final logical
journal lease closes. A safe subprocess fixture explicitly inherits only its
synthetic owner descriptor and keeps it alive. Before the correction it failed
at 09:09:48 UTC: recovery remained Busy after the last working reservation ended.

Journal ownership now uses one shared Arc across the journal, working reservations
and active export stages. Its final destructor explicitly unlocks in the acquiring
process only, after SQLite and local-copy resources close. A PID guard prevents a
forked child's Rust destructor from unlocking a live parent's lease. Merely adding
unlock to UploadJournal's destructor would incorrectly release live reservations.
The exact inherited-descriptor regression passed at 09:15:37 UTC (one test),
including refusal while a legitimate working reservation remained alive and
successful reopening while the unrelated subprocess still held its descriptor.
Artifacts: `.local-state/icloud-access-inherited-journal-owner-{red,green}-2026-10-01/`.
Cross-lease export/preparation regressions and the full check follow separately.

Cross-lease regressions passed at 09:15:50 UTC: upload preparation 10 passed
(one ignored helper) and working export 22 passed. Independent source review
found no remaining owner-clone, drop-order, cancellation-publication or lock-order
blocker. The complete repository check started at 09:16:09 UTC; its outcome is
not implied by these focused results.

The complete `scripts/check.sh` passed from 09:16:09 to 09:23:56 UTC,
including formatting, Clippy, workspace and feature tests, real FUSE mount tests,
script/ledger checks and documentation. The previously failing recovery export
also passed in that complete run. Artifact:
`.local-state/icloud-access-journal-shared-owner-fullcheck-2026-10-01/`.
Native window scenarios and installed-daemon acceptance are separate; this change
has not restarted or replaced the installed daemon.
