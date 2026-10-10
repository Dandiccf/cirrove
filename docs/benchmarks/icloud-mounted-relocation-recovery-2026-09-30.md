# Mounted iCloud combined relocation and acknowledgement-loss recovery

Registered before the live arms on 2026-09-30.

## Question and prediction

Can a file be moved and renamed through the ordinary FUSE namespace when both
naive orderings collide, then survive process loss after the provider's final
receipt but before the mutation journal acknowledges it? Can that recovered mount
also move and rename a populated folder without losing its child or either blocker?

Prediction: the router's durable three-step plan reaches Complete at step 3,
while the interrupted mutation remains Applying on disk and becomes VerifyRequired
when a new journal owner opens it. Recovery adopts the saved receipt without any
mutation dispatch for the interrupted operation. Source ID and full bytes stay
unchanged. The following populated-folder move retains the folder/child IDs and
both blocker entries. All paths remain correct after another mount restart.

## Registered procedure

Use a fresh UUID with feature-gated
`--account-mounted-relocation-interrupt <UUID>`. In its new owned validation
folder, create Destination and a source-side Combined.txt blocker directory through
FUSE. Upload Account Router.txt in the root and a different Account Router.txt in
Destination; independently hash both. Rename the root file to Destination/Combined.txt
through FUSE while retaining an open original descriptor. Both rename-first and
move-first would collide. Let the normal router complete its three stages, then
exit 86 before returning the receipt to the journal worker. Do not run destructors.

Detach only that process's stale isolated mount. Launch a fresh process with
`--account-mounted-relocation-recover <same UUID>`, once only. Validate the owned
fixture, unfinished journal record and sealed Complete/step-3 plan. Independently
hash the already moved file and unchanged blocker before mounting. Refuse and count
any mutation dispatch for the interrupted operation. Confirm the recovered receipt
and both file paths. Then create a populated Folder Source, source-side Folder Target
blocker and Destination/Folder Source blocker; move to Destination/Folder Target
through FUSE. Independently check the folder identity/location, child digest and
blockers, then remount/read the file and folder cases again. Audit SQLite read-only.

Only newly owned fixture items may be changed; no permanent deletion, personal-file
mutation, installed-service restart or normal iCloud write opt-in. Failed fixtures
remain retained. PID, binary SHA-256, command, expected duration and private disk-backed
TMPDIR/SQLITE_TMPDIR are recorded before each arm. One arm at a time, no compilation
while either arm runs. A timeout does not authorize replay or reuse of the fixture.

## Scope

This point is final cloud completion before local acknowledgement, not an in-flight
network loss or any of the two intermediate relocation steps. One run establishes
the registered sequence, not repeatability or a performance distribution. Remaining
operation boundaries, uncertain outcomes, recovery UI and installed release acceptance
stay open. No within-arm spread is available.

## Results

Run `0bc993cf-3503-491f-a132-7b7b02feab35`, interruption arm: exit 86 at the
registered boundary after 249.916 seconds; the stale isolated mount detached.
Binary SHA-256:
`2a3ab9f99a7836fec343958a795b8a04b29fa6e6da981524800a752eff1668cf`.
The owned setup was independently confirmed before the rename. This arm alone
proves the interruption was reached, not that recovery is correct.

- [Interruption result](icloud-mounted-relocation-recovery-0bc993cf-3503-491f-a132-7b7b02feab35-interrupt-live.json)

The fresh-process recovery arm exited 1 after 4.177 seconds and detached.
Its preflight verified the sealed Complete plan and both remote contents, but the
shared journal marked the returned receipt NeedsReview: the iCloud nodes had no
content-version token and the metadata ETag changed during relocation. The router's
full-byte evidence did not cross the provider-neutral acknowledgement boundary.
The fixture remains retained and is not reused.

- [Retained failed recovery](icloud-mounted-relocation-recovery-0bc993cf-3503-491f-a132-7b7b02feab35-recover-live.json)

A synthetic shared-worker regression reproduces NeedsReview without the fix.
The new verified-content result carries the original digest and binds it to scope,
item, source/result ETags and size. The journal validates and retains that evidence;
ordinary unproven observations still require content lineage or remain NeedsReview.
The iCloud router supplies it only after its full-content-checked file adapter or
a completed durable three-step plan. Changed/missing bytes do not gain a bypass.

Corrected fresh run `6bc62da5-7a26-4713-a002-b769cf86d95a` passed both arms.
The interruption arm exited 86 after 235.352 seconds and detached. A read-only
SQLite audit before reopening found Applied, Applied, Applying, with no receipt
or content proof yet acknowledged for the interrupted move. Recovery used the
same binary SHA-256:
`373a64324fa6fbb9f55ceef450a866449861a59e7159d98319f45eb0cb8d7de9`.
It exited 0 after 119.113 seconds and detached, with zero mutation replays for
the interrupted operation. The three-step file receipt and independently checked
contents survived, the populated folder and its child retained their identities,
both naive-order blockers remained, and all registered paths read correctly after
another mount restart.

The final independent SQLite audit passed integrity checks, found three Uploaded
records and seven Applied mutations, and validated the persisted content proof's
scope, item, original/result ETags, size and digest against the known original.
Eight fixture identities retained distinct active owners. There were no deletion
intents and no unfinished queue reservations. This is one corrected correctness
sequence; the failed run is retained and no latency distribution is claimed.

- [Corrected interruption](icloud-mounted-relocation-recovery-6bc62da5-7a26-4713-a002-b769cf86d95a-interrupt-live.json)
- [Before-recovery journal audit](icloud-mounted-relocation-recovery-6bc62da5-7a26-4713-a002-b769cf86d95a-before-recovery.json)
- [Corrected recovery and folder result](icloud-mounted-relocation-recovery-6bc62da5-7a26-4713-a002-b769cf86d95a-recover-live.json)
- [Final journal audit](icloud-mounted-relocation-recovery-6bc62da5-7a26-4713-a002-b769cf86d95a-journal.json)

Synthetic tests cover exact fixture binding, refusal of mutation replay, missing
content lineage, persistence across journal restart, subsequent write rebasing,
foreign account/provider/collection/item proofs, mismatched ETags, malformed digest,
size changes and package/folder exclusions. Ordinary unproven observations still
become NeedsReview. No installed daemon or normal iCloud access mode was changed.

## Full project validation

`scripts/check.sh` passed without `--fast` (exit 0) on 2026-09-30,
20:38:07–20:46:28 UTC. It ran formatting, workspace and feature-gated
clippy/tests, actual-kernel FUSE fixtures, script tests, the acceptance ledger
and documentation generation. The worktree used its own
`.target-icloud-feasibility`; `TMPDIR` and `SQLITE_TMPDIR` were the same private
`/var/tmp` directory on verified btrfs. Both live arms had finished before this
check began. The existing rustdoc warning about the `retry_stuck` link remains.
Display-dependent window scenarios are outside this command and were not run;
this change does not modify the GUI.
