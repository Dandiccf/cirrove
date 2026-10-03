# Mounted iCloud competing content edit and keep-both recovery

Registered before the live run on 2026-09-30. This is a correctness experiment,
not a throughput claim or a release-wide reliability result.

## Question and prediction

Does the normal account writer refuse a prepared replacement when the original
content changes under the same provider identity, and can the shared journal
recover the local save beside that newer cloud version after restart?

Prediction: the normal replacement adapter returns Conflict before moving the
old file to Trash. The sealed local payload remains readable. After restarting
the isolated mount, keep-both queues an ordinary Create with a distinct name and
identity, and both versions remain independently readable after another remount.

## Registered procedure and endpoints

- Create a fresh UUID-owned `Cirrove Write Validation-<run>` folder and a tiny
  ordinary `Account Router.txt` through the actual FUSE mount and account router.
- Independently hash its complete remote content, then save the replacement via
  the mount. Before the prepared MoveOld commit enters the normal adapter,
  record a durable one-shot marker and change one byte of the exact original
  using the feature-gated test helper. No personal file is eligible. Confirm
  the changed original and untouched staged file by exact identity and contents.
- Let the unmodified normal adapter proceed. Require Conflict, the local payload,
  the newer cloud content, and the original ID outside Trash. No synthetic
  Conflict result is injected by the wrapper.
- Shut down and remount; invoke ordinary keep-both for that exact failed UUID.
  Verify mounted names, wait for the rescue Create receipt, and independently
  hash both remote files. Shut down and remount once more; read both paths fully.
- Retain the owned staged file and all local evidence. Cleanup is a separate
  unresolved gate; do not pretend a successful rescue removes its intermediate
  remote object. No permanent deletion is used.

Any ambiguous edit result stops the mutation hook permanently for this run. The
helper never retries a same-ID edit. A failed arm is retained and is not reused.
The runner registers binary hash, command, PID, expected duration and private
btrfs TMPDIR/SQLITE_TMPDIR before launch. No compilation runs alongside it.

## Status

The first run `80b00ac2-3d46-40b0-a8c8-5396f4ab9963` stopped after the normal
adapter returned Conflict. Both contents were checked, but the harness incorrectly
used the positive-membership Trash probe to prove absence: that API errors on zero
matches. Exit 1 after 240.313 seconds is **not a pass**. Its fixture and journal
are retained, with one Uploaded and one Conflict record. A separate complete-listing
absence check now rejects incomplete listings and any matching Trash identity.
The next arm uses a fresh UUID and does not replay the failed fixture.

Synthetic ownership and one-shot checks do not establish the live endpoint.
A single successful run will establish this scenario only; repeatability,
atomic-editor conflict chains, newer local generations and staging cleanup remain
separate release requirements.


## Controlled result

Run `29227b46-c13b-43e0-b535-df49d98e1db4` passed with binary SHA-256
`eca83719d92831262a792917f909e5b8e691f46e1ca25a9f1c4da8872fc8fa61`.
The run took 281.608 seconds and exited 0 with its isolated mount detached.
This is one correctness run; the duration is not a latency benchmark and no
within-arm spread or repeatability claim is available.

The injected edit retained the original provider ID and changed its ETag/content.
The normal replacement adapter returned Conflict. Full local and cloud contents
were verified and a complete Trash listing proved the original absent. After
an orderly shutdown/remount, keep-both used the same working identity and payload
digest to create `Account Router rescued.txt` under a distinct remote ID. Both
remote contents were independently hashed, and both mounted names were read again
after another remount.

The independent read-only SQLite audit passed integrity checking and found the
three upload states Uploaded, Resolved, Uploaded. It confirmed exactly one remote
owner per version, distinct local identities and no incomplete queue reservations.
The abandoned staging object remains retained: cleanup is explicitly not passed.

Evidence:

- [Successful live arm](icloud-mounted-competing-29227b46-c13b-43e0-b535-df49d98e1db4-live.json)
- [Read-only journal audit](icloud-mounted-competing-29227b46-c13b-43e0-b535-df49d98e1db4-journal.json)
- [Earlier failed harness arm](icloud-mounted-competing-80b00ac2-3d46-40b0-a8c8-5396f4ab9963-live.json)

This closes the ordinary current-save conflict/rescue scenario, not all concurrent
editing. Newer pending generations, atomic editor replacement chains, safe staging
cleanup and installed desktop recovery flows remain open. No normal iCloud write
setting or installed daemon was changed.

## Code validation

`scripts/check.sh` passed on 2026-09-30 at 19:08:19 UTC: formatting, clippy,
workspace and feature-gated iCloud tests, actual kernel mounts, script tests,
ledger and generated documentation. The private retained manifest is
`.local-state/icloud-competing-check-2026-09-30/run.json`, exit 0. Display-dependent
window scenarios were not run; no desktop widgets changed. The pre-existing
rustdoc link warning for `retry_stuck` remains. No live probe was running during
this check, and both live arms ended with their isolated mounts detached.
