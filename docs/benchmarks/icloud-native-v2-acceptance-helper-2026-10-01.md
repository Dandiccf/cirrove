# Exact owned replacement acceptance helper

Question: can an independent verifier accept exactly the original import, the
explicitly abandoned old v1 operation and one fresh v2 operation while preserving
old proofs, payload and encrypted checkpoint unchanged?

Run three focused v2_ tests under icloud-write-probe. Before acceptance, remove
the operation inventory fence, the new-run identity fence and existing-artifact
refusal separately. Each named fixture must reject its deliberate weakening.
Restore each control before the next. Full scripts/check.sh before commit.

This helper does not submit provider mutations. Synthetic tests prove local
binding and replay refusal only. The separately preregistered owned live arm must
prove fresh provider semantics, Trash recovery and retained old-stage identity.
Independent review found no concrete Rust authority/replay blocker. Runner fixes
make attempted flags durable via fsync and refuse optimized Python before work;
parent confirmed optimized refusal and default preparation-only invocation.
No live arm has run. No source, checkpoint or private document contents are logged.

Initial focused build found a missing explicit anyhow::bail macro import in the
new module. Added it; no provider behavior or guards changed.

All three focused tests passed after the import correction. Review found that
the old-run test called filesystem validation too: a missing evidence directory
could mask a disabled identity check. Isolated pure run-to-directory binding
from the existing owned-directory check and assert both rejected old run and
accepted new run without filesystem dependence. Now disabling that exact guard
must fail the old-run assertion before any account access.

`native-v2-run-negative` failed at the intended old-run rejection assertion.
Production guard restored. No negative binary used a real account.

Inventory bypass failed at rejection of unexpected rows; artifact-guard bypass
failed at refusal of existing preflight evidence. Both exact production sources
restored. Restored positive tests and full validation follow.

Full scripts/check.sh passed in native-atomic-retirement-fullcheck-corrected: formatting, clippy, workspace/features, kernel mounts, scripts, Strata/Dolphin and ledger/docs. First run stopped on collapsible_if; condition simplified without semantic change. Existing rustdoc retry_stuck link warning remains; display window scenarios not included. No installation or cloud mutation.
