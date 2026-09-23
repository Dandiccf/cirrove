# Google OAuth verification preparation

This is an internal preparation note, **not** a published privacy policy or a
claim of Google approval. The Cirrove Cloud app is currently External/Testing;
the [dated console audit](benchmarks/google-oauth-release-preflight.json) records
the missing public branding and the scope mismatch. Do not submit or publish the
app until the operator has supplied and approved the public identity, domain and
policy text.

Google's [verification requirements](https://support.google.com/cloud/answer/13464321)
call for a homepage on a verified domain, a linked privacy policy explaining
access, use, storage and sharing of Google data, and an end-to-end demonstration
video for sensitive or restricted scopes. The [restricted-scope guide](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification)
requires an accurate scope declaration. Whether a security assessment applies
to Cirrove's local desktop architecture must be resolved with Google; source
review alone is not approval.

The [Workspace user-data policy](https://developers.google.com/workspace/workspace-api-user-data-developer-policy)
also requires an in-app disclosure immediately before consent, separate from the
public policy. The Google connection dialog, re-sign-in dialog and CLI browser
flow now explain the requested access, local keyring/index/cache, direct Google
writes and local-data removal before sign-in. A native window test and visual
snapshot confirmed the Google disclosure and sign-in button are visible
together; an approved public policy is still needed before a production
submission. The
[Drive terms](https://developers.google.com/workspace/drive/api/terms)
describe local-sync applications as an allowed restricted-scope use, but state
that a security assessment is required; the more specific restricted-scope
guide ties its annual assessment to access from or through a third-party server.
Ask Google to resolve that applicability for Cirrove rather than assuming an
exemption. The policy's cache-retention wording likewise needs review against
Cirrove's on-demand cache and optional offline pins.

## Scope justification to review with the operator

| Mode | Requested Google scope | Why the narrower scope is insufficient |
| --- | --- | --- |
| Read-only | `drive.readonly` | Cirrove lists the selected My Drive or Shared Drive, reads arbitrary existing file metadata and bytes, and exports native Docs/Sheets into read-only packages. `drive.file` does not grant the whole existing collection. |
| Allow changes | `drive` | The mounted filesystem also replaces, renames, moves and trashes existing ordinary files in the selected collection. `drive.file` is limited to app-created or explicitly selected files and cannot fulfill those whole-collection operations. Native Docs/Sheets packages stay read-only. |

The app also requests `openid email profile` for sign-in and account display.
The scope descriptions are in Google's [Drive authorization guide](https://developers.google.com/workspace/drive/api/guides/api-specific-auth).
Cirrove's [Google implementation guide](google-drive.md) separates the current
preview capabilities from unimplemented native writeback. The Cloud Console must
declare both Drive scopes that the two supported modes actually request before
production verification; the audit found only `drive.readonly` declared.

## Demonstration sequence

Record against a dedicated test account and run-owned fixtures, with the final
Cirrove name and branding shown. Google asks the video to show the complete
OAuth consent screen in English and the same scopes submitted for review.

1. Show the public Cirrove homepage, its linked privacy policy and the matching
   URLs in Google Auth Platform. Record only after the operator has approved
   their content and verified domain ownership.
2. In Cirrove, enter a connection name. Show the default local folder, choose
   Google Drive and leave **Allow changes** off. Show the data notice above
   sign-in. The optional custom OAuth app row is not part of the normal flow;
   no user JSON is needed.
3. Complete browser consent for the read-only mode, select My Drive or a
   run-owned Shared Drive, and show ordinary existing file listing, an on-demand
   read and a native Doc/Sheet read-only export package. Show that a native
   package cannot be edited through Cirrove.
4. With a separate run-owned connection or explicit reauthentication, enable
   **Allow changes** and show its data notice and the full-Drive consent screen.
   Create, replace, rename, move and trash only run-owned ordinary files in
   the selected test collection; show that Cirrove reports the completed upload.
5. Show **Remove**, `cirrove local-data`, and the documented separate steps for
   local-data discard, keyring grant removal and Google-side consent revocation.

The recording is evidence for Google's review, not proof of provider reliability.
The isolated Shared Drive duration and late uncached 16 MiB read probes are now
recorded as bounded passes; finish the repository checks for the recording branch
before recording. Do not
include access tokens, refresh tokens, signed URLs, raw provider bodies or
unrelated Drive content in the recording or its notes.

## Candidate homepage wording for operator review

**Internal draft only.** Publish only after the operator has chosen and verified
their domain, supplied contact details and approved the final site.

> Cirrove brings a cloud drive into the Linux file manager as a local folder.
> You choose one Google My Drive or Shared Drive for each connection. Cirrove
> shows file names from that drive, downloads content when opened and can keep
> selected files available offline. With Allow changes, ordinary file saves
> are sent back to Google; without it, the connection is read-only. Native
> Google Docs and Sheets appear as read-only export packages. Cirrove is a
> pre-release preview under validation, not a general Google Drive release.
>
> [How to connect Google Drive] · [Privacy policy] · [Source code] ·
> [How to remove local data and revoke Google access] ·
> [Operator contact]

The privacy-policy link must point to the same URL as the OAuth consent screen
and be hosted on the verified homepage domain. The final page must not imply
Google approval, a tagged release or native document writeback before those
facts change.

## Candidate Google-data wording for operator review

**Internal draft only.** This is not a complete privacy policy and must not be
used as the OAuth privacy-policy URL. The operator must supply their identity,
contact details, domain, applicable legal disclosures and final approval.

> Cirrove is a Linux desktop app that presents a Google Drive you select as a
> local folder. To connect it, Cirrove receives your Google account identifier,
> name and email address. A read-only connection requests permission to read
> Drive files; turning on Allow changes requests permission to read and change
> Drive files. Google's grant covers files across your account, while Cirrove
> mounts the drive you select. Cirrove lists file names and metadata, downloads
> content when you open or keep a file offline, and sends ordinary-file edits
> back to Google when you allow changes. Native Google Docs and Sheets remain
> read-only export packages in Cirrove.
>
> Cirrove keeps account details, file names and metadata in local state on
> your computer. It keeps downloaded content in a bounded local cache; files
> you choose to keep offline remain pinned there until you release them.
> Sign-in credentials are kept in the desktop keyring. The desktop connection
> talks directly to Google and does not send your files to a Cirrove-operated
> server. Cirrove does not use Google data for ads, sale, credit decisions or
> training general-purpose models.
>
> Removing a connection stops it and sets its local data aside for recovery;
> it does not delete files from Google. You can separately discard that
> removed local data, remove Cirrove's keyring grant and revoke Cirrove's
> access in your Google Account. Uninstalling the packages alone does not
> remove local state or the keyring grant. Google's user-data restrictions,
> including their Limited Use rules, govern Cirrove's use of data received
> through Google Workspace permissions.

The public text must link the exact removal steps in the
[user guide](user-guide.md#removing-cirrove), and its claims need a
final code and policy review. In particular, Google's
[Workspace policy](https://developers.google.com/workspace/workspace-api-user-data-developer-policy)
mentions cache lifetime limits while the
[Drive terms](https://developers.google.com/workspace/drive/api/terms)
list local-sync apps as eligible for restricted scopes. Do not publish a
retention promise until the cache and offline-pin implications are resolved.
Section 5(e) of
the [Google APIs terms](https://developers.google.com/terms) limits permanent
and cached copies absent permission from the content owner or applicable law;
whether a user-requested offline pin meets that condition, especially for
Shared Drive content owned by an organization, needs explicit review. The
published text must not claim a resolved policy interpretation on this record.

## Inputs still required

- The existing GitHub repository page is not an owned, DNS-verifiable homepage
  domain for Google's production review. The operator chose to keep the
  homepage and privacy wording as an internal draft for now. A later GitHub
  Pages site could host them under an operator-owned custom domain, after the
  Cloud project owner verifies that domain through Search Console. See
  [Google's domain verification guidance](https://support.google.com/cloud/answer/13804266).
- An operator-controlled public domain and verified Search Console ownership.
- The operator's public identity/contact and approved homepage, privacy-policy
  and terms wording, including accurate local retention and removal behavior.
- Approval for any Cloud Console configuration changes and the final
  verification submission. The source and live preview evidence do not grant
  those approvals or substitute for Google's review.
