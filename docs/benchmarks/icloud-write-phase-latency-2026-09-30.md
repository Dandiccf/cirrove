# iCloud write-phase latency diagnosis

## Registered question, endpoint and prediction

Which network phase accounts for minute-scale waits in tiny-file create and
replacement? Earlier complete account-router runs took 503–508 seconds and
reported repeated 60–130 second backup checks. Prediction: content download
lookup or response acquisition, rather than hashing a few bytes or worker polling,
dominates those checks. This is a diagnostic, not a performance improvement claim.

Run the existing `--account-uploads` probe once with a fresh owned fixture and
static phase timers enabled only by the `write-probe` feature. Record folder
metadata, exact-item metadata, download lookup, download response headers/body,
upload slot, content upload and registration durations. Handoff phase timings
remain available to relate nested reads to replacement steps. No IDs, paths,
contents, response bodies, URLs or credentials appear in timing lines.

The endpoint is successful create/replacement, independently verified content,
recoverable original in Trash and persisted successful receipts after reopening,
plus attribution of the long waits to measured phases. One run cannot establish
repeatability or an improvement; any proposed optimization needs its own predicted
comparison and repeated validation. Retain the owned fixture and failed state.

A manifest records binary hash, command, PID, expected 1,800-second bound and
private btrfs TMPDIR/SQLITE_TMPDIR before launch. No other measurement or compile
runs concurrently. No normal service restart or ordinary-account write enablement.

## Results

Run `b6cc58c4-19de-487e-ba72-0069d74ccfdf` passed in 331.430 seconds.
Six folder metadata calls took 29.342–31.353 seconds each. Uploads took
0.495/0.500 seconds, registration 1.475/1.526, lookup 0.424–0.616, and
verification response headers 0.195–2.077 seconds. Trash exact-ID observations
were 0.801–0.866 seconds. The prediction about download acquisition dominating
is contradicted: folder metadata dominates the measured waits. Timing labels in
this first arm do not distinguish root from child listings. Existing full
ancestor checks are a candidate explanation, not yet a controlled causal result.

[Raw duration arrays and functional result](icloud-write-phase-latency-b6cc58c4-19de-487e-ba72-0069d74ccfdf.json)

## Registered read-only follow-up

Using only the completed fixture's saved session, compare three pairs of root
listings with `partialData=false` then `partialData=true`. Require the same
complete multiset of scoped item identities, parents, names and kinds; incomplete
responses remain rejected by the existing count check. No write decision uses the
experimental result. Prediction: reduced metadata may avoid the expensive folder
expansion while preserving the complete identity inventory. Failure, truncation
or a differing inventory disqualifies it as a drop-in parent-check optimization.
Record both durations and equivalence booleans only; no private IDs or names.
Fresh process, disk-backed temporary directories, no concurrent compile or live
measurement, 900-second total deadline. This is not a cold-cache comparison;
fixed arm order and provider caching may confound timing differences.


The first three-pair diagnostic completed in 3.224 seconds. Full listings took
0.596–0.742 seconds; the partial branch produced no complete inventory in any
pair. Its initial error handling grouped all failures together, so that artifact
alone cannot distinguish count truncation from another error. A read-only repeat
now maps only the typed `IncompleteFolder` error to `partial_complete=false` and
propagates other failures. It also leaves the source fixture untouched; the
external runner records printed durations in its own fresh manifest directory.
Prediction: the count-incomplete classification will repeat. No optimization has
been enabled, and warm full listings do not reproduce the slow post-write calls.

[Initial read-only comparison](icloud-parent-listing-timing-b6cc58c4-19de-487e-ba72-0069d74ccfdf.json)


The classified repeat exited 1 after 1.268 seconds. The error was
`invalid iCloud folder listing response (unexpected JSON shape)`, not the typed
incomplete-count error. This corrects any interpretation of the first artifact's
`partial_complete=false` as proven truncation: it only proved no usable complete
inventory. The narrower repeat preserves the actual failure instead of hiding it.
The reduced projection is **not** enabled for production metadata or writes.
No raw response bodies were logged to investigate this shape.

[Classified repeat, retained failure](icloud-parent-listing-classified-b6cc58c4-19de-487e-ba72-0069d74ccfdf.json)

Next work must isolate slow full listings under controlled post-write conditions
and validate any targeted parent-identity request against the existing account,
parent, name, identity and collision guards. Neither dropping full-content checks
nor substituting this unverified response shape is supported by these results.


## Validation

The initial full `scripts/check.sh` stopped at clippy because explicit `drop`
of the zero-sized, non-Drop normal-build timer is rejected. The timer now has a
consuming `finish` method in both configurations; write-probe duration recording
is unchanged and ordinary builds still have a no-op timer. No transport or
validation condition changed for this correction.

The complete command then passed without `--fast`, exit 0, from 21:30:58 to
21:38:15 UTC on 2026-09-30. It covered format, workspace and feature clippy/tests,
actual-kernel FUSE scenarios, script tests, ledger and docs. It used the separate
worktree target and a private verified btrfs TMPDIR/SQLITE_TMPDIR under `/var/tmp`.
All live measurements had ended first. The existing rustdoc `retry_stuck` link
warning remains; display-dependent GUI scenarios were not run because no GUI
behavior changed. This is measurement infrastructure and evidence, not an
installed speed improvement or a new iCloud write permission.


