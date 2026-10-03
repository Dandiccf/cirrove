# Lost replacement-stage registration confirmation

Registered before live execution, 2026-10-01. Question: can the ordinary account
router recover an already registered replacement stage after confirmation loss,
finish its conditional handoff and preserve the predecessor without uploading or
registering another copy?

Use one fresh Cirrove-owned folder. Create and independently hash the 32-byte
original. Queue a 64 MiB + 17-byte position-dependent replacement. Discard only
this operation's staging-registration response with the feature-gated adapter
fault. Require durable `verify_required` with no remote receipt and an encrypted
checkpoint containing the completed upload-body receipt and allocated document ID.
Independently verify exactly two files: unchanged original and registered stage,
including full content hashes and exact identities/revisions. Record the boundary
and exit 86. Do not recover a fixture that fails these requirements.

A fresh process checks the same journal, request, checkpoint, original and stage,
and exports the sealed local replacement with verified size/hash before recovery.
A guard rejects begin, stream, part and Stage commit calls. Inspection may advance
to a Handoff checkpoint only for the captured staged document ID. Only those
Handoff commits are allowed; the production adapter independently validates the
entire saved plan, request, folder and revisions. Require `uploaded`, no prohibited
replay attempts, at least one inspection and one handoff commit, the same staged
ID as the final current file, exactly one file in the owned folder and the full
independent final hash. Rename may change the stage's ETag: the final receipt must
match the independently observed final ETag, not the pre-rename ETag. Require the
original exact ID in Trash. Original bytes are independently verified before
handoff; the provider checks their hash during handoff. An independent post-handoff
Trash-body download is not part of this arm.

Prediction: inspection recognizes the staged file, constructs the handoff and
finishes it without a repeated upload/registration. The original remains
recoverable in Trash. This is one functional fault/restart arm after persisted
uncertainty, not physical power loss or proof of every TCP interruption timing.
Independent observation proves registration even if an actual transport error
precedes the deliberate response discard; it does not prove response arrival.
No performance or repeatability claim.

Deadline 1200 seconds per process. Record command, PID, binary SHA, private
non-tmpfs TMPDIR and SQLITE_TMPDIR before starting. No concurrent compilation or
measurement. Retain every failed fixture and manifest. No regular daemon,
installation or mount change; no permanent deletion or personal-file mutation.

## Result

Run `df3c953e-5c75-4bb0-9e12-98c20b7dcaa3` passed, 03:41:50–03:46:36 UTC,
285.358 seconds including setup. Binary SHA-256:
`496b552a921700b4814932b81504862b27ca07b7308dc02f72343b381ad3731d`.
The [manifest](icloud-replace-registration-recovery-df3c953e-5c75-4bb0-9e12-98c20b7dcaa3-live.json)
records first-process exit 86 after independently verifying the registered
67,108,881-byte stage and unchanged original. Fresh recovery exited 0: one
inspection, three allowed handoff commits, zero reconciliation calls and zero
prohibited replay attempts. The already registered stage's exact ID became the
current file. Its ETag changed during rename; the final receipt and independently
observed revision matched. Exactly one current file remained, its full hash
matched, the local export passed and the completed checkpoint was cleared.
The original exact ID was found in Trash. This arm did not independently download
its Trash body after the handoff; the provider's handoff validation checked it.

Private journals, sealed checkpoint evidence and exported bytes remain in
`.local-state/icloud-account-replace-registration-recovery-df3c953e-5c75-4bb0-9e12-98c20b7dcaa3/`;
logs/manifests in `.local-state/icloud-replace-registration-recovery-df3c953e-5c75-4bb0-9e12-98c20b7dcaa3/`.
Temporary storage was btrfs; no concurrent compilation/measurement ran.

Five targeted guard tests passed. The negative control replaced the new
handoff allowance with the previous blanket commit refusal: the exact registered
handoff acceptance test failed, while malformed/foreign/stage refusal still
passed. Logs: `.local-state/registration-handoff-{red,green}.log`.
These are validation-guard tests, not a new production recovery algorithm.
The live arm validates the existing adapter/journal recovery with a new
feature-only fault point and a restrictive replay guard.

After execution, the result serializer was clarified to calculate
`same_remote_revision` from the observed before/after ETags instead of assuming
it from the replacement flag. Both retained receipts confirm `false` for this run;
the evidence value is unchanged. No provider behavior changed after the live arm.

Full `scripts/check.sh` passed 03:47:09–03:53:40 UTC: formatting, clippy,
workspace/feature/kernel tests, script checks, translations, ledger and docs.
Private manifest/log: `.local-state/icloud-replace-registration-check-2026-10-01/`;
TMPDIR/SQLITE_TMPDIR were private btrfs storage. The pre-existing service rustdoc
link warning remains. No GUI behavior changed, so native-window scenarios were
not rerun. No installation changed; this is isolated validation, not installed
release acceptance.
