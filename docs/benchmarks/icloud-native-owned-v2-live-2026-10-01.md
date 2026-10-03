# Proposed isolated acceptance: retain old Stage, then one fresh v2 replacement

PLAN ONLY. No command below has been executed. No cloud operation, daemon start,
session refresh, schema migration or build is authorized by execution of this file.
The parent must first finish review/fullcheck and register the actual run under
`docs/benchmarks/` before executing a newly created harness.

## Fixed authority and immutable inputs

- Existing isolated state: `/var/tmp/cirrove-public-native-ec7f82e1/state`.
- Label: `iCloudPublicNativeValidation`.
- Account: `ca43ba36-9f5c-4784-9ae9-20e5c3cbe319`.
- Original import operation: `b534c269-8b9e-47e0-97a0-a512d0127765`.
- Old v1 Conflict: `82767008-4553-4f8b-99cc-8a521ad9c5ac`.
- Original: `FILE::com.apple.CloudDocs::8A909C8D-32D7-4E39-9202-DF1862E46C5A`.
- Retained stage: `FILE::com.apple.CloudDocs::086B85BF-007F-427E-88A6-78333B18DC8A`.
- Old evidence directory: `/var/tmp/cirrove-native-replacement-live-42a313de-1e94-488f-aed8-e7c704944fed`.
- Edited owned archive exact raw SHA256:
  `83527e22cb8c56dd07a4d2a82acbc711711ff24d912df46f218eaab4ef62e776`.
- Explicit archive root: `Replacement.pages`.

Read-only source inspection was limited to the existing runner and helper code.
No credential/config dump, raw encrypted/plaintext checkpoint, provider body or
private document content was read for this plan.

## Existing runner contract and why it cannot be reused

`.local-state/run-public-native-replacement-attempt2.py` fixes run42a313de, creates
an exclusive attempt2 manifest, requires only the original import row before its
one submission, and expects old preflight metadata. It starts only its isolated
daemon with explicit state/socket, hashes binaries/source, uses disk-backed temp,
checks exact original selection and owned mount, submits once, polls public jobs,
retains list results and stops only recorded child PIDs. Those safety properties
are reusable in a new harness; its run/manifests/preflight/row expectations are not.

`replacement_live.rs::bound_run/source_binding` accept only run42a313de and
`preflight.json` must not exist. `retained_public_import_for_replacement` admits
exactly1/2 journal rows. Therefore neither current preflight nor postflight helper
can honestly validate the new three-row history. Do not remove these checks or
replace old files to make them pass.

Required small helper before execution: an exact preregistered new-run read-only
pre/postflight binding, tied to the above old run/account/import/Conflict/stage.
It must accept precisely the original import + exact typed-abandoned old row
(+ one exact new operation for postflight), reject all other rows, and verify old
row identity/representation v1/raw payload/checkpoint hashes remain unchanged.
It must compute fresh source/original semantic v2 proof without rewriting old v1
proofs. Reuse public import identity binding and canonical Folder content_version
None, but require a freshly observed nonempty exact ETag. Do not infer unchanged
revision from name, size, cached paths or the historical abandonment receipt.

## New harness registration

Generate a new UUID once during harness preparation; record it before creating
private directories. Use `/var/tmp/cirrove-native-abandon-v2-live-<NEW_UUID>` for
exclusive manifest/output creation, and a short private disk-backed temp path
(the previous long TMPDIR hit Unix socket SUN_LEN limits). Assert findmnt FSTYPE
is neither tmpfs nor ramfs. Record binary SHA256, clean source commit, exact argv,
filesystem, bounded duration (suggest total45min), supervisor PID and every child
PID. No concurrent compilation, measurements or other isolated cloud arms.

Read existing benchmark JSON/recorded PIDs to confirm no open measurement watches
this isolated daemon. Require socket absent and no mount at the exact isolated
mountpoint; do not restart installed cirroved or alter default file manager.
No cleanup recursion, mount traversal, token output or automatic session renewal.

## Finite sequence and predictions

1. **Read-only inventory before schema18 open.** Check exact account journal in RO
   mode, exact import+old Conflict operations only, old v1 representation, no typed
   completion, no attempt token, current Stage/RegistrationArmed continuation.
   Retain hashes/counts/sanitized phase only. Export the old saved archive via the
   existing offline `export-save --label ... --operation <OLD> --offline --state
   <ISOLATED_STATE> --destination <NEW_OUT>/old-saved-before.pages`; destination
   must not exist. Verify returned digest/size equals old journal row; no contents
   printed. Do not copy token files to output or dump settings/checkpoint JSON.
   Preserve offline SQLite safety evidence before any schema migration.

2. **Start isolated current daemon once**, explicit `--state-dir` above and socket
   `/var/tmp/cirrove-public-native-ec7f82e1/control.sock`. Schema18 migration only
   after code/fullcheck/schema compatibility approval. Query capabilities for
   abandonment/receipt lookup/export/list/replace. Expect exact account mounted.
   Warm only owned parent/package paths, with bounded retries for metadata reads,
   never retries of mutation requests. Record safe status counts/selected IDs.

3. **One public local-abandon request**, prerecord `abandon_request_attempted=true`
   before writing request bytes. Exact argv:
   `cirrove abandon-native-stage --label iCloudPublicNativeValidation --account-id
   ca43ba36-9f5c-4784-9ae9-20e5c3cbe319 --operation
   82767008-4553-4f8b-99cc-8a521ad9c5ac --socket <ISOLATED_SOCKET>`.
   Prediction: bounded fresh metadata inspection confirms original exact revision
   + known stage active; local transaction records Resolved with no handoff,
   allocation, upload, rename, Trash, restore or cleanup. Any refusal/timeout ends
   this arm; query the durable receipt to determine whether local commit occurred,
   but never automatically repeat abandonment or submit fresh replacement.

