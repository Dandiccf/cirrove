# Exact-ID lookup of owned iCloud Trash predecessors

Registered before live invocation on September 30, 2026.

## Question and prediction

Can `retrieveItemDetails` return the same identity, version, explicit size and
restore metadata as a complete `retrieveItemDetailsInFolders` Trash listing for
known owned predecessors? The preceding mounted truncate/refill arm spent about
one minute per full Trash listing. Prediction: an exact lookup will be faster,
but it may omit restore metadata or refuse trashed IDs. Missing fields are a
negative result; two absent fields never count as a match. No production lookup
or write safety check changes as a consequence of this diagnostic alone.

## Scope and arms

Use the nonempty and empty predecessors recorded by successful owned fixture
`21d05f56-70c4-47a8-a5e3-577ab68994ac`. Validate its passed marker, disabled
account, owned root, local receipts, file types and sizes. Read-only calls only;
no mount, transfer worker, cloud mutation or installed-service change.

Three sequential arms, with no competing compilation or measurement. Each arm
loads the complete Trash listing, requests each of the two exact IDs separately
through the existing direct-item envelope, then reads the complete Trash listing
again. Require exact recoverable identity in both listings and unchanged fields
used by recovery receipts. Record HTTP status, count, field-presence/equality
booleans and timings only. Do not record raw bodies, IDs, paths, URLs or credentials.
The two IDs are reported in fixed order: nonempty original, then empty predecessor.

Report per-arm timings and range of observations, not just a single speed ratio.
Even matching metadata and lower latency would not prove concurrency behavior,
completeness of arbitrary direct lookups, or safe replacement of production Trash
membership checks. Failures retain the original completed fixture untouched.

## Execution

Feature-gated `--account-trash-lookup FIXTURE_UUID`; 1200-second outer deadline.
Before launch record command, binary hash, PID, source revision, dirty state,
private disk-backed TMPDIR/SQLITE_TMPDIR and expected duration in the run manifest.

Two synthetic diagnostic tests cover missing fields and unequal identities/
restore paths. These validate reporting only, not Apple's contract.

## Result

The original envelope comparison finished with exit 0 after 188.984 seconds.
All six direct lookups returned HTTP 400 (161–173 ms, 12 ms range). All six
complete listings contained 54 items and the selected metadata remained stable.
Listing times were 31.519, 31.165, 31.098, 31.614, 30.801 and 31.709 seconds
(range 0.908 seconds). These are sequential observations on one account, not
independent broad performance samples. No faster valid lookup was established.
The public live JSON retains all boolean/timing reports.

## Preregistered follow-up: active-file and request-envelope controls

