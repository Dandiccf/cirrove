# iCloud phased streamed Create recovery — 2026-09-28

Registered before the live runs. Each arm creates one new UUID-named
Cirrove-owned folder and one generated 20 MiB binary payload in the isolated
`iCloudGuiValidation` account. Neither arm addresses an existing user item,
the ordinary mount or another cloud client. Both processes use the saved
local Apple session; no credentials or provider responses are logged.

Question: does separating Apple's content POST from its visible `add_file`
registration preserve an exact content receipt in the credential vault before
registration, and allow an uncertain content-only phase to use a fresh slot
without creating duplicate visible files? The earlier combined stream could
not distinguish an accepted content POST from a sent registration when its
response was lost. The legacy small-file and old v2 checkpoints remain
conservative; only a new v3 streamed checkpoint without a receipt proves this
client did not send `add_file`.

Arm A deliberately discards an already received registration response after
the worker has saved the content receipt. A second process reconciles the
allocated document ID and full SHA-256 without resending content or
registration. Prediction: first journal state `VerifyRequired`, saved receipt
present, then one exact-ID `Uploaded` receipt from the second process.

Arm B deliberately discards an already received content response before its
receipt is checkpointed. It sends no `add_file`. A second process verifies
absence of the reserved visible ID, marks the old attempt uncommitted, then
allocates a fresh slot and completes the one owned file. Prediction: first
state `VerifyRequired` with no saved receipt; retry reaches `Uploaded` with a
different document ID and exactly one visible item in the test folder.

These are functional single arms, not network-timeout, process-crash,
repeatability, memory or latency measurements. A private manifest is written
before each process starts with command, PID, binary SHA-256, expected duration
and disk-backed TMPDIR/SQLITE_TMPDIR. No parallel compilation occurs during
either arm. Endpoint is the exact-ID/full-content provider receipt and
independently reopened SQLite record. Normal GUI/FUSE writes remain disabled.

## Observed

All four isolated processes exited zero using btrfs-backed temporary storage.
Their private manifests record the same test binary SHA-256
`e7288ff9b9a24bc220c7d6b3bad428d6b0b387b93bede2e3955eaa7a69c58452`.

Arm A first stopped at `VerifyRequired` for operation
`495dae25-2b04-4e3a-89a5-e0982d2758f8`; the validator confirmed the
content receipt was saved in the private vault, without showing it. A new
process reconciled the exact remote ID without issuing another upload or
registration. Read-only SQLite reopening found `uploaded`, remote ID
`FILE::com.apple.CloudDocs::A94CE812-8B75-4416-A5D7-6516E6780DBF`,
20,971,520 bytes and SHA-256
`995219f6d6158bae228fc93356bfd3da46f495b571460849a90380fac7b9c02b`,
matching the retained local snapshot.

Arm B first stopped at `VerifyRequired` for operation
`4789e63c-b417-454e-9323-41b7102c89cf`; its saved v3 checkpoint contained
the reserved document ID but no content receipt. A fresh process classified
that content-only phase as uncommitted, allocated another ID and reached
`Uploaded`. An independent folder listing found exactly one visible item,
with an ID different from the abandoned slot. Read-only SQLite reopening
found remote ID `FILE::com.apple.CloudDocs::CAD7D42F-214C-447D-BBED-C6DE4033946E`,
20,971,520 bytes and SHA-256
`e1b810fbb2b75096878735a38dfce1467688805d49a7d79ab3ba8b4b714c4e9a`,
matching the retained local snapshot.

This proves the two injected lost-response paths for new owned streamed
files. It does not prove behavior when a real network request remains active
past a worker deadline, nor continuous process termination at every
instruction boundary. It also does not establish general mounted writes or
cross-account reliability. The earlier v2 combined flow never interprets a
missing receipt as permission to retry.
