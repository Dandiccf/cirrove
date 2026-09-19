# 0009: What a second provider costs, and what to change before writing one

Status: accepted; read-only Google preview implemented and initially live-checked, with the concrete
boundaries and deviations from the original sequence recorded below.

## Implementation update, 2026-09-19

Google Drive is the second provider, initially read-only. The original survey and
proposed sequence below remain as their historical rationale. The concrete first
implementation uses a tagged `AppRegistration` inside the account rather than a
second, independently mutable account-provider field: `registration.provider` is
`microsoft` or `google`, and maps to the existing runtime scope names `onedrive`
and `googledrive`. Settings are version 2 with a side-effect-free version-1 reader.

`TokenSource` and the collection descriptor now live in core; the old OneDrive
names are re-exports for callers. Auth does not depend on OneDrive. The existing
PKCE/callback/vault/refresh implementation is shared, with Google scopes, signed
identity verification and user-info binding isolated in `auth::google`. Microsoft
logic stays in place while both paths are validated; this is an incremental
extraction, not a new generic OAuth framework.

Account construction returns `Arc<dyn ReadProvider>`. Microsoft write factories
and validation verbs explicitly reject Google accounts. Status gains a provider
field and preserves the old field names and protocol number, so this increment
does not require an otherwise unrelated IPC rename. Google has no Graph counters.
The planned compare-and-set capability work is deferred until there is an actual
Google write implementation; the read-only adapter does not weaken any existing
write precondition.

A later create-side investigation found one contract change that has a concrete
Google use before a writer exists. Drive can pre-generate a file ID for safe
retries, but the worker previously had no way to persist that identity before the
request that starts a resumable session. `UploadStep::Prepared` now creates that
durability boundary in the credential vault. Recovery inspection reuses it, and
reconciliation receives the last saved checkpoint so it can address the exact
provider identity instead of a non-unique Google sibling name.

The adapter now has a create-only transport using that boundary. It generates an
ID, starts or replaces a resumable session without changing the ID, follows the
server's exact committed offset and verifies uncertain completion by streaming
the exact object's SHA-256. Only the configured upload origin and path can enter
a session checkpoint. Synthetic tests cover the ordering, lost sessions, partial
offsets, receipts and content verification. Account construction still rejects
Google write access, and no write scope, writable mount or live Google mutation
is part of this decision. Duplicate-name collision semantics and conditional
replacement remain unresolved.

The second adapter found real differences at the boundary: initial listing and
change tracking are separate Google endpoints; sibling names are not unique;
Google-native documents require an explicit export or link representation. See
[the implemented policies and validation](../google-drive.md).

The optional read-session contract now has its second implementation too. Google
does not provide Graph's expiring download URL plus strong ETag combination, so
its session keeps the stable provider identity and streams a bounded 8 MiB window
between metadata/version checks. The shared cache publishes the staging file only
after the final version still agrees. This reuses the contract without pretending
the two providers have the same transport primitive.

## What prompted it

The project's own statement of intent names three providers — "beginnend mit
OneDrive, GoogleDrive und iCloud Drive, more to come" — and the roadmap has
promised a Google Drive adapter since before there was a mount. Nothing had ever
checked whether the code could take one. ADR 0001 deliberately refused to build
a generic interface ahead of a real second use case, which was right; the
question this answers is what that decision actually cost, now that the second
use case is approaching.

So: a survey of the whole workspace for how much of it a second provider could
reuse, and how much assumes Microsoft.

## What is actually true today

**The core is genuinely neutral, and better than expected.** Every provider
trait — `MetadataProvider`, `ReadProvider`, `ReadSession`, `UploadProvider`,
`MutationProvider` — is object-safe over concrete core types, with no associated
types anywhere. That single property is what makes a second provider a matter of
writing an adapter rather than reshaping the engine. Three methods
(`watch_changes`, `open_read_session`, `read_path_counters`) already default to
unsupported, so a minimal adapter can ship without notifications or session
reads. The engine derives the scope from `provider_id()` and enforces it on
every store access; `cirrove-store` and `cirrove-allocator` contain not one
Microsoft word.

**The store's key space is already provider-qualified.** Every table keys on a
serialized `Scope`, and `Scope` has carried a `provider` field from the start.
Two providers can share a state directory without a schema change.

