# Namespace SIGKILL fixture readiness race

The Linux job for commit a182197 failed in
[CI run 36654121874](https://github.com/Dandiccf/cirrove/actions/runs/36654121874/job/109694589702),
`actual_sigkill_preserves_pending_outcome_and_acknowledged_receipt`, at the UUID
parse after killing the child. The reported error was `ParseLength { len: 0 }`.
All package jobs and the separate mounted-read job passed for that revision.

The child publishes its UUID with `std::fs::write("ready", ...)`. The parent only
waited for path existence, then killed the child and read the UUID. File creation
can become visible before the write, so this synchronization can kill the child
between those steps and leave an empty marker. This failure is not evidence that
the journal discarded its acknowledged record: the parent failed before opening
and inspecting that journal.

A focused regression constructs absent, empty, incomplete and complete markers.
Only a parseable full UUID may release the parent to send SIGKILL. Test the former
exists-then-parse behavior first, then make the parent wait for the complete UUID
and retain it before terminating the child. Keep the same actual process kill and
journal/receipt assertions for applying, applied and resolved phases. The existing
five-second deadline still kills and reaps an unready child.

No cloud mutation, installed service change or new durability guarantee is involved.
The original CI result remains a failed run; local validation cannot retroactively
turn it green.

The negative regression reproduced the empty-marker `ParseLength { len: 0 }`
panic with the former existence-only synchronization (exit 101). With complete
UUID readiness, the mutation test target passed: 15 passed, one deliberately
ignored child-process entry point. This includes the actual SIGKILL test across
all three journal phases. The final `CARGO_TARGET_DIR=.target-icloud-feasibility
scripts/check.sh` completed with exit 0, including workspace, feature, real-kernel
mount, script and ledger checks. Graphical window scenarios were not run. The
existing rustdoc link warning remains; no new CI outcome is claimed here.
