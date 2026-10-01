# Public native Stage recovery

Question: can an exact account+operation request abandon only an observed retained
Stage conflict, preserve historical stage identity/local recovery, and recover its
durable receipt after downgrade to RO without cloud calls or database changes?

Registered arms: run service native_abandon tests (public socket success, paused
inspection lifecycle/stale journal refusal, late-stop job outcome) and CLI exact
request parsing. Evidence inspection is synthetic; no real vault/cloud calls.
Prediction: exact current control and journal snapshot are rechecked at commit;
late stop cannot hide committed result; receipt lookup works through RecoveryControl
on a freshly reopened RO account. Client-supplied checkpoint/path fields refuse.

Negative controls: remove final same_mount check and require paused mount arm to
fail; suppress receipt after late stop and require job regression to fail. Restore
all controls before full repository check. No real abandoned upload has been
released or replayed by these tests.

Integration resolved only a module-declaration context overlap with native_edit;
the rest of the reviewed surface patch applied unchanged.

Initial build found a test fixture cloning Box<Node> where the evidence expects
Node; cloned the inner Node without changing production types. All three public
route/job/lifecycle tests passed, including exact RO database-byte preservation.
Now run the registered final mount-control negative.

The initial mount-comparison negative control passed because its fixture removed
the writer entirely; `is_none_or` still rejected absence. That control did not
exercise mount identity. The corrected fixture also substitutes a distinct
WriteControl belonging to the exact same Engine and journal. Prediction:
disabling only `same_mount` must now incorrectly succeed for `mount-replaced`.

Corrected `native-abandon-mount-swap-negative` failed on `mount-replaced`:
the disabled identity check incorrectly allowed Succeeded.
`native-abandon-late-stop-negative` failed with Failed versus Succeeded after
a late Stop when deliberately suppressing an already committed outcome.
Both production guards were restored before the combined positive run.

Restored run `native-abandon-restored`: all three public recovery tests passed,
including the added same-Engine replacement mount arm. `native-abandon-cli`:
one exact-identity/unknown-payload CLI test executed and passed. These remain
synthetic evidence; no Apple Stage was abandoned by these tests.

Full `scripts/check.sh` passed in arm `native-path-recovery-fullcheck`: format,
clippy, workspace and iCloud feature tests, actual-kernel mounts, script tests,
Strata/Dolphin integration checks and acceptance ledger. Existing rustdoc link
warning for `Writeback::retry_stuck` remains. Window scenarios are not included.
No installation or real-account mutation occurred.