**The edges are entirely Microsoft.** `cirrove-auth` hard-codes Graph scopes, the
`login.microsoftonline.com` endpoint shape, the Microsoft JWKS URL, a
tenant-match assertion, and a client-id validator that a Google client id fails
by construction. It also depends on `cirrove-onedrive`, because the
`TokenSource` trait lives there — an inverted layering that would force every
future provider to link the OneDrive crate.

**The account record cannot say what it is.** `accounts.json` has no provider
field at all. It embeds `DriveInfo`, a `cirrove-onedrive` type, in the on-disk
format, alongside a Microsoft app registration and a tenant id. Its version is
refused rather than migrated when unknown, so adding a discriminant is a
deliberate bump with a read-old path to write.

**One contract-level assumption is real.** `UploadIntent::Replace` requires a
non-empty ETag precondition and `MutationRequest::validate` enforces it. That
exists because Graph offers `If-Match` and using it is what makes a replace
safe. Drive v3 has no equivalent on `files.update`; an adapter would have to
either fake a precondition it cannot honour, or take a weaker path. This is the
only place where the write contract encodes a Microsoft capability as a
requirement rather than as a capability.

## Decision

**The order is: finish OneDrive, then take the edges apart, then write the
adapter.** Not because the refactors are hard, but because every one of them
touches the account file, the IPC wire format or the auth flow — the three
things a 1.0 must not be caught reshaping. ADR 0001's reasoning still applies in
its own terms: the interfaces should appear alongside the use case, and the use
case is the adapter, not the anticipation of it.

**Before any adapter is written, in this order:**

1. `Account` gains a `provider` field and `Settings.version` goes to 2, with a
   read-old path that fills in `onedrive`. Nothing below can be done first;
   there is nothing to dispatch on.
2. `TokenSource` moves from `cirrove-onedrive` to `cirrove-core`, and
   `cirrove-auth` stops depending on the OneDrive crate.
3. `cirrove-auth` splits into a generic OAuth/OIDC core and a `microsoft`
   module. The PKCE, nonce, loopback-callback and Secret Service storage are
   already provider-neutral in everything but where they are written; the
   endpoints, scopes and claims are not.
4. `Account.drive` becomes a provider-neutral collection descriptor instead of
   a `cirrove-onedrive` struct.
5. `accounts::provider()` returns `Arc<dyn ReadProvider>` and dispatches on the
   new field. The daemon reaches exactly one construction site, and injection
   points for a second already exist because the tests use them.
6. The status wire format loses its Graph vocabulary — `drive_id`, `tenant`,
   `drive`, and `ReadPathCounters::graph_gets` in the crate that calls itself
   provider-neutral — in one deliberate protocol bump, with the desktop.

**The ETag question is decided as a capability, not a requirement.** A provider
states whether it offers a compare-and-set on content. Where it does, a replace
carries the precondition and nothing changes. Where it does not, the replace is
allowed without one and the reconciliation path — which already exists, because
an upload that is interrupted has always had to be reconciled against what the
provider actually holds — carries the weight. What is not acceptable is an
adapter inventing a token to satisfy a signature: a precondition that cannot
fail is worse than none, because the code above it believes it is safe.

**User-facing Microsoft wording stays until there is a second provider to
confuse it with.** "Sign in with Microsoft" is correct while Microsoft is the
only choice, and generalising it early produces a vaguer product for nobody's
benefit.

## What was changed now rather than deferred

Two findings were not about a second provider at all.

The diagnostics bundle leaked the drive id. Free-text redaction looked for the
shape of an item id — a long run of upper case and digits — and a drive id is a
longer base64-ish run, mixed case, with dashes and underscores. The engine logs
`collection=b!...` whenever a collection stops updating, so the id of the drive
a person signed into reached a file whose entire purpose is being pasted
somewhere. Fixed, with a test naming the vocabulary a bundle is read for so the
new shape cannot start eating it.

The provider id in a `Scope` was the literal `"onedrive"` written out at each
site, while the engine derived it from `provider_id()`. Agreement between the
two was a convention. A divergence would not have failed anywhere: it would have
written pins under a key the engine never reads, so pinning would have quietly
done nothing. There is now one constant.

Both of these were wrong before anyone thought about Google Drive. Looking for
one thing found another, which is the usual way.

## What this does not decide

Which provider is second. Drive is named in the roadmap and iCloud Drive in the
goal, and the work above is the same either way. Drive's change feed needs
webhooks that expire, so `watch_changes` will likely report unsupported at
first and the periodic feed will carry it — ADR 0003 anticipated that. iCloud
Drive has no documented general-purpose API at all, which is a question about
whether it can be done rather than how.