4. **Independent retained-state verification before any new upload.** Query
   `native-stage-abandonment` with exact account/op. Expect original ID, retained
   stage ID, historical time and no secret fields. Export old archive to a second
   exclusive destination using public `export-save`; exact digest/size/bytes must
   match first export. Gracefully stop only isolated daemon, then RO journal and
   sealed-file hashes prove old bytes/checkpoint/v1 representation unchanged;
   typed abandonment row and queue terminal state are the intended local delta.
   Read-only provider verification (new helper) confirms original still exact
   captured revision and old stage still present. If metadata changed, stop; no
   fresh operation. Never delete old stage or claim it is a Trash backup.

5. **Fresh v2 preflight under NEW_UUID**, daemon stopped. Bind new helper to exact
   immutable edited owned archive, old typed abandonment, owned original/parent.
   Capture canonical original Node and newly observed ETag; produce fresh v2
   source+original semantics and assert they differ. All old evidence immutable.
   This is a separate attempt, not semantic reinterpretation/retry of OLD.

6. **One new public replacement**, restart only isolated daemon, exact warm
   selection, then prerecord `replacement_request_attempted=true` before sending:
   `cirrove replace-native-package --label iCloudPublicNativeValidation --account-id
   ca43ba36-9f5c-4784-9ae9-20e5c3cbe319 --path <FRESH_BOUND_VISIBLE_PATH> --item-id
   FILE::com.apple.CloudDocs::8A909C8D-32D7-4E39-9202-DF1862E46C5A --etag
   <FRESH_BOUND_ETAG> --archive <NEW_PRIVATE_BOUND_ARCHIVE> --source-root
   Replacement.pages --socket <ISOLATED_SOCKET>`.
   Capture new durable operation UUID; require it differs from both old operations.
   If reply lost, bounded list-native-replacements discovery only; no resubmission.
   Prediction: v2 accepts directory-entry normalization with exact files unchanged,
   then new staged identity, single guarded Trash+rename handoff and typed receipt.
   Any unresolved state ends mutation work; inspect only.

7. **Convergence and recovery, read-only:** successful public job, one visible
   document at original path, new current ID, old original's recoverable Trash
   receipt, exact source payload/semantic v2 on independent download, original
   semantic recovery in Trash, normal mount open, fresh-process/offline/remount
   evidence as existing helper contracts permit. Old abandoned stage remains
   separately recorded and untouched. Require three exact upload rows; old row
   stays Resolved v1 with unchanged payload/checkpoint and no typed handoff receipt.
   Apple Pages GUI open/editability remains a separate explicit observation;
   same-ID/history/share-link continuity is not claimed by a two-ID replacement.

Finally SIGTERM only recorded isolated daemon PID, wait up to60s, record exit or
shutdown_pending. Never pattern-kill, recursive-delete, unmount unrelated paths,
or retry a failed mutation. Retain every artifact for review.

## Interpretation

The local-abandonment arm tests durable recovery without provider mutation. The
fresh replacement arm tests one owned edited archive and v2 normalization. Neither
alone establishes ordinary editor save/atomic-replace support, Numbers/Keynote,
all failure paths, broad provider reliability or same-ID editing. Success is a
specific controlled result; any schema, identity, lifecycle or receipt failure is
a stop condition, not grounds to weaken a guard.

Registered executable arm: icloud-native-owned-v2-live-2026-10-01.json. Source commit and exact binaries are pinned there; this registration supersedes the plan-only status above for that one owned arm.

## Observed result and correction

Initial arm stopped before daemon start: offline export requires a disabled account.
Continuation1 verified unchanged pre-migration inventory, checkpoint and payload
hashes against the retained baseline, then used public export via the isolated
daemon. Abandonment and identical before/after archive exports succeeded. One
fresh replacement completed, with typed receipt, original Trash locator and one
visible mounted document. Both isolated daemon processes exited0. No replay.

Independent postflight remains incomplete. All preflight row bindings matched
except original_semantic: the public capture used version1, while fresh preflight
computed version2. native_trash::capture_active_semantic still uses the legacy
default; new archive semantics correctly use2. This is not proof of changed bytes.
A read-only bridge must independently establish both versioned proofs from the
same retained original archive before verifying current and Trash content. Do not
rewrite the operation, prior proof, baseline or preflight, and do not repeat the
completed replacement. The full release gates remain open.

Fresh-capture regression registered after diagnosis: synthetic actual-HTTP capture
must return version2, while explicit version1 verification remains version1 and
performs no Trash call. Run before policy correction to establish failure, then
change fresh capture choices only; replay continues using saved.semantic.version.

## Independent readback completed

The registered read-only arm `icloud-native-owned-v2-readback-2026-10-01.json`
completed with exit 0. Both semantic versions were recomputed from the retained
original archive without changing historical proofs. Independent current download
matched the edited v2 proof; recoverable Trash content matched the original v2
proof. Exact current revision stayed stable, the abandoned Stage remained present,
and the old v1 record and payload remained preserved. No mutation was replayed.
The bridge regression passed, including changed-archive and wrong-receipt refusals.
Apple Pages GUI opening and the wider application/release gates remain open.