Registered after the first comparison and before the control invocation.
The initial result cannot distinguish rejected Trash IDs from a request-envelope
problem: it had no active-item positive control. The nested-array request matches
the current public [rclone transport source](https://github.com/rclone/rclone/blob/master/backend/iclouddrive/api/drive.go),
but source usage does not establish validity against this account/API today.

One new read-only control uses the same two predecessors plus their active
replacement, verified against the owned parent listing. Request each exact ID
through the same `retrieveItemDetails` endpoint using, in order: an array with
an `items` wrapper (the current form), an object with an `items` array, and a
flat array of item descriptors (the folder-list endpoint's form). These last two
are explicitly experimental envelope hypotheses, not documented Apple contracts.
Do not try unrelated endpoints or mutations. Compare responses against full
Trash metadata and the active parent's exact revision. Recheck both after the
nine requests. Record only statuses, timings and field-presence/equality booleans.

Prediction: if envelope shape is the issue, one alternate form will also fix the
active control. If every form fails even for the active file, no Trash-specific
conclusion follows. If an alternate form returns incomplete recovery metadata,
it still cannot replace the existing production checks. One control arm supports
protocol investigation only. No compilation or competing measurement while live.
Allow 600 seconds and record a separate manifest/binary hash before invocation.
The control completed with exit 0 after 64.686 seconds. Both array forms returned
HTTP 400 for all three identities. The object form returned HTTP 200 and an
object containing one `items` entry, taking 899 ms for the active file and
881/852 ms for the predecessors. All compared fields present in the full listing,
including both `restorePath` values, matched exactly. The active file correctly
had no restore path. Full metadata remained unchanged after the arm.
The control JSON retains the field reports. This establishes a request *and*
response envelope bug, not an Apple prohibition on trashed-item lookup.

## Corrected production transport repeat, preregistered before invocation

The normal exact-item transport now sends an object with `items` and parses the
response's `items` object field. It requires exactly one matching ID. A synthetic
HTTP fixture first failed against the old transport with HTTP 400; it passes after
the correction and also rejects empty, duplicate, foreign-ID and array replies.
The test executes the real client serialization and deserialization.

Repeat the original three-arm full-before/direct/full-after comparison through
this corrected production helper. Add booleans for explicit Trash parent and FILE
type; no raw metadata is exposed. Prediction: all six direct observations will
match full metadata, including explicit recoverability, at less than 2 seconds
each. Reject changed full metadata. Record timings/ranges even if the prediction
fails. This still does not change the production handoff verification path.
One run at a time, 1200-second deadline, disk-backed temporary storage and a new
manifest/binary hash. The initial rejected-request and control artifacts remain
unchanged. The corrected repeat finished successfully after 183.041 seconds. All six
production direct lookups matched every compared full-list field, explicitly
reported a FILE under Trash, and had an unchanged recoverable identity across
the bracketing full listings. Direct times (ms): [1005, 817, 822, 880, 1110, 864]; range
293 ms. Full-list times (ms): [29992, 29584, 29711, 29384, 29534, 29237]; range
755 ms. This proves the bounded comparison on these two fixtures,
not arbitrary metadata consistency under concurrent actors.

## Production handoff change and acceptance, preregistered before invocation

Replace only the two full Trash inventory reads inside `verified_trash_backup_node`
with the corrected exact-item helper. Require the explicit Trash parent, FILE
type, exact Drive/document IDs and restore metadata on both observations, plus
the existing size/ETag/name and full-byte SHA-256 checks. Do not infer absence or
list completeness from this helper. Other Trash/namespace paths stay unchanged.

A synthetic HTTP case must first fail against the full-list implementation,
then pass after the change. It also removes required fields on either observation
and changes the final ETag; each must reject the receipt. Then repeat the owned
mounted truncate/refill workflow with new state and files. Retain independent full
Trash checks in that validator as a separate oracle. Prediction: all functional
endpoints still pass and per-handoff receipt verification is substantially faster;
a single run only supports a direction, not a performance release claim. Record
all phase values, complete manifest and retained journals; no installed service
change or competing build during the live arm. Allow the same 1800-second deadline.
The mounted handoff arm `e42e5a79-d55a-4714-ab01-e4d9d7a21ff1` passed all endpoints
(exit 0, 508.030 seconds): mounted create, truncate to zero, refill, independent
content digests, both predecessor identities in full Trash listings, and read
after unmount/remount. Exact `findmnt` observed fuse.cirrove during execution and
no mount after completion. The original payloads and all journals remain retained.
The installed service and ordinary write grant remain unchanged.

The preceding full-list handoff arm took 924.709 seconds; this arm was 416.679
seconds shorter (about 45%). There is only one whole-workflow run per version,
so the difference is directional evidence, not a measured within-arm performance
result. The repeated read-only comparison above supplies its own observed spread
but cannot substitute for repeated mounted workloads. The independent full-list
oracle remains in both workflows.

Per-replacement postflight times were 34.0/33.5 seconds, install preflights
4.4/4.4 seconds, and final receipts 33.8/34.6 seconds. Earlier values were
129.7/126.2, 64.3/61.7, and 128.6/128.0 seconds, respectively. The remaining
roughly 30-second delays immediately after mutations have not been isolated to a
specific request; no claim about their cause is made.

The exact-item and direct-Trash HTTP regression tests both failed before their
corresponding fixes (exit 101) and pass after them. Clippy for the isolated mounted
binary passed before the live arm. Full `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` passed (exit 0),
including formatting, clippy, workspace/feature tests, real kernel mounts, scripts,
ledger and documentation. The retained log is
`.local-state/icloud-direct-trash-full-check.log`. Desktop window scenarios are
outside this script and were not run for this transport change.
