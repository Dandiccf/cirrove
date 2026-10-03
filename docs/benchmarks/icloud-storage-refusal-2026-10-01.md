# iCloud storage refusal and retained edits

Question: does an explicit HTTP 507 storage refusal stop automatic writes while
retaining recoverable local bytes and provider checkpoints for a user-directed,
verification-first retry? Prediction: the previous generic uncertain mapping
retries; the typed refusal waits for explicit action without assuming whether an
earlier upload or namespace mutation committed.

## Registered synthetic arms

- Controlled negative: retain new error variants/tests but remove the HTTP 507
  classification and worker terminal cases. The HTTP adapter and retained-state
  tests must fail at behavior assertions, not compilation.
- Restore mappings: allocation, registration, folder create/rename/move/Trash
  refuse storage distinctly. Folder Trash exercises preflight, send and follow-up.
- Signed upload URL expiry remains uncertain; it is not an account-session failure.
- Unknown status text and Apple JSON codes remain uncertain; no string guessing.
- Upload checkpoint/payload and namespace prepared identity survive restart.
  Local sealed bytes export successfully. Restored storage alone does not restart
  work; explicit retry first reconciles the earlier attempt and avoids duplication.

These are bounded local HTTP/provider fixtures, not evidence of the response Apple
returns when a real personal account quota is full. No user's storage is filled.
The signed-content check covers the mapper, not a full HTTPS upload exchange.

## Results

The base implementation was integrated after desktop commit `5c4fd21`.
Removing the central HTTP 507 mapping caused the allocation test to fail at
`UploadError::InsufficientStorage` (exit 101). Removing the worker terminal cases
caused both mutation and upload tests to observe `VerifyRequired` instead of
`Failed` (exit 101). Restoring the implementation passed all seven matching tests
across cirrove-icloud and cirrove-service. Logs:

- `.local-state/icloud-storage-refusal-http-red.log`
- `.local-state/icloud-storage-refusal-worker-red.log`
- `.local-state/icloud-storage-refusal-transfer-red.log`
- `.local-state/icloud-storage-refusal-green.log`

Independent review found adjacent gaps subsequently corrected: direct conditional
namespace HTTP 401/403 lost their authentication type; signed verification GETs
lost HTTP 507. Signed URL authentication failures must remain uncertain. Initial
folder creation without a returned provider identity remains indeterminate on
retry; retained-prepared-identity tests do not establish automatic recovery of
that case. No unsafe replay is enabled.

Removing the conditional 401/403 mapping failed the HTTP rename/401 assertion.
Removing signed-download 507 classification failed both file-Trash and replacement
backup boundary tests. Restoring both changes passed the complete cirrove-icloud
suite (110 tests). These signed-download tests exercise the classifiers used by
normal paths, not complete HTTPS exchanges. The bounded folder-create preflight
507 test confirms that retry with no identity remains indeterminate and sends no
second request. Logs:

- `.local-state/icloud-storage-namespace-auth-red.log`
- `.local-state/icloud-storage-signed-verification-red.log`
- `.local-state/icloud-storage-followup-green.log`



Full `scripts/check.sh` passed on 2026-10-01, 06:01:31–06:10:35 UTC
(exit 0), with the storage and reauthentication changes together. Evidence:
`.local-state/icloud-storage-reauth-check-2026-10-01/`. No installed daemon
or account access grant was changed. Rustdoc reports an existing broken private
Writeback link; the required check completed successfully.
