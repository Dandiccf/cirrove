# iCloud document metadata shape investigation

Question: does the account's root and Apple Pages/Numbers/Keynote listing expose
provider metadata that distinguishes document bundles and app containers from
ordinary files/folders? The normal adapter currently drops unmodelled fields and
sets `Node.package = false`; filenames cannot establish package semantics.

Prediction before the run: root app containers are distinguishable by `type`
and/or metadata fields, while native documents may require more than the folder
facet. A missing field in this sample does not prove that Apple never supplies it.

Read-only arm: use the existing isolated session of the successful mounted
empty/refill fixture `21d05f56-70c4-47a8-a5e3-577ab68994ac`. Request only root and
up to three fixed Apple document container identities found there. Maximum four
listing requests, each bounded by the normal response-size and 90-second listing
timeout. Outer timeout 420 seconds. No recursive scan, download or cloud mutation.

Output is a histogram of top-level field names and JSON types/boolean counts,
plus counts of the four supported provider kind strings (all others become
`other`). Names, IDs, string values, nested objects, raw bodies and signed URLs
are never output. Session, installed daemon and user files remain unchanged.
The dedicated synthetic test checks that private string and nested values do
not enter this report. This is discovery, not acceptance of document writes.

Source inspection: [pyicloud drive.py](https://github.com/picklepete/pyicloud/blob/master/pyicloud/services/drive.py)
and [rclone drive.go](https://github.com/rclone/rclone/blob/master/backend/iclouddrive/api/drive.go)
show package-download handling but do not establish a reliable package metadata
classifier for this account. Neither implementation is introduced as a dependency.

Record binary hash, command, private disk-backed temporary directory, process ID
and terminal status before interpreting the result. Keep the result here with
its limits; do not turn an absent example into universal package support.

## First result and representation follow-up

The first arm `5fec475f-d1c6-4f3e-afa9-bd78cc69ee84` passed in 1.920 seconds.
Root contained 18 APP_LIBRARY, 132 FOLDER and four FILE entries. Pages contained
36 FILE entries, Numbers 23 FILE and one FOLDER, and Keynote 11 FILE entries.
No explicit package flag appeared among top-level keys. Sharing-related fields
were present and are not currently modelled by DriveEntry. This is a sampled
schema observation, not a claim that all FILE entries are ordinary blobs.

Follow-up question: does one native document per app advertise `data_token`,
`package_token` or both? Prediction: at least one native document may use the
package representation despite its FILE listing kind. Select only the first FILE
with the app's exact native extension in each fixed app listing; issue at most
three download-location lookups, report presence booleans only, and do not fetch
any returned URL. At most seven requests total, outer timeout 720 seconds.
This metadata lookup does not alter a document or download its contents. Preserve
the first result separately; the follow-up cannot retroactively change it.

## Representation result

Arm `9e177f26-cd5d-4884-b1d5-56f441fb6c96` passed in 3.021 seconds.
The selected Pages document supplied only `package_token`; the selected Numbers
document supplied only `data_token`. No content URL was fetched. Both entries
were FILE in their listings. This directly disproves treating the FILE kind as
proof of an ordinary downloadable blob. It does not establish that all Pages
or Numbers documents have the same representation.

No Keynote sample was selected: the diagnostic initially compared its extension
to `keynote`, whereas the native extension is `key`. The selector is corrected
and has a focused test; that correction is subsequent to this live binary.
There is no Keynote representation result in this arm. Keep this limitation with
the result instead of counting an unselected sample as a successful check.

Next implementation requirement: retain the distinction between data and package
representations through reads and write preflight. A package download must not
silently inherit the byte length, range semantics or overwrite eligibility of
an ordinary blob. The current normal write gate remains closed. Shared-document
identity and app-container semantics remain separate open requirements. No
provider behavior is changed by this diagnostic commit.

## Validation

The final `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` passed
with exit 0, including feature-enabled diagnostic tests, workspace tests,
real-kernel FUSE tests, scripts and the acceptance ledger. The existing rustdoc
link warning remains. Graphical window tests and installed-daemon acceptance were
not run. Both live arms are terminal and read-only; no measurement remains live.
