# Isolated iCloud handoff inner phases, 2026-09-29

## Registered before live execution

Question: the preceding run had two 125-second worker deadline drops in
the two-ID replacement handoff. Was each deadline consumed by a remote
preflight observation, the Apple mutation request, or postflight/receipt
verification? Two later inspections recovered a completed result, but the
previous provider-phase timing cannot separate those inner awaits.

The feature-gated owned handoff probe now emits only timing labels for
preflight, request, postflight and receipt; the existing outer probe emits
only phase name, duration and boolean outcome. No provider body, token,
cursor, URL or remote identity is printed. Use a new Cirrove-owned fixture
under private run `0b187a71-5f41-49ff-9b3f-260169d43fc3`: A creates and
independently verifies the source file, B replaces it through FUSE and
requires exact new bytes, a two-ID journal receipt and the recoverable old
ID, C remounts in a fresh process and checks the result without mutation.
Stop on a failed or uncertain arm.

Prediction: full remote observations in the handoff, especially Trash
backup verification, consume a substantial share of each 125-second
deadline; a mutation request alone may be short. This remains a hypothesis.
Each arm records command, binary SHA-256, PID, expected duration and
separate disk-backed `TMPDIR`/`SQLITE_TMPDIR` before execution. Bounds are
360 seconds for A and C, 900 for B. No compilation or second measurement
overlaps. The installed daemon and ordinary mounts remain untouched and
read-only iCloud remains unchanged. A single run can identify the slow
awaits; it supplies no within-arm spread or reliability claim.

## Observation

All three arms passed for owned fixture
`21897b4c-78a1-4aa9-a62b-7fa40f8bfaf3`. Arm A created and independently
verified the source. Arm B completed mounted replacement with exact new
bytes, a two-ID journal receipt and recoverable old identity. Arm C
remounted in a new process and independently verified the result without
mutation. The test mounts shut down; the ordinary service stayed running.

For `MoveOld`, the preflight took 4.9 seconds and the accepted Apple Trash
request 19.7 seconds. Its subsequent full postflight observation was still
running after 100.4 seconds and the worker dropped the whole commit at its
125-second limit. The next inspect completed the full observation in 64.7
seconds and advanced to `InstallNew`. There, the preflight observation took
65.3 seconds, the accepted Apple rename request 2.2 seconds, and the full
receipt check was still running at 57.5 seconds when the next 125-second
commit deadline dropped it. The final inspect completed that observation
in 108.3 seconds and recognized the finished replacement. Upload confirmation
arrived at 478.9 seconds; independent listing at 479.4, Trash confirmation
at 510.8 and byte verification at 512.5 seconds. The journal retained two
failed attempts.

This supports the prediction: repeated full remote observations, not the
small content transfer or Apple mutation requests, consumed the worker
deadlines. The checks were necessary for conflict and backup integrity;
this run does not justify deleting them. It supports separating mutation
and full verification into durable phases so each await gets its own
bounded worker call, while retaining exact-ID verification before the next
mutation. That change needs its own pre/post comparison and recovery tests.

Private manifests `arm-a.json`, `arm-b.json` and `arm-c.json` are in
`.local-state/icloud-handoff-inner-phases-0b187a71-5f41-49ff-9b3f-260169d43fc3/`.
Each records binary SHA-256
`312cb66274cbfa7486732ce6552ac5001f60d5338028777c250546eab8bf518a`,
PID, bound and separate btrfs-backed temporary directories. No second
measurement or compilation overlapped. One run per arm offers no within-arm
spread or ordinary-account reliability claim.
