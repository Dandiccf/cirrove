# Normal-service native Trash live acceptance

Run: `f2dec2e2-b870-4f1f-bfb5-e6d63f008941`. Status: passed normal-service owned Pages import/Trash/readback; initial cold preflight refusal remains unexplained.
Exact new name: `Cirrove Public Trash f2dec2e2-b870-4f1f-bfb5-e6d63f008941.pages`.

Question: does a fresh native Pages document created through the public Cirrove
import command survive exact-revision removal into recoverable iCloud Trash,
with independently verified complete native contents and durable operation
recovery? Prediction: one new import and one conditional Trash operation, with
no re-dispatch after an uncertain result.

Use only the existing owned source archive from ac9e5456-bd10-4b7d-9215-21bbb85dde69
and its recorded raw/semantic identity. Create the new document only in that
run's owned validation folder. Never remove an existing fixture or personal file.
The validation mount uses CloudDocs root, so this is not a mount-scope sandbox;
all mutation commands must bind the exact new name, account, identity and ETag.

Fresh isolated state: `/var/tmp/cirrove-public-native-trash-f2dec2e2-b870-4f1f-bfb5-e6d63f008941`.
Bootstrap creates the directory and fresh account/credential IDs once, using the
existing sealed-session identity checks. Do not pre-create its directory. The
installed daemon, existing accounts and ec7 GUI session remain untouched.

Before any daemon or cloud operation: finish helper tests/build, register exact
commands and binary hashes, private disk-backed TMPDIR/SQLITE_TMPDIR filesystem,
runner and daemon PIDs, and expected duration. No overlapping build/measurement.
Stop only the directly spawned daemon by recorded PID; never recursive cleanup.

Acceptance sequence:

1. Bootstrap once and stage the verified owned source as a private local file.
2. Start isolated daemon and submit one public import. Retain durable UUID; an
   uncertain reply is inspected rather than resubmitted.
3. Stop isolated daemon; independent import verifier checks journal binding,
   current exact native identity and complete semantic contents, saving immutable
   pre-Trash selection.
4. Restart isolated daemon, confirm mounted visibility and read a generated
   archive through an open descriptor; retain bytes/hash without displaying content.
5. Submit one exact selected-revision public native Trash command. Capture
   retained-operation listing, historical receipt/publication, mount disappearance
   and held-reader contents. Do not retry an uncertain mutation.
6. Stop isolated daemon. Independent read-only verifier requires exact Applied
   receipt, metadata absence and same-ID recoverable native contents in Trash,
   compared with the original synthetic source semantic identity.

This arm does not restore the document or prove collision-safe restoration,
native editing/replacement, Numbers/Keynote support or installed release reliability.

Helper verification: 54 targeted tests passed. Removing the exact fresh-name
predicate made the negative-control test fail; the predicate was restored.
Review identified an anonymous staging-file permission issue before any live call;
its explicit0600 fix and validation remain pending.

## Helper preflight results

All 55 targeted validation tests pass after the private anonymous-staging fix.
The mode regression first failed without `set_permissions(0600)` (0666 retained
instead of0600); restored code passes. Both this and the exact-name negative
control were restored before the green run. Feature-specific mounted-write-probe
Clippy also passes with warnings denied. This is preparation only; no cloud call
or daemon start has happened for this run yet.

## First live preflight outcome

At 2026-10-01T14:59:22.737592+00:00 the runner stopped before import while checking the
owned mounted parent. Bootstrap succeeded; the directly spawned isolated daemon
exited0 and unmounted. Both submission flags remained false. Independent
read-only inspection confirmed zero upload rows and zero mutation rows. The
owned parent had no row in this isolated metadata database.

The runner had not retained the failing `paths` response, so the precise cause
is not yet established. Initial root metadata timings were about30 seconds;
that timing alone does not diagnose authentication, pagination or indexing.
A separate read-only diagnostic must retain the exact owned-path response before
any continuation. No import or Trash retry has been authorized by a timeout.

## Controlled live result

The separate read-only diagnostic completed with both exact owned-parent path
queries successful (before and after warming the directory), source visible, new
destination absent and zero uploads/mutations/write_queue rows. The first failure
therefore remains a transient preflight finding, not a demonstrated missing folder
or fixed indexing defect. No production timeout or consistency guard was relaxed.

Continuation used the same isolated state without repeating bootstrap. Before
starting it verified both previous daemons were gone, all journal queues were
empty, and the staged synthetic archive exactly matched the retained source.
The new manifest durably armed each first mutation before launching its command.

At 2026-10-01T15:09:35.397450+00:00 the continuation passed:

- Public import operation `c52350d0-8b9d-4278-8c49-578adcf94c8f` created one fresh owned Pages
  document; independent readback bound its exact identity/revision and full
  root-normalized archive semantics to the retained synthetic source.
- Public native Trash operation `b715c3e2-702e-475d-ad6d-3fa5feff3253` completed, remained
  discoverable through `list-native-trash`, and passed observer-only watch.
- Mounted directory no longer contained the document. Its held generated archive
  descriptor read the same 55675 bytes and SHA-256
  `c2b120506046df34b73e870a5c378506790948e6eee64d243f6bf0b9d8fce0e7` before and after removal.
- Independent exact-ID Trash readback verified matching native contents and
  recoverable identity, plus the exact Applied journal receipt and recorded
  metadata absence. The verifier sent no cloud mutation.
- Both directly spawned isolated daemons exited0. No installed service restart,
  permanent deletion, old fixture deletion or restoration occurred.

Evidence: `/var/tmp/cirrove-public-native-trash-runner-f2dec2e2-b870-4f1f-bfb5-e6d63f008941-attempt2`;
independent receipt: `/var/tmp/cirrove-public-native-trash-f2dec2e2-b870-4f1f-bfb5-e6d63f008941/verify-0bd51a8e-b77b-487b-8749-2bd00b2f0e26/public-trash-verified.json`.
Receipt SHA-256: `40724f9819fd89c1ba90749d1a3d9de96bae7c963684924acedf84d512d9ca75`.
The manifest binds command argv, process identities, disk temporary filesystem,
binary SHA-256 and source hashes. This is one controlled successful owned Pages
arm, not reliability statistics, offline proof, native replacement/editing,
Numbers/Keynote acceptance or installed release acceptance.

## Post-live project validation

The full `scripts/check.sh` passed at 2026-10-01T15:17:49.050706+00:00
(`public-native-trash-live-fullcheck`): formatting, all required Clippy targets,
workspace and iCloud feature tests, real FUSE mounts, scripts/integration checks,
acceptance ledger and documentation. The pre-existing rustdoc link warning
remains. No new GUI behavior was introduced; window scenarios are outside this
script. The installed daemon remained PID3206061 and both isolated mounts were
gone after their recorded clean exits.
