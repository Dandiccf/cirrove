//! Broken native authority must not prevent independent byte recovery.
use super::*;
#[test]
fn missing_or_corrupt_native_binding_preserves_exact_dirty_and_sealed_ro_exports() {
    for sealed in [false, true] {
        for damage in ["missing", "malformed", "wrong_scope"] {
            let t = temp();
            let initial = bytes(b"initial synthetic source");
            let mut j = journal(&t);
            let working = publish(&mut j, binding(&t, &initial), &initial);
            let sealed_bytes = bytes(b"first accepted archive generation");
            let save = if sealed {
                edit(&mut j, working.id, &sealed_bytes);
                let capture = j
                    .capture_native_working(working.id)
                    .unwrap()
                    .capture(&CancellationToken::new())
                    .unwrap();
                Some(
                    j.seal_captured_native_working(capture, &CancellationToken::new())
                        .unwrap(),
                )
            } else {
                None
            };
            // Intentionally invalid ZIP: recovery must not require native parsing.
            let dirty = b"accepted incomplete native save\0with binary tail\xff";
            edit(&mut j, working.id, dirty);
            let generation = j.working_file(working.id).unwrap().generation;
            match damage {
                "missing" => {
                    j.db.execute(
                        "DELETE FROM native_working_bindings WHERE working=?1",
                        [working.id.to_string()],
                    )
                    .unwrap();
                }
                "malformed" => {
                    j.db.execute(
                        "UPDATE native_working_bindings SET body='{' WHERE working=?1",
                        [working.id.to_string()],
                    )
                    .unwrap();
                }
                _ => {
                    j.db.execute("UPDATE native_working_bindings SET body=json_set(body,'$.scope.account','foreign') WHERE working=?1", [working.id.to_string()]).unwrap();
                }
            }
            assert!(j.capture_native_working(working.id).is_err());
            assert_eq!(count(&j), i64::from(sealed));
            drop(j);
            let db_path = t.path().join("journal/uploads.db");
            let before = std::fs::read(&db_path).unwrap();
            let ro = RecoveryJournal::open(&t.path().join("journal"), "native-working").unwrap();
            let listed = ro.working_recovery_list(None, 20).unwrap().0;
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].file, working.id);
            assert_eq!(listed[0].generation, generation);
            assert_eq!(listed[0].size, dirty.len() as u64);
            let destination = t.path().join("dirty-recovery.bin");
            let prepared = ro
                .working_export_source(working.id, generation)
                .unwrap()
                .prepare_copy(&destination, &CancellationToken::new(), |_| {})
                .unwrap();
            let receipt = ro
                .verify_working_export(prepared)
                .unwrap()
                .publish(&CancellationToken::new())
                .unwrap();
            assert_eq!(receipt.source.file, working.id);
            assert_eq!(receipt.source.generation, generation);
            assert_eq!(receipt.sha256, hex::encode(Sha256::digest(dirty)));
            assert_eq!(std::fs::read(&destination).unwrap(), dirty);
            if let Some(save) = save {
                let destination = t.path().join("sealed-recovery.zip");
                let receipt = ro
                    .local_export_source(save.id)
                    .unwrap()
                    .copy_to(&destination, &CancellationToken::new(), |_| {})
                    .unwrap();
                assert_eq!(receipt.operation, save.id);
                assert_eq!(receipt.size, sealed_bytes.len() as u64);
                assert_eq!(receipt.sha256, hex::encode(Sha256::digest(&sealed_bytes)));
                assert_eq!(std::fs::read(destination).unwrap(), sealed_bytes);
            }
            drop(ro);
            assert_eq!(
                std::fs::read(&db_path).unwrap(),
                before,
                "{sealed} {damage}"
            );
        }
    }
}
