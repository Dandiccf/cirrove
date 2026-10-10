# iCloud adaptive read windows: registered cache-path comparison

Question: do ordinary adaptive read windows reduce repeated read validation work
and elapsed sequential-read time while preserving exact varied-content bytes?

Registration (before live execution): 2026-10-01. The previous 1 GiB run used
constant bytes per generation and cannot serve as this comparison's baseline.

## Arms and endpoint

Create exactly one fresh Cirrove-owned folder/file using the isolated account
router and verify its full remote SHA-256 independently before timing. Size:
64 MiB + 17 bytes. Each 8-byte word is an invertible transform of its position;
64 KiB blocks differ, so reordered chunks are detectable. All reads target only
this exact newly created receipt. No personal file, installed account or daemon
changes; no cloud deletion or replacement in this experiment.

One binary, sequential A/B/B/A/A/B (three arms per variant): A disables only
`open_read_session` at the validation wrapper; B forwards it to the iCloud adapter.
Each arm constructs an independent empty disk block cache and adapter, with a
256 MiB budget, and reads 64 KiB requests through the real `ContentCache` path.
No mount: these timings exclude filesystem traversal, FUSE/kernel behavior and
GUI latency. No second measurement or compilation during execution. Private
TMPDIR and SQLITE_TMPDIR must be disk-backed; record filesystem, binary hash,
process id, command and deadline in the run manifest before starting.

Primary endpoint: full-read elapsed seconds including cache I/O and byte checking,
but excluding source creation/independent verification and cache construction.
Secondary: first 64 KiB latency, logical exact-range/window call counts, validated
windows, staged bytes and peak disk staging reservation. Logical calls are not
wire-level HTTP counts or a measured network-time decomposition. No RSS claim.
Every arm must match expected bytes at their exact positions and the complete
SHA-256; sparse cached rereads must perform no additional provider calls. Each B
arm must actually validate a streamed window; A must issue 17 exact-range calls.
Record failures and keep every fixture; never silently replace an unsuccessful
arm. Deadline: 600 seconds per arm, 1800 seconds overall. Stop on failure.

Prediction: B should make fewer logical transfer calls and reduce median total
read time; first-read latency should be similar because both start with an exact
range. The magnitude is unknown and includes Apple's variability. Report each
arm and each variant's min/max spread, not just a percentage. This cannot close
interrupted-network, expired-session, mounted-application or installed gates.

## First attempt: receipt compatibility failure, not a comparison result

Run `de7762b5-1b39-44c7-a8bc-7c02b748ff35` terminated with exit 1 after
103.795 seconds. Source upload and independent digest succeeded; arm A0 read all
67,108,881 bytes correctly in 27.070 s, first 64 KiB 2.653 s, 17 exact-range calls,
no windows, and cached sparse rereads without cloud calls. B1 then failed before
content transfer: `invalid iCloud ordinary read identity`. No remaining arms ran;
this is not performance evidence for B.

Inspection of the owned receipt identified `content_version == etag`. File-create
receipts already carry this valid revision in both namespaces, while ordinary
listing metadata carries only ETag. The new session validator wrongly rejected
all content revisions. A regression test reproduced the failure before the fix.
The fix accepts matching content/ETag revisions while retaining before/after
ETag+size validation. Distinct synthetic content revisions use the existing
exact-range fallback instead of making the file unreadable. The six ordinary
session tests pass after the fix, including changed-revision rejection for an
upload receipt. The failed fixture and manifest are retained.

A second run is registered with the same size, endpoints, A/B/B/A/A/B sequence,
and deadlines, using a fresh owned source and new empty caches. It is a new
attempt, not a continuation or replacement of this failed measurement. The fixed
binary hash will be recorded in its own manifest.

## Corrected live result

Run `285cf2d5-ba33-4074-bd42-5bacc35e1bd4` completed successfully in 234.363 s
including setup, 2026-10-01 01:19:11–01:23:05 UTC. Binary SHA-256:
`3d3e2ecfad6cefd21d1d4bab471e57ab365f9c1cab44a0ab630f0d19aa8a687a`.
The [run manifest](icloud-read-windows-285cf2d5-ba33-4074-bd42-5bacc35e1bd4-live.json)
contains exact observations; the failed predecessor remains a separate artifact.

| Arm | Variant | Full read seconds | First 64 KiB seconds | Exact ranges | Windows |
| --- | --- | ---: | ---: | ---: | ---: |
| 0 | A: exact ranges | 26.918 | 2.645 | 17 | 0 |
| 1 | B: adaptive windows | 14.247 | 2.795 | 3 | 3 |
| 2 | B: adaptive windows | 13.502 | 2.583 | 3 | 3 |
| 3 | A: exact ranges | 31.348 | 2.601 | 17 | 0 |
| 4 | A: exact ranges | 28.394 | 2.565 | 17 | 0 |
| 5 | B: adaptive windows | 14.272 | 2.769 | 3 | 3 |

A median: 28.394 s, range 26.918–31.348 s (spread 4.430 s).
B median: 14.247 s, range 13.502–14.272 s (spread 0.770 s).
Median difference: 14.148 s / 49.8% less elapsed time in this experiment. Every B
arm made 6 logical transfer calls instead of 17, with 3 validated windows,
56 MiB staged and 32 MiB peak staging reservation. These are private disk staging
counters, not process RSS. All six full reads passed position-by-position and
whole-file hash comparisons; all cached sparse rereads made no provider calls.

First-read ranges overlap (A 2.565–2.645 s; B 2.583–2.795 s). There is no observed
first-open latency improvement. Three runs per arm on one account and one file
support this bounded result, not a guarantee for all networks or folder sizes.
No build/second measurement ran concurrently. Temporary storage was btrfs under
`/var/tmp`; the installed daemon was unchanged. No filesystem was mounted by
this test. All private cache/source receipts and journals are retained below
`.local-state/icloud-account-read-windows-<run>/`; logs/manifests are below
`.local-state/icloud-read-windows-<run>/`. Mid-transfer faults and installed
file-manager acceptance remain open.

Complete `scripts/check.sh` passed after both live attempts, 2026-10-01
01:24:28–01:30:55 UTC, including workspace/feature/kernel tests, clippy, script,
translation, ledger and documentation checks. Its manifest/log are retained at
`.local-state/icloud-read-windows-live-check-2026-10-01/`; temporary storage was
disk-backed btrfs. No GUI behavior changed, so native-window scenarios were not
rerun. No installation or regular service restart was performed in this step.