## Registered schema-only follow-up

Read the reduced root listing once more through the same read-only comparison.
Emit only fixed booleans for array shape, exact requested ID match, root type,
items array, declared count, matching count and OK status. Never emit arbitrary
keys or provider bodies. Prediction: the reduced envelope omits metadata required
by DriveEntry; if its child inventory is complete, a dedicated envelope parser
could preserve all child validation without inventing root metadata. Otherwise
reject it. Fresh runner output, same 900-second bound, no concurrent compilation.


The schema-only arm exited 1 in 1.367 seconds but established a one-folder array,
matching requested ID, OK status, a present type, items array and declared count
matching its length. It contradicts the missing-root-type prediction. This still
does not establish valid child metadata. Extend fixed booleans to child ID/type
presence, null nested items and whether every child individually decodes; retain
all values privately and print none. The next hypothesis is a null/default
representation difference in child fields, not an incomplete inventory.

[Envelope shape](icloud-parent-listing-shape-b6cc58c4-19de-487e-ba72-0069d74ccfdf.json)


The child-shape follow-up exited 1 in 1.218 seconds: IDs are present but child
types are not all present, and child decoding fails. Nested items are not null.
The reduced projection cannot substitute for full DriveEntry metadata. Retain
that refusal; do not fill in missing kinds from ID prefixes or stale cache.

[Child shape](icloud-parent-listing-child-shape-b6cc58c4-19de-487e-ba72-0069d74ccfdf.json)

## Registered exact-parent optimization

Change only file-create parent verification (also used for staged uploads) to
request the known folder's full envelope instead of enumerating its grandparent.
Preserve exact requested folder ID, parent ID, name, folder kind and complete
child-count checks. The old file-create guard does not require sibling-name
uniqueness; handoff's stricter sibling-collision checks remain unchanged.
A synthetic HTTP regression failed against the old code with the root ID instead
of the requested owned folder. It also checks mismatched ID, parent, name, kind
and incomplete replies; all must still fail after the change.

Run a fresh account create/replacement fixture with the optimized binary and
independent full-content/Trash/reopened-journal acceptance. Prediction: fewer
expensive ancestor checks during create/staging, with identical identity guards.
The earlier 331.430-second run is an exploratory baseline, not a randomized
matched control. Repeat a successful optimized arm with another fresh fixture;
report within-arm spread and do not attribute a whole-run latency ratio causally
because provider caches and handoff phases can vary. Each arm uses the existing
1,800-second manifest, disk-backed temp storage and no overlapping compilation.


The optimized live arms both passed:

- `04638ccd-021f-4b03-a90d-b9a46cdba699`: 291.657 seconds, exit 0.
- `a083ceaf-9a98-4c20-aa11-ee6cca529a3a`: 295.075 seconds, exit 0.

Both used SHA-256
`7fa21182bb39a46bd133b3425ff091f610b9cf467bd75775419cf1c040a38da0`.
The range is 291.657–295.075 seconds, spread 3.418 seconds (1.17% of their
293.366-second mean). Both verified create and replacement contents independently,
the exact original in recoverable Trash, and successful persisted journal receipts
after reopening. The same ordinary router and original full-response projection
were used; no normal service or account access mode changed.

The exploratory old arm took 331.430 seconds; this non-randomized, differently
instrumented comparison supports a direction only, not a causal percentage speedup.
The structural result is precise: file-create parent verification no longer
requests the grandparent inventory. Remaining root calls still include roughly
28–30 second waits in both optimized runs. The full sequence includes fixture
setup, router/engine creation and independent validation, not just one user save.
The large overall latency remains an open product issue.

- [Optimized arm 1](icloud-write-phase-latency-04638ccd-021f-4b03-a90d-b9a46cdba699.json)
- [Optimized arm 2](icloud-write-phase-latency-a083ceaf-9a98-4c20-aa11-ee6cca529a3a.json)


The first full check after the exact-parent change failed in the independent
synthetic FUSE test `real_combined_namespace_churn_preserves_mapped_content`:
cached navigation during committed changes took 2112.491568 ms against a 500 ms
bound, at round 3. This fixture does not use iCloud or the modified create adapter.
Its other 11 capacity tests passed; no check is waived and no threshold changed.
The failure is retained. A later host snapshot showed load 1.86/5.04/3.45 and no
active compiler among the leading CPU processes; it does not prove the load at
the failing instant. Repeat the complete check with a fresh private temporary
directory to distinguish a persistent failure from a transient scheduling delay.


The complete repeat passed, exit 0, 2026-09-30 22:03:25–22:08:59 UTC,
including the previously failing capacity fixture. It ran the unmodified full
`scripts/check.sh`: formatting, workspace/feature clippy and tests, real kernel
mount scenarios, script checks, ledger and docs. It used the separate worktree
target and fresh private btrfs TMPDIR/SQLITE_TMPDIR. No threshold, test selection
or product code changed between the failed and successful complete checks.
The existing rustdoc `retry_stuck` link warning remains. GUI window scenarios
were not run; there is no GUI change or installed write-mode enablement here.
