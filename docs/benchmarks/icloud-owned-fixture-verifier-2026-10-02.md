# Registered read-only fixture observer checks

The new feature-only verifier binds a private manifest digest, isolated account, owned parent/item/revision, explicitly observed DATA/PACKAGE representation and exact source hash/proof. Source and target archive roots may differ; normalized semantic identity must match. It never signs in, mutates or replays work, and claims no GUI fidelity.

Before live use, run actual synthetic TLS success and post-body representation/revision faults, private path/manifest/account/root/content checks. Prediction: both unchanged representations succeed; removing the post-body representation or revision fence makes its fault arm incorrectly succeed and therefore fail its regression. Restore each fault before subsequent tests. Private disk temp and isolated target, no concurrent compilation/live runs.

Path reads reject symlinks per component; output-directory identity is rechecked but path-based output publication is not claimed resistant to malicious same-user ancestor renames.

## Negative controls and harness correction

Run `owned-fixture-rg-1f70f813d517` checked representation, revision, manifest
digest and semantic equality removals at their intended assertions. The fifth
control bypassed source-root selection and failed the intended
`proof(&f, &a, &ar, true).is_err()` assertion, but the harness rejected its line
number: replacing five source lines with one moved that assertion from443 to439.
This is a harness-location error, not a product-test pass. The actual test ran
once, failed at that exact assertion, and ignored zero tests. The retained arm
log and terminal exit101 document it; no compilation or unrelated error counted.
Both production files were independently hash-checked against their retained
pre-run hashes after the harness restored them. Restored positive runs follow
separately; this interrupted harness is not labelled completed-red-green.

Restored `owned-fixture-restored-provider` passed all four provider tests,
including TLS DATA/PACKAGE reads and both post-body fault cases.
`owned-fixture-restored-service` passed all three private-path, manifest/account
and exact-root/content tests. Both runs ignored zero tests and exited0. These
results prove the fixture observer's synthetic guards, not Apple application
compatibility or installed reliability.

Complete project validation: `native-formats-verifier-fullcheck-catalogue` ran
`scripts/check.sh` to exit0 with all checks passed. No installation or new cloud
mutation accompanied these verifier tests.
