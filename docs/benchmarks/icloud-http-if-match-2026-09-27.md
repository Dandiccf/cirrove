# iCloud HTTP If-Match content update experiment — 2026-09-27

Registered before the run. The previous live trial showed that a stale ETag
inside the JSON body did not protect `update/documents`. This trial adds a
standard HTTP `If-Match` header containing the old ETag. Only a newly created
Cirrove validation file is targeted; all existing user files and normal
services remain untouched.

Arm: create and read an owned file, update it once under the same ID, verify
the new bytes and ETag, then submit a same-size candidate with the old ETag
both in the JSON body and in a quoted `If-Match` header. Independently list
and read after the request. No retry and no comparative arm.

Prediction: Apple's private document endpoint may ignore this standard header
and accept the stale update. If rejected, the trial alone does not prove
conflict safety: a fresh `If-Match` control and a competing-device race would
be needed to show that the rejection was conditional rather than due to an
unsupported header. Endpoint: stale accepted with exact candidate bytes,
rejected with exact prior bytes/ETag, or indeterminate. The fixture remains.

The ignored `.local-state/icloud-http-if-match-validation/manifest.json`
records the binary hash, command, PID, disk-backed private temp filesystem,
expected duration and timing. No tokens, signed URLs, raw provider bodies or
file contents are recorded.

## Observed first run

The pre-registered run completed in 79.9 seconds. The first same-ID update
produced exact revised bytes and a new ETag. The second same-ID update carried
the original stale ETag in both the JSON body and a quoted HTTP `If-Match`
header. Apple accepted it; independent listing retained the same Drive and
document ID, and a fresh read returned the exact second candidate bytes.
The tested request shape therefore does **not** enforce the HTTP precondition
on this account. The fixture remains in iCloud Drive. This one run supplies
no within-arm spread and does not exclude an undocumented different API.
