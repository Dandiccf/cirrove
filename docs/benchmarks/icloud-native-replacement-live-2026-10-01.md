# Native iCloud replacement live validation — 2026-10-01

Run: `42a313de-1e94-488f-aed8-e7c704944fed`. Status: preparing an owned edited Pages source; no Cirrove
replacement submitted yet.

## Registered question and prediction

Can the public explicit replacement command replace an owned Pages PACKAGE with
a separately valid, edited Pages archive while retaining exact original bytes
and a historical Trash receipt, then expose the new document at the same mount
path and open its changed contents in Apple Pages? Prediction: one new identity
and one original recovery identity, independent semantic proofs, no replay after
rejoining the saved operation. This does not prove normal application saves or
sharing/history preservation.

## Preparation scope

Use Apple Pages Duplicate on the owned document
`Cirrove Public Import ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc`, then rename only the
new copy to `Cirrove Replacement Source 42a313de-1e94-488f-aed8-e7c704944fed` and edit synthetic text there.
Keep the original import unchanged. Export the owned copy as native Pages. No
other user files, sharing settings or permanent deletion are in scope. Record
actual identity, representation and archive proof before any Cirrove replacement;
if export is DATA instead of a valid PACKAGE source, report it rather than
relabeling the representation. Live command/owner manifest must precede submission.

Pending: duplicate/edit/export, actual source capture, public workflow test,
owned live replacement, independent old/new content and Apple app verification.

## Owned source preparation observed

Apple Pages duplicated the owned import and saved the uniquely named version-B
copy. Its UI document ID is `3EA9FB30-0110-4798-9DF7-C662A3E9833D`. The screenshot
shows the new run marker and original synthetic text. Native Pages export yielded
a flat ZIP: 109812 bytes, 12 entries, 108248 expanded bytes; SHA-256
`2f18fc8fba5c2161150d5cae924343268c40f3d67a5abca3944877b17dc8a13b`. It is retained unchanged alongside a ZIP with the
explicit `Replacement.pages/` root added to each entry, without changing entry
bytes. This structural conversion is not evidence of remote DATA/PACKAGE type.
Artifacts and preparation manifest: `/var/tmp/cirrove-native-replacement-live-42a313de-1e94-488f-aed8-e7c704944fed`.
No Cirrove replacement has been submitted. Next inspect exact remote metadata
and semantic binding, then use only the public replacement route once ready.
