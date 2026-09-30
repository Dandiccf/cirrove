# Exact iCloud file Trash recovery — 2026-10-01

## Preregistered question and endpoint

Can a fresh worker reconcile a deliberately discarded delete acknowledgement
using the exact recoverable item identity, ordinary representation and complete
byte digest, without sending another mutation or enumerating all Trash items
inside the adapter?

The arm creates only a fresh owned validation folder and file. The first process
must leave its journal in VerifyRequired with the prepared identity. A second
process must reach Applied with the same operation and exact Removed receipt.
Its adapter rejects all mutation dispatch through a feature-only reconciliation
guard. An independent global Trash lookup remains in the probe as a control;
this is deliberately separate from the adapter's exact-ID proof.

Prediction: both processes exit successfully, and no replay is needed. This is
one bounded functional arm, not a latency comparison or reliability estimate.
Each process has a 900-second deadline; manifests record the executable hash,
command, private disk-backed temporary directories and process IDs before work.
No compilation or other measurement runs concurrently.

## Synthetic evidence

The new exact-ID regression failed before the change (one executed test,
exit 101) because the old implementation depended on a global inventory. After
the change all three file-Trash tests pass. The regression covers incorrect
identity, document ID, kind, parent, restore data, size, digest, package
representation, and metadata changing during verification. In write-probe builds
it also proves the recovery guard rejects mutation dispatch while permitting
reconciliation. Zero-length representation handling is synthetic in this arm.

## Scope

This tests orderly fresh-process recovery after an intentionally discarded
response. It is not a mounted unlink, abrupt power loss, in-flight transport
interruption, large-file or concurrent external-edit result. Validation files
remain recoverable in Trash. Ordinary iCloud write access remains disabled.

## Result

Passed: [recorded arm](icloud-file-trash-exact-recovery-414d1ea9-912a-4e30-a1d6-f55a68f477c1.json).
The discard process exited 0 after 61.321 seconds; the fresh recovery process
exited 0 after 96.835 seconds, including root/session and independent global
Trash controls. Both used SHA-256
`22b81b57b0094096dd91a238a7d3865d4c7fe814473b72bee0487dbf56202645`.
The second process reached Applied with the same operation's exact Removed
receipt and independently confirmed the recoverable item in Trash. Mutation
replay was disabled in this recovery process. These end-to-end durations are
not adapter-only latency and have no repeated-arm spread.

An independent read-only SQLite audit also confirmed one operation, Applied
state, matching account, unchanged request/prepared identity and exact Removed
receipt; its booleans are recorded in the arm JSON.

Full `scripts/check.sh` passed (exit 0), UTC 2026-09-30 22:23:25–22:29:48,
including workspace and feature tests, real-kernel FUSE tests, scripts, ledger
and documentation. Private log and manifest:
`.local-state/icloud-exact-trash-check-2026-10-01/`.
The preexisting rustdoc `retry_stuck` link warning remains. GUI window scenarios
are outside that command; this change does not alter the GUI. The installed
normal daemon was not replaced and ordinary writable iCloud remains disabled.
