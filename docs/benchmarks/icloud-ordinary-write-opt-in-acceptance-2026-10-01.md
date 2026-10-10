# Registered next milestone: installed ordinary iCloud write opt-in

Status: implementation and acceptance in progress; installed acceptance not started. This milestone is an intermediate step
toward full iCloud support. Native Pages/Numbers/Keynote editing and complete
provider/release reliability remain separate open requirements. Existing accounts
must remain read-only unless the user explicitly changes their access mode.

## Question and prediction

Can the current context-aware iCloud write router be exposed through the ordinary
connection workflow while preserving selected access, retained edits, recovery and
unsupported-document boundaries? Prediction: current settings and UI intentionally
block the route; simply removing those blocks would mishandle reauthentication,
permanent-delete visibility and active working-byte recovery. Complete those
contracts first, then run the finite acceptance matrix below.

## Required behavior

- Preserve existing serialized access; do not migrate old read-only accounts.
- New iCloud connections default read-only, including when the user switches from
  a provider whose Allow changes control was enabled. Password and 2FA stages carry
  only the explicit selected mode.
- Ordinary reauthentication preserves access. An explicit upgrade or downgrade
  takes effect only after same-account session validation and successful durable
  persistence. Cancellation/failure preserves prior settings and desired state.
- Downgrade prevents new cloud changes without discarding pending local data.
- Use the context-aware router and existing journal/checkpoint ownership. Keep
  the context-free factory refusal; it lacks required recovery context.
- Expose working-byte recovery to active users without requiring account removal,
  sealing/uploading their bytes, or copying while holding the journal mutex.
- Protected native packages/app containers remain readable. Unsupported mutations
  must fail before local namespace acceptance; ordinary siblings remain writable.
- Keep permanent deletion unavailable for iCloud. Ordinary writable support is
  not evidence of a permanent-delete adapter.
- Explain that read-only is enforced locally by Cirrove, not a narrowed Apple
  OAuth permission. Never label native packages editable or all iCloud support done.

## Finite acceptance matrix

1. Synthetic settings/access transitions: old settings, explicit opt-in, ordinary
   reauth, wrong account, cancelled/failed session save, explicit downgrade with
   pending bytes. Verify mode, desired state and retained journal data.
2. Controlled failure arms: local ENOSPC, provider quota, transfer deadline and
   session rejection. Use private bounded storage and synthetic provider faults;
   never fill the user's cloud or ordinary system disk. Each arm requires retained
   exportable bytes, understandable status, no unsafe replay and explicit recovery.
3. Two independent fresh Cirrove-owned-folder application runs: real editor save
   and atomic-save workflow plus file-manager create/rename/move/recoverable Trash.
   Require independent content/identity checks and remount. Register each exact
   command, binary, fixture, endpoint and prediction before running; quote timing
   spread rather than infer broad reliability from two arms.
4. Native refusal: package/app-container browsing and export, rejected nested
   writes/replacements/rename/deletion, ordinary sibling positive controls.
5. Installed GUI acceptance: audit package/developer exclusivity and open
   measurements first; follow install-developer procedure only at that stage.
   Verify default read-only, explicit opt-in, export/recovery, reauth, downgrade,
   file-manager behavior and no unsupported permanent-delete action. Do not
   interrupt the current ordinary daemon merely to prepare this plan.

## Claims and remaining scope

Synthetic fault coverage establishes deterministic recovery behavior, not real
Apple reliability. Existing bounded live arms remain supporting evidence; they
cannot replace the installed/user-facing checks. Unknown native FILE bundles and
native editing remain open beyond this milestone. More complex namespace races,
unbounded file sizes, long-session behavior and abandoned staging cleanup remain
explicit limitations only where failures retain data and provide usable recovery.
The current 1 GiB live result is a tested size, not an arbitrary-size guarantee.

No gate closes by this plan's existence. Record each actual result and failed arm
here or in linked registered artifacts before announcing this milestone achieved.

## Progress recorded 2026-10-01

- Access persistence/cancellation and native window scenarios have passed isolated
  checks: [explicit access evidence](icloud-explicit-access-workflow-2026-10-01.md).
- Two fresh owned-folder runs passed actual Neovim/Gio operations and independent
  provider readback: [application acceptance](icloud-real-applications-acceptance-2026-10-01.md).
  Those first two arms preceded the selected-file admission hook. The subsequently
  registered combined arm C also passed (410.1 seconds), with all seven Neovim/Gio
  operations, independent hashes/identities, Trash and remount checks. This is one
  combined run, not repeatability or installed acceptance.
- The selected-file hook refuses unknown-extension package representations before
  local acceptance in a synthetic FUSE scenario. Review found further existing-
  working-file and pathname-truncation races; both now have targeted passing
  kernel tests and meaningful removed-guard failures. The recorded full project
  check passed at 08:13:13 UTC and combined live arm C passed at 08:43:57 UTC.
  [Admission evidence](icloud-selected-write-admission-2026-10-01.md) records the
  failed attempts and successful checks; the application artifact records arm C.
  These results do not validate later source changes or the installed workflow.
- The source now exposes the explicit ordinary-file opt-in. This does not close
  the above safety checks, native editing, or the installed GUI gate. The regular
  installed daemon remains unchanged.


## Installed rollout must preserve the existing Strata integration

The user's accepted Strata integration is maintained in the separate
`feat/strata-integration` worktree. Its helper requires the daemon's
`paths-cached: 1` capability; without it, menus and badges intentionally disappear.
The current iCloud feasibility service does not contain that capability or the
Strata installer. A successful iCloud build alone is therefore not safe evidence
for replacing the installed daemon.

Before installed acceptance, reconcile the already-working Strata service/helper
changes with this branch, preserve the companion provider-enabled Strata binary,
provider installation and default file-manager association, and check conditional
pin menus, direct/inherited pins, kept/fetching badges and event-driven updates
without visible clearing. Preserve all ordinary installed account settings and
pending edits, check for measurements, and obey package/developer exclusivity.
This is a compatibility gate, not a request to reinstall or change defaults now.
