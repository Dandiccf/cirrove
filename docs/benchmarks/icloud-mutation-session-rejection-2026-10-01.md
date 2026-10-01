# iCloud namespace writes preserve session rejection

Following the upload fix, review found the same erased typed session failure in
folder creation and file/folder rename, move and recoverable removal. These
adapters now preserve `SessionRejected` as `MutationError::Provider(Authentication)`.
The account router's independent original-content verifier uses the same mapping.
Non-authentication failures remain uncertain; no response text is propagated.
Signed content fetch transport errors remain uncertain as before.

The mutation worker's existing behavior records Failed, stops automatic attempts
and keeps the request and any prepared identity. Explicit retry enters verification,
not a new mutation. Successful iCloud sign-in currently retains its read-only
preview policy and does not automatically retry write failures. This change does
not enable writes, alter grants, add automatic retry or change the shared worker.

## Synthetic evidence

The HTTP folder preflight fixture checks 401/403 versus 503. With the old mapping,
`folder_preflight_preserves_session_rejection_without_claiming_a_conflict` failed
at 401, returning Uncertain. After the adapter changes it passes; 503 stays
Uncertain. Typed-cause tests also cover wrapped session errors and refusal to infer
authentication from arbitrary strings. Raw messages are never returned.

`session_rejection_preserves_namespace_intent_and_prepared_identity_across_restart`
passes for relocation, file removal, folder removal and prepared folder creation.
Each arm loses its first mutation confirmation, then rejects reconciliation with
authentication. The worker reports Failed and does not spin. After reopening the
journal, the same intent and reserved identity remain. Restored authentication
and explicit retry acknowledge the first result with exactly one mutation call;
folder creation does not reserve a second identity. These are shared-worker
safety tests with a synthetic provider, not real Apple expiry tests.

Logs retained:
- `.local-state/icloud-mutation-auth-red.log`: expected pre-fix failure.
- `.local-state/icloud-mutation-auth-green.log`: intermediate compile failure
  caught two raw reqwest transport errors; their uncertain classification remains
  unchanged rather than passing them through session classification.
- `.local-state/icloud-mutation-auth-green-2.log`: passing HTTP regression.
- `.local-state/icloud-mutation-auth-recovery.log`: intermediate test compile
  error; MutationIntent deliberately has no Debug. The assertion now compares
  equality without adding potentially sensitive debug output.
- `.local-state/icloud-mutation-auth-recovery-2.log`: passing restart test.

Live session expiry, installed reauthentication UX and the broader release gates
remain open. No real-account mutation, installation or service restart occurred.

Full `scripts/check.sh` passed 04:10:28–04:17:16 UTC, covering formatting, clippy,
workspace/feature tests, kernel mounts, scripts, translations, ledger and docs.
Manifest/log: `.local-state/icloud-mutation-auth-check-2026-10-01/`; TMPDIR and
SQLITE_TMPDIR used private btrfs storage. The existing service rustdoc link warning
remains. No GUI change; native-window scenarios were not rerun. Installed
acceptance remains open.
