# 0016: iCloud Drive needs a supported transport before it gets an adapter

Status: proposed; feasibility boundary recorded on 2026-09-24.

## The user-visible goal

The desired product is the same one Cirrove offers for its other providers: a
person connects an existing account and sees that account's existing files in a
Linux filesystem. It is not enough to create a new Cirrove-only store that happens
to consume the person's iCloud quota.

That distinction rules out an easy-looking implementation. Apple describes
[CloudKit](https://developer.apple.com/documentation/cloudkit) as storage for an
app's data in that app's iCloud containers. CloudKit JS likewise requires an
existing CloudKit app, a configured container and an API token. Its databases are
the app's databases. They do not expose the person's general iCloud Drive tree.

Apple's [File Provider](https://developer.apple.com/documentation/fileprovider)
framework points in the other direction: it lets an app publish the app's remote
storage into Files or Finder. It does not give a Linux client a transport for
reading Apple's iCloud Drive provider.

Apple documents manual iCloud Drive upload and download through iCloud.com, but
does not document that web application's private requests as a developer API.
Apple's documented third-party authorization and app-specific-password flows
cover Mail, Calendar and Contacts. No corresponding iCloud Drive authorization
or file API is documented there.

The resulting conclusion is deliberately narrower than saying that access is
technically impossible: **as of this review, Apple publishes no general-purpose
API with which Cirrove can enumerate and mount a person's existing iCloud Drive
directly from Linux.** This is an inference from the boundaries of the published
interfaces above, and must be rechecked before implementation because Apple can
add an interface later.

## Routes considered

| Route | Existing iCloud Drive files | Supported interface | Fits Cirrove now |
| --- | --- | --- | --- |
| CloudKit or CloudKit JS | No; only a Cirrove-owned app container | Yes | No |
| File Provider extension | No; publishes Cirrove storage to Apple clients | Yes, on Apple platforms | No |
| iCloud.com private requests or browser automation | Potentially | No documented API contract | Rejected |
| App-specific password | No documented Drive service | Yes for named iCloud services such as Mail, Calendar and Contacts | No |
| User-selected folder on a signed-in Mac | Yes, within the selected folder | Yes, through macOS file access | Candidate bridge |
| Manual export or copy | A point-in-time copy | Yes | Import feature, not a provider |

Calling a CloudKit-backed Cirrove container “iCloud Drive support” would give the
wrong answer to the product question. Depending on private iCloud.com endpoints
would also make authentication, two-factor prompts, protocol changes and account
lockouts part of an interface Apple has not offered to developers. Cirrove will do
neither.

The EU interoperability-request process is worth monitoring, but its existence is
not an API and does not remove this gate. Apple's currently documented Account
Data Transfer API concerns App Store usage data, not a mounted iCloud Drive tree.

## The candidate worth testing

A small macOS companion can use the Apple-supported local iCloud Drive projection
on a Mac that is already signed in. The person grants it one folder with
`NSOpenPanel`; a security-scoped bookmark can preserve that choice across companion
restarts. Cirrove pairs with that companion and consumes a narrow file service.
Cirrove never receives an Apple Account password, browser cookie or two-factor
code.

```mermaid
flowchart LR
    Apps[Linux applications] --> FUSE[Cirrove FUSE]
    FUSE --> Adapter[iCloud bridge ReadProvider]
    Adapter <-->|paired encrypted connection| Companion[macOS companion]
    Companion -->|security-scoped selected folder| Local[iCloud Drive projection]
    Local <--> Apple[Apple sync service]
```

This is a compatibility bridge, not a direct provider API. It requires a Mac that
is online often enough to serve the selected folder. Files that macOS has evicted
must be materialized before the companion can stream them; Apple exposes download
status and a supported request to start that materialization. A Cirrove range read
may therefore require macOS to download the whole remote file first.

The companion should use Apple's coordinated file APIs. A raw walk of
`~/Library/Mobile Documents` is not the proposed contract: that path is an
implementation detail, it bypasses explicit folder consent, and it does not by
itself solve concurrent access or placeholder hydration.

## Mapping to Cirrove's read contract

The first spike must prove the following mapping before a provider crate, account
type or settings UI is added.

| Cirrove requirement | Candidate macOS evidence | Gate |
| --- | --- | --- |
| Account and collection | Companion installation identity plus the selected root's persistent identity | Must remain distinct for two companions and two selected roots |
| Item identity independent of path | Volume identity plus `URLResourceKey.documentIdentifierKey` | Must be present and stable across restart, rename and move; Apple says support varies by volume |
| Content revision | A serializable generation/version observation, checked before and after each coordinated read | Must change when bytes change and must not change for a metadata-only rename |
| Initial metadata | Bounded enumeration below the selected root | Must represent dataless files without reading their contents |
| Incremental changes | `NSMetadataQuery` updates or a bounded rescan with a committed checkpoint | Must detect create, change, rename, move and delete without losing the last complete snapshot |
| Exact bytes | Materialize if needed, coordinate the read, then stream bounded ranges | Hash must equal a Finder copy of the same current version |
| Cancellation and timeout | Cancel waiting materialization and network streaming without publishing partial bytes | Must leave the prior complete metadata and cache entry usable |
| Permissions | Explicit user-selected root and persisted security-scoped bookmark | Reboot must not silently widen or lose access |

`fileResourceIdentifierKey` is not sufficient: Apple documents that value as not
persistent across system restarts. `documentIdentifierKey` is the promising
candidate because Apple documents it as surviving restart, rename/move on a volume
and safe-save replacement, while also warning that not every volume supports it.
The feasibility probe must measure its actual behavior on iCloud Drive rather than
assuming support.

Cirrove's normal identity remains `(account, provider, collection, item)`. A bridge
must return opaque collection and item identifiers. Linux paths, Mac paths and file
names remain presentation metadata and never become remote identity.

## Read-only milestone I0

The first milestone is **I0: prove a supported, read-only iCloud transport**. It is
complete only when an isolated macOS probe records all of these results on a
dedicated test folder:

1. The user selects the folder through `NSOpenPanel`, quits the probe, starts it
   again and regains only that folder through a security-scoped bookmark.
2. The probe records volume, document and generation identifiers for a directory,
   a materialized file and a dataless file without recording their paths or names as
   identity.
3. Rename and move preserve the document identity. Replacing file bytes changes the
   content revision. Reboot preserves both conclusions.
4. A change made on a second Apple device reaches the probe as a bounded change or
   appears in the next complete rescan. A deliberately interrupted rescan does not
   replace the last complete snapshot.
5. Opening a dataless file requests materialization, returns the exact current bytes
   and can be cancelled. A failed or stale materialization publishes no cache block.
6. A paired test client can list the selected root and read one file while requests
   for a sibling directory and `..` are refused.

The probe is successful only if the evidence distinguishes provider behavior from
the probe's own bookkeeping. Any remote creates, edits, moves or deletes used for
the experiment need a separate, explicit authorization and must be confined to its
owned test folder.

If I0 passes, the next change can add an `icloud-bridge` read adapter behind an
experimental feature and run the existing provider contract tests. If persistent
identity, revision checking or complete refresh cannot be established, the bridge
does not become a Cirrove provider. A manual copy tool can still be considered under
a different product name.

## Writes are a later decision

macOS offers coordinated local writes and reports unresolved ubiquitous-item
conflicts, but that does not yet establish the provider-side compare-and-set and
recovery evidence that Cirrove's write journal needs. The bridge starts read-only.
Create, replace, rename, move and delete each need an explicitly authorized live
probe and an exact post-interruption reconciliation rule before any writable mount
is offered.

In particular, a successful local filesystem call is not proof that Apple accepted
the remote change. The bridge would need to distinguish “queued locally,” “uploaded,”
“conflicted” and “confirmed remote,” and Cirrove must not acknowledge a durable
provider receipt before the last state.

## Security and data boundary

- Pairing creates device credentials stored in the two operating systems' secret
  stores. A short-lived pairing code is not an Apple credential.
- The companion serves opaque item handles rooted in the selected directory. It
  never accepts an arbitrary path from Linux and never follows a result outside the
  selected root.
- Transport is authenticated and encrypted. Revoking the pairing stops new reads;
  removing a Cirrove account follows the existing local-cache retention rules.
- Logs may contain operation names, counts and redacted identities. They do not
  contain Apple credentials, security-scoped bookmarks, paths, file content or raw
  framework/provider responses.
- The first probe listens only on loopback. Network pairing is a second step after
  the local identity and revision gates pass.

## Evidence to retain

I0 should produce a small, reviewable artifact under `docs/benchmarks/` before it
runs: platform and macOS version, selected test topology, operations, expected
identity/revision transitions, interruption points and hashes of owned fixture
bytes. Secret bookmarks, paths and Apple Account identifiers stay outside the
artifact. One passing run is a direction; repeat each identity and interruption arm
before treating it as a result.

## Official references

- [CloudKit](https://developer.apple.com/documentation/cloudkit)
- [CloudKit JS](https://developer.apple.com/documentation/cloudkitjs)
- [CloudKit Web Services](https://developer.apple.com/library/archive/documentation/DataManagement/Conceptual/CloudKitWebServicesReference/index.html)
- [File Provider](https://developer.apple.com/documentation/fileprovider)
- [Accessing files from the macOS App Sandbox](https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox)
- [NSOpenPanel](https://developer.apple.com/documentation/appkit/nsopenpanel)
- [`documentIdentifierKey`](https://developer.apple.com/documentation/foundation/urlresourcekey/documentidentifierkey)
- [iCloud download status](https://developer.apple.com/documentation/foundation/urlubiquitousitemdownloadingstatus)
- [NSFileCoordinator](https://developer.apple.com/documentation/foundation/nsfilecoordinator)
- [NSMetadataQuery](https://developer.apple.com/documentation/foundation/nsmetadataquery)
- [Apple Account app-specific passwords](https://support.apple.com/102654)
- [Upload and download files from iCloud Drive on iCloud.com](https://support.apple.com/guide/icloud/mmad632d1df2/icloud)
- [EU interoperability requests](https://developer.apple.com/support/ios-interoperability/)
- [Account Data Transfer API access](https://developer.apple.com/support/account-data-transfer-api)
