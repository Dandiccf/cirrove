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
