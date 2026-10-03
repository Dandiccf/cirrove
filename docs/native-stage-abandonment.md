# Retain and abandon a native replacement Stage

`abandon-native-stage` is an explicit local recovery action for one conflicting
native replacement which has not entered its handoff. It releases that operation's
reservation so a separately selected new operation can be considered. It neither
reinterprets the old semantic proof nor retries, deletes, renames, restores, or
uploads a cloud item.

The action requires the selected active, writable iCloud account and its exact
account UUID. Recorded receipt lookup also works after downgrade to an enabled
read-only account, without starting a writer or migrating its journal. Read-only
accounts gain no new write capability. This surface does
not enable normal native editor writes or a desktop action.

```
cirrove abandon-native-stage --label LABEL --account-id ACCOUNT_UUID --operation OPERATION_UUID
cirrove native-stage-abandonment --label LABEL --account-id ACCOUNT_UUID --operation OPERATION_UUID
```

The first command creates a bounded job. The daemon prepares the exact durable
Conflict row, reads its current account-bound encrypted checkpoint, and requires a
Stage checkpoint whose package registration was armed. It inspects the original
and allocated stage without downloading content. The original must retain its
captured identity, revision, name, parent and size. The stage must retain its exact
allocated identity, generated name and parent and still be a PACKAGE. The generated
name uses the canonical `.pages`, `.numbers` or `.key` suffix of the captured
original; its complete spelling and operation UUID must match exactly. Metadata is
fenced before and after this inspection; the checkpoint is reread. Only then does
a local transaction recheck the row, dependencies, current mount and cancellation
before recording abandonment. Any unsupported or uncertain case refuses.

The second command only reads the recorded local outcome. Use it after a lost
reply, stopped job or daemon restart before considering another action. A late
stop can race a completed local transaction: the durable receipt takes precedence
and is never reported as rolled back. A missing receipt is not permission to retry
a cloud mutation. Both verbs advertise separate capability version 1.

The receipt exposes account and operation UUIDs, original and retained-stage
metadata, and the observation timestamp. These are historical observations, not a
guarantee of current cloud availability. No checkpoint, signed URL, credential or
archive content appears in the reply. The retained stage is **not** a confirmed
Trash backup; the original was not trashed by this action.

The archive, encrypted checkpoint, stage locator and original recovery reservation
remain retained. Export the saved archive with the existing exact-operation API:

```
cirrove export-save --label LABEL --operation OPERATION_UUID --destination /absolute/new/archive.pages
```

For a stopped account, the existing `export-save --offline --state STATE` path
remains available. There is no automatic cloud-stage cleanup or replay, and this
command does not automatically enqueue a fresh replacement.

Validation is synthetic until the separately registered owned-provider arm passes.
The socket fixture uses an injected typed evidence producer; it does not validate
Apple behavior or access a desktop keyring. Provider evidence validation belongs
to the separate actual-HTTP adapter tests.

The [format-bound recovery regression](benchmarks/icloud-native-stage-format-recovery-2026-10-03.json)
records a matching Numbers/Keynote defect: the Stage constructor used the correct
format, while the final abandonment record still required `.pages`. The correction
uses the same suffix rule at both boundaries. Its local HTTPS fixtures verify
observation-only behavior and retained original content; they do not establish
live Numbers/Keynote abandonment or application fidelity.
