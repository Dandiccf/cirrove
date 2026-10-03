# Large iCloud files through the regular account writer

Registered before live invocation on September 30, 2026.

## Question and prediction

Can a real mounted account create a 65 MiB + 17 byte file, replace it with
66 MiB + 29 bytes, verify exact remote digests and the recoverable predecessor,
and read the entire replacement after remount? These sizes exceed both the old
32 MiB adapter cap and the old fixed 64 MiB manager journal budget. Uneven tails
exercise the last partial range. Prediction: the bounded streaming paths work
without retaining a whole payload in the validator. This is a functional test,
not an RSS/throughput measurement or a claim covering every file size.

## Changes and synthetic controls

WriteContext now opens its journal using the account's configured `cache_bytes`,
instead of a fixed 64 MiB. Pending data remains separate from evictable read cache;
each budget uses that configured value, so combined local storage can exceed it.
A manager regression first failed when reserving 80 MiB under a 96 MiB account.
After correction it passes, including reopening below retained usage, refusing
new growth, preserving a queued payload and increasing the budget on reopen.

Normal iCloud write adapters now use the journal/filesystem's signed 64-bit
representable length bound rather than a 32 MiB experiment cap. This is not an
Apple maximum-file-size claim; actual local quota, remote quota, protocol and
timeout limits still apply. Legacy fixture-only upload probes keep their own
smaller limits. Create preflight and replacement checkpoint tests first failed
at larger sizes and now pass; values above the local numeric bound are rejected.

Content POSTs have an explicit 900-second request timeout instead of inheriting
the authentication/metadata client's 30 seconds. Full verification downloads
have 300 seconds. Outer worker bounds still apply: 900-second stream, 125-second
create begin/commit and 300-second replacement inspection/commit. This arm does
not establish slow-link or multi-gigabyte acceptance; per-phase deadline scaling
and cancellation during long local hashing remain open follow-up work. No
request timeout authorizes replaying an uncertain mutation.

## Arm

Feature-gated `--account-mounted-large RUN_UUID` with a newly owned remote folder,
fresh disabled account and 512 MiB local budget. Same regular ICloudWriteProvider,
WriteContext, transfer workers and receipt-based ownership guard as the small
mounted account trials. Python writes 64 KiB blocks and fsyncs through real FUSE;
it allocates no whole-file input buffer. After each confirmed upload, a separate
session verifies exact revision, size and whole-file SHA-256 against a separately
computed synthetic pattern. The former exact ID must appear in a complete Trash
inventory. Shut down and reopen the mount, then read all replacement bytes from
an external Python process and compare each block plus final size.

No ordinary account write grant, installed daemon or unrelated cloud file is
changed. Retain the fixture, manifests, account receipts and journals. No
competing compilation or other measurement while the arm is live. Before start,
record binary hash, revision/dirty state, command, PID, 2400-second outer deadline,
private disk-backed TMPDIR/SQLITE_TMPDIR and filesystem. A timeout/failure retains
state and never triggers a blind retry. Approximate retained cloud fixture size
is 131 MiB across original and replacement; no automatic fixture cleanup.

## Result

The arm `e1fbb31a-6195-49dc-9b77-eb70e8326934` completed with exit 0 after
407.578 seconds. Both exact sizes and remote SHA-256 values matched. The two
journal uploads reached Uploaded; the old exact ID was independently found in a
complete Trash inventory. A remounted external Python reader compared every byte
of the 69,206,045-byte replacement, including its uneven final block. All recorded
functional endpoints are true. `findmnt` observed fuse.cirrove during the run and
no remaining mount after successful shutdown.

One functional arm establishes these two sizes on this account. It does not prove
multi-gigabyte support, stable throughput, slow-link recovery or bounded total RSS.
No performance comparison to the small-file trials is claimed. The numeric bound
is not advertised as a tested cloud capacity. The installed service and ordinary
iCloud write gate remain untouched. The final full
`CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` completed with
exit 0 after the local cancellation and CI-readiness fixes. Workspace, feature,
real-kernel mount, script and ledger checks passed. Graphical window scenarios
and installed-daemon acceptance were not run; the existing rustdoc link warning
remains.

## Post-run local hash cancellation regression

The streamed upload previously checked cancellation only after a complete local
SHA-256 pass. A synthetic reader now cancels after its first block to verify that
the blocking hash task stops before asking for another block. This local change is
subsequent to the live binary above; its evidence is separate from the cloud arm.
The hash loop also requires exact length and digest, with a fixed 64 KiB buffer.
The cancellation assertion failed before the guard (exit 101), then both payload
hash tests passed after it (cancel before another block; exact size/digest). This
covers the iCloud adapter's additional local hash pass, not cancellation of every
shared journal disk operation. No live cancellation/recovery result is claimed.
