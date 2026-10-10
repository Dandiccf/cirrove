# Mounted iCloud relocation: lost intermediate checkpoints

## Registered question and prediction

Does the regular account writer recover a three-step combined file rename/move
when the cloud has confirmed a child operation but its updated encrypted plan has
not been saved? Prediction: the persisted Sent plan forces exact-item inspection;
the worker then dispatches only the remaining child steps, preserving bytes and
identity. The complete logical operation must never be acknowledged at an
intermediate location.

## Arms and endpoint

Two fresh owned fixtures, each with its own process-interruption and fresh-process
recovery arm, use the existing mounted relocation fixture. Arm 1 interrupts before
saving Ready/step 1 (temporary rename confirmed); arm 2 before Ready/step 2
(cross-parent move confirmed). Both persist Sent for the preceding child first.
The feature-only wrapper surrounds the real sealed checkpoint vault, neither
changes cloud responses nor implements a replacement transport. It writes a
private marker and exits 86 before saving the next checkpoint.

Recovery uses the same binary and journal, verifies the Sent checkpoint, hashes
the remote intermediate file and destination-name blocker independently, then
mounts through the ordinary worker. A checkpoint guard rejects attempts to send
an already completed child and records the exact remaining Sent sequence. The
required sequences are [1, 2] and [2] (zero-based child indices). Recovery must
reach Applied, independently hash the final file, preserve both name collisions,
complete the populated-folder control and read all paths after remount. No
permanent deletion is authorized. Failed fixtures remain retained, not reused.

Each arm records its command, binary SHA-256, process ID, disk-backed TMPDIR and
SQLITE_TMPDIR, and a 1,800-second deadline before running. No compilation or other
measurement overlaps the live arms. The exact stale isolated mount may be detached
only after its child process terminates. No normal account/service is changed.

## Limits

These boundaries follow a confirmed remote response. They do not reproduce an
in-flight transport loss or prove provider-wide write reliability. One run per
boundary has no within-arm spread and supports no latency distribution. General
write enablement, deletion interruption, concurrent actors at intermediate paths,
recovery UI and installed release validation remain open.

## Results

The first boundary passed using run `8c2c3382-5394-446f-ad8e-9adfdb7a05c6`.
The interruption exited 86 after 217.734 seconds; recovery exited 0 after
149.330 seconds. Both isolated mounts detached. Independent full-content checks,
the populated-folder control and remounted paths passed. A separate read-only
SQLite audit found three Uploaded and seven Applied records, preserved identities
and no unfinished queue reservations. Only child indices [1, 2] were dispatched
on recovery, once each; child 0 was recovered by inspection.

- [First interruption](icloud-mounted-relocation-intermediate-8c2c3382-5394-446f-ad8e-9adfdb7a05c6-interrupt-live.json)
- [First recovery](icloud-mounted-relocation-intermediate-8c2c3382-5394-446f-ad8e-9adfdb7a05c6-recover-live.json)
- [First independent journal audit](icloud-mounted-relocation-intermediate-8c2c3382-5394-446f-ad8e-9adfdb7a05c6-journal.json)

Binary SHA-256:
`4c8305d480eda27995307f9ab6f6e2ef046048c3e98b419c404ebdc6d72aa366`.
The second boundary also passed, using fresh run
`666cdd69-c0c0-4414-9823-882edb589d23` and the same binary. Its interruption
exited 86 after 231.393 seconds; recovery exited 0 after 135.966 seconds.
Both mounts detached. The independent journal audit again found three Uploaded,
seven Applied, eight distinct active owners and no unfinished queue reservations.
Only child index [2] was dispatched during recovery: the already completed
cross-parent move was inspected, not sent again.

- [Second interruption](icloud-mounted-relocation-intermediate-666cdd69-c0c0-4414-9823-882edb589d23-interrupt-live.json)
- [Second recovery](icloud-mounted-relocation-intermediate-666cdd69-c0c0-4414-9823-882edb589d23-recover-live.json)
- [Second independent journal audit](icloud-mounted-relocation-intermediate-666cdd69-c0c0-4414-9823-882edb589d23-journal.json)

For these intermediate arms the independent `remaining_child_dispatches` sequence
is the evidence against repeating completed child operations. The older
`recovery_mutation_replays` field counts forbidden *whole-operation* dispatches
only in the final-acknowledgement arm; intermediate recovery legitimately calls
the logical mutation entry point to execute its remaining children.

Durations include fixture setup and controls; no individual API latency or
within-arm spread is inferred from them. These are two distinct boundaries, not
two repetitions of the same arm. Both use real provider responses, with synthetic test data
and child effects deliberately limited to newly owned cloud items.


## Full project validation

`scripts/check.sh` passed without `--fast`, exit 0, on 2026-09-30 from
21:08:01 to 21:14:03 UTC, after all four live processes had terminated.
Formatting, workspace/feature clippy and tests, actual-kernel FUSE scenarios,
script tests, acceptance ledger and docs completed. The worktree used its own
`.target-icloud-feasibility`, with TMPDIR and SQLITE_TMPDIR in the same private
verified btrfs directory under `/var/tmp`. The known rustdoc `retry_stuck` link
warning remains. Display-dependent window tests were not run; no GUI changes
are included. Four targeted relocation-boundary unit tests also passed before
the live runs, including foreign fixture rejection and refusing repeated or
out-of-order child dispatches.
