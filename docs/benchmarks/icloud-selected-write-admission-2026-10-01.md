# Selected-file write admission

Question: can a metadata-normal FILE with an unknown package extension be refused
before any local writable open/truncate/unlink/rename/replacement acceptance,
without downloading content or probing every directory entry? Prediction: a
provider-neutral selected-target hook, revision-bound metadata checks and local
identity revalidation close this gap while ordinary files remain writable.

Implementation: default no-op ReadProvider hook for other providers; iCloud
validates exact FILE ID/parent/name/etag/size before and after Data-versus-Package
lookup. Only Data is admitted. A bounded256-entry/30-second cache stores only
metadata; hits still fetch exact metadata. Cancellation/timeouts are bounded, and
no journal/projection lock crosses provider awaits. Local aliases resolve to
journal-backed remote identity; bindings are rechecked before journal mutation.

## Checks so far

Removing the iCloud Data-only guard caused the unknown-extension Package test
to fail at admission, with valid after-metadata available (not an exhausted-fixture
false negative). Restoring it passed all seven provider tests.

Removing the shared provider call caused the real FUSE scenario to fail at the
first forbidden edit. Restoring it passed the eight refusal operations and alias
checks, but the race-fixture setup then failed with JournalError::Stale while
trying to enqueue a second relocation. That green attempt is failed evidence,
not acceptance. The fixture setup is being corrected without weakening the
required stale-binding refusal.

Logs: `.local-state/icloud-access-write-target-package-red-2026-10-01`,
`icloud-access-write-target-green-2026-10-01`, `icloud-access-admission-bypass-red-2026-10-01`,
and `icloud-access-admission-green-2026-10-01`. Private disk-backed temporary
directories and source hashes are recorded by each run. No real cloud operations
were performed by these tests. Installed release remains held.

The corrected fixture reactivates the idle alias through the foreground journal
API before constructing its successor. `admission-green2` passed the actual
FUSE scenario on 2026-10-01 at 07:57 UTC, including the unchanged ESTALE
assertion for a binding changed during admission. Both writable iCloud validation
wrappers now forward the hook through their existing owned-identity restrictions.
The full project check initially stopped at two Clippy style errors; these were
corrected before restarting the complete check. No live acceptance is inferred
from these synthetic checks.

Independent review then found two additional shared-path races: an existing
working file returned from a nontruncating open did not recheck its admission;
pathname truncation to a nonzero length mutated in a separate unchecked journal
call. The passing fixture did not cover these cases. Corrections and dedicated
race controls are required; installed acceptance remains held.

The existing-working and pathname-truncation fixes now passed a dedicated real
FUSE test (`working-path-green2`, 08:04 UTC). Each branch's removed recheck first
failed with an accepted stale operation (`existing-open-red`, `path-truncate-red`).
The first restored run was still marked failed: after the intentional rename,
Linux returned ENOENT instead of ESTALE, consistent with retrying the old pathname.
The corrected syscall assertion accepts either refusal, never success; unchanged
working length, old-descriptor truncation and ordinary pathname shrink/grow remain
required. No cloud calls were made.

The second full check stopped at an intermittent Busy in the existing local-export
lock test; an immediate unchanged rerun passed all eight tests. A concurrently
spawned CLI child inheriting a flock during fork/exec is a plausible, unproven
cause. The CLI test was moved unchanged to a separate integration binary, keeping
strict immediate lock assertions and strengthening wrong-account rejection to
the exact Account error. Production locking was not changed. Full check reruns
retain the failed evidence rather than treating the targeted rerun as completion.

Full `scripts/check.sh` passed on 2026-10-01, 08:04:45–08:13:13 UTC
(`.local-state/icloud-access-fullcheck-optin3-2026-10-01`). This includes formatting,
Clippy, workspace/feature tests, actual synthetic kernel mounts, script tests,
translations, ledger and docs. Rustdoc retained one pre-existing broken private
link warning in engine.rs; it was not a check failure. Native window scenarios
were checked separately earlier; neither result establishes installed acceptance.
