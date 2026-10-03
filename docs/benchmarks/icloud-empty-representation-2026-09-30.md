# Ordinary-file proof for zero-length write sources

## Change and negative control

The normal revision-bound file hash used during write preflight previously
skipped the download-representation lookup when metadata size was zero. It could
therefore return the empty SHA-256 for a package or ambiguous representation.
Logical length alone cannot authorize ordinary-file replacement.

The representation lookup now precedes the size branch. Every source must supply
one validated ordinary Data URL; package, ambiguous or missing locations fail,
including size zero. Empty ordinary files still avoid a content transfer and
retain the two matching parent/revision/size observations. The synthetic fixture
failed before moving this lookup (a zero-length package was accepted), then all
three download tests passed. The fixture checks package/ambiguous/missing refusal,
a single metadata lookup and successful empty ordinary hashing. This does not
by itself prove Apple's behavior for a real empty file.

## Registered live arm

Question: does a real empty ordinary file still qualify under the stronger
representation check, and remain readable through the account router and FUSE?
Prediction: the existing empty-create protocol reaches Uploaded; a new process
verifies its exact ID/revision, receives an ordinary Data representation, computes
the empty digest and reads stat size zero / EOF through the mounted fixture.

Use a fresh run UUID with the existing `--account-empty RUN_UUID` validator,
followed only after success by `--account-empty-read RUN_UUID`. The first command
creates one owned `Cirrove Write Validation-<UUID>` folder and one empty test file.
The second reopens only that confirmed fixture/journal, checks its digest and
mounts it for an external Python EOF read. No user file or prior uncertain fixture
is changed; retain all new receipts/journals and remote test items. The installed
service/accounts remain unchanged. Do not retry a failed mutation automatically.

Record binary SHA-256, commands, phase PIDs, private disk-backed TMPDIR and terminal
results before execution. Outer bound 600 seconds for each phase, no concurrent
compilation or other measurement. This is functional compatibility validation,
not a performance/reliability comparison, and does not newly establish replacement
or Trash recovery (covered separately by existing mounted empty/refill arms).

## Live result

Run `6f5a1073-8aca-4639-b458-ee8577bf95a9` passed both phases (81.354 seconds
end to end, one functional arm). The regular create adapter confirmed its new
empty file, and a new process reopened the Uploaded journal receipt, passed the
stronger independent ordinary-representation/revision/digest check and read zero
size / EOF through the real mounted fixture. The fixture mount shut down cleanly;
its new remote folder/file and local evidence are retained. Public lifecycle and
report: `icloud-empty-representation-live-2026-09-30.json`. This closes the empty
ordinary-file compatibility check for this change, not native-package writes or
a new replacement/recovery acceptance claim.

## Repository validation

Full `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` passed,
including workspace/feature tests, real kernel mounts, scripts, ledger and docs.
Private log: `.local-state/icloud-empty-representation-full-check.log`. The known
rustdoc `Writeback::retry_stuck` link warning remains; GUI window scenarios were
not run. This change is not installed into the ordinary daemon and does not
change the disabled production iCloud write grant.
