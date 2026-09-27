# iCloud Trash restore stopping-stage follow-up — 2026-09-27

Registered before the live run. The first owned-file Trash/restore cycle
ended indeterminate. A read-only Trash count afterward matched the count
before it, but that cannot identify the owned file or the failed stage.
This follow-up creates a different, fresh Cirrove-owned folder and file;
it never targets an existing user item.

Question: At which checked transition does the native restore cycle first
become uncertain: Trash request, Trash-list completeness, restore response,
parent listing, restored metadata, or restored bytes?

One arm: repeat the preregistered exact-ID/byte cycle with the same guards
and one request per operation. Return a fixed, non-sensitive stage code if
the result is uncertain. No response body, file name, item ID, token or
content is logged. There is no comparative arm and no automatic retry.

Prediction: because the read-only Trash count returned to its prior value,
the restore request may have succeeded but a post-restore metadata check
failed. The stage code, not this guess, decides the next action. One run
does not establish reliability.

A fresh private manifest under `.local-state/icloud-trash-restore-stage/`
records command, binary SHA-256, PID, expected duration and disk-backed
temporary filesystem before the run.

## Observed stopping phase

The process exited after 128.9 seconds at `restore_metadata`. The restore
request was accepted and one item appeared in the original test folder, but
the combined kind/document-ID/name check failed. No request was replayed.
The next run will distinguish those fields and compare the full restored
bytes before classifying identity. This run alone cannot claim recovery.
