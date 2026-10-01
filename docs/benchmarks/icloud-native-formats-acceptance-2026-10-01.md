# Native document formats: prepared acceptance boundary

Status: implementation integrated in the isolated development worktree. Targeted synthetic controls passed as recorded in [the red/green record](icloud-native-formats-redgreen-2026-10-02.md). No Apple application acceptance, installation or release gate is completed by this change.

The proposed common path accepts Pages, Numbers and Keynote **actual PACKAGE** objects. An extension only selects a supported format; it never establishes provider representation. Existing DATA downloads and FileBytes uploads are unchanged. Resolver admission still requires an exact generated archive/source identity, fresh owned CloudDocs folder ancestry, a PACKAGE download representation, bounded ZIP validation, semantic proof and unchanged postflight source. App containers/libraries and ambiguous DATA/PACKAGE identities remain refused.

Explicit import captures and validates local ZIP contents before enqueue. Archive root and destination suffix must identify the same supported format. Explicit replacement additionally verifies the exact current original PACKAGE and revision. Trash remains one exact original native container, never generated archive children or recursive package contents. Stored semantic versions, handoff phases and checkpoint formats remain unchanged; no saved operation is replayed or converted.

The desktop import chooser/form and CLI descriptions are extended together. Existing native command capabilities and wire formats are unchanged. An older daemon remains authoritative and may refuse Numbers/Keynote; a newer UI cannot make that daemon accept them. Replacement and Trash retain their existing public command surfaces; this patch adds no new desktop action.

## Synthetic coverage (targeted controls executed; full suite pending)

- Real resolver HTTP fixtures for each format: exact cached archive succeeds; changing representation to DATA, app-container/library ancestry, wrong zone, missing item or changed revision refuses. Cached semantic proof does not bypass fresh classification.
- Real Manager capture/admission: raw DATA bytes and wrong-root ZIP refuse without durable enqueue; matching-root archive creates exactly one explicit PackageArchive row.
- Public selection validators require Folder + package and exact item/revision/parent/name for all formats. A same-suffix ordinary File is refused by native selection.
- Desktop form permits matching format pairs and rejects every cross-format pair; existing unsafe names and paths remain refused.
- Internal format prerequisites separately exercise typed replacement/handoff pairing, generic core neutrality and legacy Pages checkpoint compatibility.

## Remaining owned-fixture live application gates

No personal document may be selected opportunistically. Create a new UUID-owned document independently in each Apple application, record its representation (DATA or PACKAGE) rather than infer it from its name, and preregister each mutation arm and expected result before execution. A DATA fixture validates the ordinary FileBytes route; it cannot count as evidence for PACKAGE replacement.

For each actual PACKAGE format, retain original bytes/proof and test public import, fresh independent semantic readback, normal mount/offline/refetch, and application-open fidelity. Then use a separate owned copy for editing and replacing with changed content: reopen in the corresponding application and verify application-specific content (Numbers cell/formula values; Keynote slide/text/media; Pages document content). Validate held readers, interrupted/restarted work, retained raw export, two-ID handoff and recovery. Trash/restore must target only the exact UUID-owned original identity and revision. Do not claim history/sharing identity preservation: current replacement is a two-ID handoff.

A synthetic ZIP with matching hashes proves transport and admission invariants, not that Numbers or Keynote accepts the document. Full-iCloud ledger 487 remains open until those application arms and remaining release requirements have evidence. No live run is authorized or scheduled by this document alone.
