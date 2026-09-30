# Unconfirmed write status — 2026-10-01

The daemon now reports a separate unconfirmed-change count from both upload and
mutation journals. VerifyRequired and Verifying are counted; pending transfers,
active dispatch, failures and conflicts remain distinct. Account events include
changes to the count. Old status/events default the additive field to zero.
The desktop binds it only to an identity-matching account, shows a non-destructive
notice and clears it after confirmation. Recent upload activity no longer calls
verification an upload or appends completed transfer progress to that state.

## Evidence

- The journal regression uses real journal transitions: pending, dispatch,
  restart, inspection and acknowledgement for an upload and a folder mutation.
  Count sequence 0, 0, 2, 2, 1, 0; failure/conflict counters remain zero.
- The event regression checks entry into and exit from uncertainty without a
  failure or mount-state change.
- All 11 native window scenarios passed. The new scenario checks a synthetic
  iCloud count, distinct notice, no Retry/Discard actions and clearing.
- Initial window attempts needed a harness correction: wait for the expander
  animation and supply a matching provider discriminator. The corrected test was
  then run with the model count deliberately forced to zero; it executed and
  failed waiting for the notice (exit 101). Restoring the real mapping passed.
  This negative control establishes that the test detects the missing notice,
  rather than relying on the earlier harness failure.
- [Rendered synthetic window](icloud-confirmation-ui-2026-10-01.png) was inspected.
  The accessibility-bus connection warning on this host did not prevent rendering
  or any of the window scenarios.

The first full repository check passed the code and kernel tests, then failed
translation completeness because the three new strings were missing. The German
catalogue and extraction template were updated (including removing an incorrect
fuzzy match). Full repeated `scripts/check.sh` passed, exit 0, UTC 2026-09-30
23:00:54–23:06:29, including formatting, clippy, workspace/feature tests, real
kernel mounts, scripts, translations, ledger and documentation. Private records:
`.local-state/icloud-confirmation-ui-check-2026-10-01/` (initial failure) and
`.local-state/icloud-confirmation-ui-check-2026-10-01-repeat/` (pass).
The preexisting rustdoc `retry_stuck` warning remains. The 11 GUI scenarios were
run separately and passed. This branch has not been installed into the regular
daemon; installed release acceptance remains open.

No cloud mutations were performed for this UI work. Per-operation recovery export,
full conflict-resolution acceptance and installed iCloud write validation remain
open. This is not a claim that ordinary iCloud writes are enabled.
