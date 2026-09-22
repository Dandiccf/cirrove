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
   Google Drive and leave **Allow changes** off. The optional custom OAuth app
   row is not part of the normal flow; no user JSON is needed.
3. Complete browser consent for the read-only mode, select My Drive or a
   run-owned Shared Drive, and show ordinary existing file listing, an on-demand
   read and a native Doc/Sheet read-only export package. Show that a native
   package cannot be edited through Cirrove.
4. With a separate run-owned connection or explicit reauthentication, enable
   **Allow changes** and show the full-Drive consent screen. Create, replace,
   rename, move and trash only run-owned ordinary files in the selected test
   collection; show that Cirrove reports the completed upload.
5. Show **Remove**, `cirrove local-data`, and the documented separate steps for
   local-data discard, keyring grant removal and Google-side consent revocation.

The recording is evidence for Google's review, not proof of provider reliability.
Before recording, finish the native-window/default-client test, the isolated
Shared Drive duration probe and the repository checks in this branch. Do not
include access tokens, refresh tokens, signed URLs, raw provider bodies or
unrelated Drive content in the recording or its notes.

## Inputs still required

- An operator-controlled public domain and verified Search Console ownership.
- The operator's public identity/contact and approved homepage, privacy-policy
  and terms wording, including accurate local retention and removal behavior.
- Approval for any Cloud Console configuration changes and the final
  verification submission. The source and live preview evidence do not grant
  those approvals or substitute for Google's review.
