# iCloud upload session rejection retains recovery state

The ordinary create, staged replacement and handoff adapters previously erased
`SessionRejected` into `UploadError::Uncertain`. An expired/rejected account
session therefore looked like an ordinary ambiguous transport result and remained
eligible for automatic verification retries. The read adapter already preserved
this typed distinction.

The write adapters now preserve the same typed cause as
`UploadError::Provider(ProviderError::Authentication)`. The existing transfer
worker records `Failed` and reports its fixed authentication message; it retains
the payload and encrypted upload checkpoint. Explicit retry requests verification,
not a fresh upload. This does not add automatic reauthentication or automatic
retry after sign-in. No new error text includes provider bodies, URLs or secrets.
Other errors remain uncertain. In particular, a signed upload-content URL's
rejection remains uncertain: that URL can expire independently of the Apple
account session.

## Synthetic validation

- The exact-folder HTTP fixture now returns 401, 403 and 503 in addition to the
  existing identity/malformed-envelope cases. With the previous adapter mapping,
  `parent_validation_queries_the_exact_folder_and_rejects_changed_identity`
  failed at 401 (`Uncertain` instead of `Authentication`). It passes after the
  fix; 503 remains uncertain. Logs:
  `.local-state/icloud-write-auth-{red,green}.log`.
- Typed-cause tests preserve rejection through an anyhow context and refuse to
  classify arbitrary strings (even the same words or HTTP status) as authentication.
- `rejected_session_keeps_uncertain_commit_and_local_bytes_until_verified_after_sign_in`
  first commits remotely with a lost confirmation, then rejects inspection with an
  authentication error. The worker stops automatic attempts, reports the fixed
  issue and retains the payload and checkpoint. The test closes and reopens the
  journal, restores authentication, explicitly retries and acknowledges the
  original remote result by reconciliation. There is one begin and one set of
  byte transfers; no duplicate upload. Local bytes remain readable throughout.
  This is a compatibility/safety test of existing worker behavior, not a worker
  behavior change. Log: `.local-state/icloud-write-auth-recovery.log`.

These fixtures do not prove prolonged real Apple session expiry or installed
reauthentication UX. The session/capacity release gate stays open. No live account
session was expired, no personal data changed and no installed service restarted.

Full `scripts/check.sh` passed 03:58:18–04:04:56 UTC: formatting, clippy,
workspace and feature tests, real kernel mounts, scripts, translations, ledger
and documentation. Manifest and log:
`.local-state/icloud-write-auth-check-2026-10-01/`, with private btrfs TMPDIR and
SQLITE_TMPDIR. The pre-existing service rustdoc link warning remains. No GUI
behavior changed; native-window scenarios were not rerun. Installed acceptance
remains open.
