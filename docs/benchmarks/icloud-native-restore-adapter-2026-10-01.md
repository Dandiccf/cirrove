# Native iCloud restore adapter validation — 2026-10-01

## Question and boundary

Can a durable restore operation return an exact native document from recoverable
Trash and verify its contents without replaying an uncertain request? The adapter
is not routed through the normal service yet. Destination vacancy checks do not
prove atomic collision protection. The feature caller is restricted to the owned
public-Trash fixture f2dec2e2-b870-4f1f-bfb5-e6d63f008941 and requires its exact
Applied removal receipt and retained semantic identity.

## Synthetic evidence

`native-restore-adapter-compile-fixed`: nine adapter tests passed, including saved
arm cancellation, failed persistence, lost reply/restart inspection, route binding,
authentication/storage failures, exact receipt and semantic verification.
`native-restore-caller-tests`: three caller provenance and single-use tests passed.
These are synthetic HTTP/local tests, not real-account restore evidence.

Two negative controls actually failed with exit101:

- `native-restore-caller-negative-binding`: removing the exact selected-node
  comparison accepted a retargeted identity; the test failed at field2.
- `native-restore-negative-durable-arm`: ignoring persistence failure allowed
  execute to succeed; the failed-arm test rejected that behavior.

Both source changes were restored in finally blocks. The restored positive run
`native-restore-restored-positive` passed all18 matching iCloud tests and all3
caller tests with write-probe features enabled. Manifests and logs are retained under
`.local-state/icloud-access-<arm>-2026-10-01/`; each records source hashes,
command, private disk-backed temporary directories and process completion.

## Preregistered controlled live arm

Use only the freshly imported and trashed Pages fixture above. Require an exclusive
read-only journal lease, exact import c52350d0-8b9d-4278-8c49-578adcf94c8f and Trash
b715c3e2-702e-475d-ad6d-3fa5feff3253, current recoverable identity/semantics and
unchanged owned destination. Persist a permanent single-use claim before provider
requests, then arm and dispatch once. Follow with a fresh adapter inspection only.
Prediction: the same provider identity returns with matching normalized package
contents. Endpoint: independent current identity/content verification, not merely
an HTTP success. Any uncertainty forbids another execute. No ordinary service,
installed daemon, unrelated documents or general restore admission changes.

## Full project validation

`scripts/check.sh` passed at 2026-10-01T15:38:54.946785+00:00 (arm
`native-restore-fullcheck`): workspace and feature tests, Clippy, real FUSE
mount tests, script/integration checks, ledger and docs. Existing rustdoc link
warning remains; graphical window scenarios are separate. No installed service
was restarted. This is not live restoration or release acceptance.

## Controlled live result

The owned f2dec restoration passed on 2026-10-01. Its single restore operation
`2cdd34c3-7c94-4ee7-a481-efabd35b5e4d` returned the exact selected provider
identity and matching root-normalized package contents. A fresh process then
reconciled that same operation with read-only inspection and independently
confirmed the same identity and semantics; it sent no mutation.

Restore confirmation: `/var/tmp/cirrove-public-native-trash-f2dec2e2-b870-4f1f-bfb5-e6d63f008941/verify-7145f813-eaa0-4285-906a-5607ee212c68/restore-confirmed.json`;
SHA-256 `c2217f319d2a368c63ae7dccbeaab1cfbeee4e54ac2e20971c8293a1fd2d2c9c`.

Fresh-process inspection: `/var/tmp/cirrove-public-native-trash-f2dec2e2-b870-4f1f-bfb5-e6d63f008941/verify-7d9c36bb-07c3-4564-9187-839b508544aa/restore-inspection.json`;
SHA-256 `fe8ad3462018d63c48b67555f73fc780f678fcb61cd45ea5a8231ff5d56d7e17`.

Arms `native-restore-owned-f2dec-live` and
`native-restore-owned-f2dec-inspect` both exited0; their manifests retain binary
SHA-256, source hashes, disk temporary filesystem and timings. One root listing
took29.405 seconds; this successful arm is not latency or reliability statistics.
No installed daemon was restarted. Normal-service Restore routing, atomic
destination collision protection and mounted restore publication remain unproven.
The fixture is now active again; its earlier Trash receipt records a historical
removal and must not be presented as proof that it is currently absent.
