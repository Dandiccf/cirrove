# Combined iCloud move and rename

Preregistered before live invocation of this new protocol on 2026-09-30.

Question: can one durable account-router relocation change both parent and name,
including cases where naive move-first and rename-first both collide?

The implementation seals three stages under the original journal operation ID:
rename the source to `.cirrove-move-OPERATION_UUID`, move that same ID into the
captured destination, then rename it to the requested final name. The destination
must initially be free of the final and temporary names. Each child rechecks its
exact captured revision. Its potentially-sent marker is saved before dispatch;
a confirmed receipt advances the captured current node and is saved before the
next dispatch. Interrupted sent stages permit inspection only. A confirmed
intermediate stage returns the overall operation to pending, where only the next
saved stage may run. Missing or ambiguous state requires review, not replay.

This is not an atomic server-side move+rename. A conflict after the first stage
can leave the item at its temporary name/location; the sealed plan is retained.
Concurrent-editor behavior and recovery UI remain release requirements.

## Synthetic protocol checks

File and folder state-machine tests pass across a lost first response and
subsequent index changes. They require a durable Sent marker before the fake
remote call, prohibit replay from Sent, inspect to advance, preserve file content
lineage, and reject cross-operation plans, malformed final locations and moves
into descendants. The negative control removed the pre-dispatch save and failed exactly
at the fake remote's durable-marker assertion (exit 101). The save was restored. These checks do not prove Apple's
live behavior.

## Live protocol and prediction

The new developer-only `--account-combined RUN_UUID` mode uses a fresh isolated
account state and owned remote fixture, with the real account router and mutation
worker. For a file, create an old-name occupant in the destination and a final-name
occupant in the source. Both remain untouched while the original file moves under
a different final name. Repeat the same collision arrangement for a populated
folder. Check exact IDs/parents/names, independently hash the moved file and the
folder's child, and recheck the blocking objects. Reopen the journal and inspect
all successful receipts. No installed daemon or other user files are changed.

Prediction: both combined operations pass through the three saved stages, retaining
identity and contents despite the two blocked naive orderings. A worker deadline,
Apple temporary-name handling or source/parent revision mismatch may instead expose
an integration failure. A first failure stops without replaying that fixture.
Permit 20 minutes. This is one functional smoke arm; no timing or repeatability
claim follows without repeated fresh arms. Use disk-backed TMPDIR/SQLITE_TMPDIR,
record findmnt, binary hash, command, run UUID, PID and expected duration before
launch, and run no compilation or competing measurement concurrently.

## Results

Arm A `6fe980d9-b3f3-45de-be0f-fef7ae2f248a` finished with exit 1 after
74.841 seconds. Only the initial Destination-folder operation completed and was
independently observed. The second setup operation, creating the source-name
blocker folder `Combined.txt`, is retained as `verify_required` in the durable
journal. Neither combined relocation was reached. No upload rows or later
operations were attempted by this protocol.

The validator's output did not retain the worker issue, so this arm does not
establish why the second creation could not be confirmed. In particular, an
uncertain result does not establish that no folder was created remotely. The
fixture and sealed plans remain untouched; do not replay creation to diagnose it.
A read-only inspection is needed before attributing the failure to transport,
name handling or revision behavior. This failed setup provides no live evidence
for or against the combined relocation implementation.

The private manifest and the public run registry record the binary hash, disk-backed
temporary directory, timestamps and terminal result. The normal iCloud write
setting remains disabled.

## Read-only setup diagnosis

Before invocation: use `--inspect-account-combined` against arm A's recorded
owned root and copied session. This path instantiates only the read session,
not a worker. It lists that folder once and outputs counts, expected-name matches,
folder kinds, parent consistency and whether name/extension are split. No bodies,
credentials, unrelated names or content are emitted. Prediction: the second folder
may already exist with its extension separated from its name, which the create
receipt parser currently does not model. A matching name alone must never be
used to acknowledge or replay the retained mutation.

The read-only inspection found exactly two children: one Destination folder and
one Combined.txt folder. Combined.txt has a nonempty separate extension. No
mutation receipt was recovered or inferred from this observation.

The new split-extension receipt regression failed before the correction (one
test executed, exit 101). With extension reconstruction, all five transport
classification/receipt tests pass, including rejection of wrong status, wrong
identity kinds, empty identity suffixes and mismatching names. Missing or null
extensions preserve plain-folder handling. This is synthetic parser evidence;
the original POST response was never logged.

### Fresh arm B

Registered before invocation: `636c20c1-8d69-4df9-830a-6e27396b5300` uses the corrected receipt parser and
the same combined file/folder protocol on fresh IDs. Prediction: the setup folder
with a dotted name now produces an independently verified durable receipt, then
both combined relocations can be evaluated. If another stage fails, retain the
worker state and bounded issue before stopping. No retry of arm A. Allow 1200
seconds; record binary hash, PID, command and disk-backed temporary filesystem.

Arm B interim observation: both setup folder operations were independently
verified, including Combined.txt. The split-extension correction therefore
passes this fresh live creation case. The combined relocation result is still
pending; setup success alone does not validate it.

### Arm B terminal result

Arm B finished successfully (exit 0) in 348.028 seconds. Both combined
relocations passed: the regular file retained its exact ID and SHA-256, and the
populated folder retained its ID and independently verified child's contents.
Source/destination blockers were re-observed. A reopened Engine/context/journal
retained all seven Applied namespace operations and three Uploaded file receipts.
The previous failed arm was not replayed.

This is one successful functional arm, not a repeatability, performance or
server-side atomicity claim. It uses the regular account router and durable
workers without FUSE or the installed daemon. Combined mounted behavior,
interruption recovery in a live combined operation and conflicting editors
remain separate gates. Ordinary account writes remain disabled.
