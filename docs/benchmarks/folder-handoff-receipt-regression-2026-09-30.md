# Folder handoff must not reuse a cached creation receipt

Investigation of CI run 36667518761, where
`real_a_directory_created_and_removed_again_is_gone_from_the_provider` failed
because a mutation entered Failed, Conflict or NeedsReview. Run 36669600314 was
green; neither log identified the precise provider precondition. This regression
reproduces a matching stale-ETag path deterministically, rather than attributing
all intermittent failures to scheduling or extending the timeout.

## Reproduction and correction

The provider fixture keeps a folder with ETag `settled-folder`; the creation
receipt and metadata cache both contain `create-receipt`. Maintenance formerly
accepted cache equality as proof that the receipt was current, skipped its node
request and cleared the local operation dependency. A subsequent removal could
therefore use the obsolete creation ETag.

`folder_handoff_refreshes_a_cached_creation_receipt_before_removal` failed before
the fix: the expected independent observation count was 1, actual 0. The
correction bypasses receipt-cache reuse for folders. Network work remains outside
the journal mutex and activity gate, and existing revision checks protect the
handoff commit. Ordinary files retain their existing cache fast path. This does
not make a provider deletion atomic against later concurrent changes.

After correction all six handoff tests passed. The new regression also asserts
that the committed folder follows the fresh remote node and that the eventual
RemoveFolder request carries its settled ETag. Existing tests cover cancellation,
backoff, concurrent local access and failed durable publication.

These are synthetic functional results, not cloud reliability or latency
measurements. No installed daemon, account access mode or cloud fixture changed.
The earlier full-check attempt used an excessively long btrfs temporary path and
failed desktop Unix-socket tests with `path must be shorter than SUN_LEN`; the
rerun uses a short private `/var/tmp/cirrove-check-*` directory on btrfs.

## Full validation

`scripts/check.sh` passed in full on 2026-09-30 (17:03:45–17:10:23 UTC).
The formerly intermittent directory-create/remove kernel test passed, as did the
other writable-session, read-only, Google, capacity and read-window mount tests.
Private manifest and complete log:
`.local-state/icloud-resume-check-short-2026-09-30/`.
This is one full-suite pass plus a deterministic failing-before regression;
it does not establish the absence of every possible concurrent-removal race.
