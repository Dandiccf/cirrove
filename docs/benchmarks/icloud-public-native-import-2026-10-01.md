# Public native import acceptance: preregistration

Run identity: `ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc`. Status: prepared, not executed.

Question: does the normal Cirrove daemon's public import request create one
owned Pages document, retain its exact identity through the upload journal, and
publish it into a warm mounted directory before reporting success?

Prediction: the public path produces a verified PACKAGE identical in semantic
contents to the previously created synthetic Pages source. A successful job is
insufficient alone: require independent exact-ID readback and mounted reads.

## Preconditions and bounds

Finish the full project check and synthetic socket-to-FUSE acceptance first. Use
a fresh isolated state directory, socket, mount and writable validation account;
never restart the installed daemon or change existing account registrations.
Reuse only the existing validation session through its established sealed-session
contract; do not print credentials or copy them into configuration.

The source is the verified 55,715-byte archive from owned source run
`ac9e5456-bd10-4b7d-9215-21bbb85dde69`, with its explicit root and semantic receipt.
Verify its retained raw hash/size and source provenance before submission. The
source must be staged as a private local regular file outside Cirrove state and
mounts, without changing archive contents.

Destination: `Cirrove Public Import ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc.pages` in that run's own
`Cirrove Package Validation ac9e5456-bd10-4b7d-9215-21bbb85dde69` folder. Confirm
that exact owned parent and vacant name. Submit once only. Unknown request or
allocation outcome means inspect retained operation; never resubmit a copy.

Record command, source commit/diff, binary hashes, private disk TMPDIR and
SQLITE_TMPDIR, filesystem, PID and duration before starting. No overlapping build
or measurement. Maximum observer20 minutes; source/expanded bounds64MiB. No
personal document, sharing changes, permanent deletion, or recursive cleanup.

## Required evidence

- Public CLI/socket returned job and durable operation IDs; admission alone did
  not report success.
- Exact journal identity, account, collection, parent, name and package semantics
  agree with the independently verified receipt.
- Warm directory shows new package after metadata publication; no content fetch
  is needed solely to show it.
- Normal FUSE read, offline retained read and fresh provider refetch agree in
  root-bound semantic contents. Raw archive representation is measured separately.
- Apple's Pages UI opens only the new owned document and shows the synthetic
  sentinel.
- Cancellation, daemon interruption and uncertain replies remain separate
  synthetic gates; this successful arm does not prove those live failure modes.

This create-only arm does not establish existing native-document replacement,
Numbers/Keynote compatibility, installed acceptance or full iCloud support.
