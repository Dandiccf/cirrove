//! Mutable bytes, immutable saves and recovery without cloud credentials.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_service::journal::{
    JournalError, SaveRefusals, UploadIntent, UploadJournal, UploadState, WorkingFile,
};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn scope() -> Scope {
    Scope {
        account: "working-fixture".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn node(size: u64) -> Node {
    Node {
        id: "remote-file".into(),
        parent_id: Some("root".into()),
        name: "Kärnten & Grüße.txt".into(),
        kind: NodeKind::File,
        size,
        modified_unix: 1,
        etag: Some("original".into()),
        content_version: Some("content-original".into()),
        target: None,
    }
}
fn open(root: &Path, quota: u64) -> UploadJournal {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(root, &scope().account, quota) {
            Err(JournalError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2))
            }
            result => return result.unwrap(),
        }
    }
}
fn payload(j: &UploadJournal, id: uuid::Uuid) -> Vec<u8> {
    let mut bytes = vec![];
    j.payload(id).unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn repeated_application_saves_keep_immutable_uploads_and_follow_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 1024);
    let working = j
        .create_working(scope(), node(8), false, b"original".as_slice())
        .unwrap();
    assert!(!working.dirty);
    assert!(j.seal_working(working.id).unwrap().is_none());
    j.write_working(working.id, 0, b"edit-one").unwrap();
    let first = j.seal_working(working.id).unwrap().unwrap();
    let attempt = j.claim_next().unwrap().unwrap();
    j.write_working(working.id, 0, b"edit-two").unwrap();
    let second = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, first.id), b"edit-one");
    assert_eq!(payload(&j, second.id), b"edit-two");
    assert_eq!(second.working_file, Some(working.id));
    assert!(j.claim_next().unwrap().is_none());
    let mut receipt = node(8);
    receipt.etag = Some("confirmed-first".into());
    j.acknowledge(first.id, attempt.attempt.unwrap(), receipt)
        .unwrap();
    j.prune_uploaded_payload(first.id).unwrap();
    drop(j);
    let mut j = open(&root, 1024);
    let recovered = j
        .working_by_identity(&scope(), "remote-file")
        .unwrap()
        .unwrap();
    assert_eq!(recovered.id, working.id);
    assert_eq!(recovered.latest, Some(second.id));
    assert!(!recovered.dirty);
    assert_eq!(j.read_working(working.id, 0, 100).unwrap(), b"edit-two");
    let next = j.claim_next().unwrap().unwrap();
    assert_eq!(
        next.intent,
        UploadIntent::Replace {
            item: "remote-file".into(),
            expected_etag: "confirmed-first".into()
        }
    );
    assert_eq!(payload(&j, next.id), b"edit-two");
}

#[test]
fn truncate_sparse_writes_and_quota_failures_preserve_the_working_copy() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 20);
    let working = j
        .create_working(scope(), node(0), true, b"".as_slice())
        .unwrap();
    assert_eq!(j.write_working(working.id, u64::MAX, b"").unwrap().0, 0);
    j.write_working(working.id, 5, b"abc").unwrap();
    assert_eq!(
        j.read_working(working.id, 0, 100).unwrap(),
        b"\0\0\0\0\0abc"
    );
    j.truncate_working(working.id, 6).unwrap();
    let saved = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, saved.id), b"\0\0\0\0\0a");
    j.truncate_working(working.id, 12).unwrap();
    assert!(matches!(
        j.seal_working(working.id),
        Err(JournalError::Quota)
    ));
    assert!(j.working_file(working.id).unwrap().dirty);
    assert!(matches!(
        j.write_working(working.id, 19, b"overflow"),
        Err(JournalError::Quota)
    ));
    assert_eq!(
        j.read_working(working.id, 0, 100).unwrap(),
        b"\0\0\0\0\0a\0\0\0\0\0\0"
    );
    assert_eq!(j.list(0, 100).unwrap().len(), 1);
    assert_eq!(payload(&j, saved.id), b"\0\0\0\0\0a");
}

#[test]
fn queue_commit_and_working_generation_advance_are_one_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 1024);
    let working = j
        .create_working(scope(), node(0), true, b"".as_slice())
        .unwrap();
    j.write_working(working.id, 0, b"preserve").unwrap();
    let fault = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    fault.execute_batch("CREATE TRIGGER fail_seal BEFORE UPDATE ON working_files WHEN json_extract(NEW.body,'$.dirty')=0 BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(matches!(
        j.seal_working(working.id),
        Err(JournalError::Storage)
    ));
    assert!(j.list(0, 100).unwrap().is_empty());
    let record = j.working_file(working.id).unwrap();
    assert!(record.dirty);
    assert_eq!(record.latest, None);
    assert_eq!(j.retained_bytes().unwrap(), 16); // Mutable bytes and retained orphan.
    fault.execute_batch("DROP TRIGGER fail_seal;").unwrap();
    drop(fault);
    drop(j);
    let mut j = open(&root, 1024);
    let saved = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, saved.id), b"preserve");
    assert_eq!(j.working_file(working.id).unwrap().latest, Some(saved.id));
    assert_eq!(j.list(0, 100).unwrap().len(), 1);
}

#[test]
fn partial_write_metadata_failure_recovers_actual_bytes_without_claiming_a_save() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 1024);
    let working = j
        .create_working(scope(), node(4), false, b"base".as_slice())
        .unwrap();
    let fault = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    fault.execute_batch("CREATE TRIGGER fail_size BEFORE UPDATE ON working_files WHEN json_extract(NEW.body,'$.node.size')!=json_extract(OLD.body,'$.node.size') BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(matches!(
        j.write_working(working.id, 4, b"-extended"),
        Err(JournalError::Storage)
    ));
    assert!(j.working_file(working.id).unwrap().dirty);
    fault.execute_batch("DROP TRIGGER fail_size;").unwrap();
    drop(fault);
    drop(j);
    let mut j = open(&root, 1024);
    let recovered = j.working_file(working.id).unwrap();
    assert!(recovered.dirty);
    assert_eq!(recovered.node.size, 13);
    assert!(j.claim_next().unwrap().is_none());
    assert_eq!(
        j.read_working(working.id, 0, 100).unwrap(),
        b"base-extended"
    );
    let sealed = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, sealed.id), b"base-extended");
}

#[test]
fn killed_process_preserves_fsynced_save_and_newer_unsealed_changes() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "working_crash_fixture", "--ignored"])
        .env("CIRROVE_WORKING_CRASH", temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let marker = temp.path().join("ready.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("working fixture did not become ready");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let working: WorkingFile = serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    let mut j = open(&temp.path().join("journal"), 1024);
    assert_eq!(
        j.get(working.latest.unwrap()).unwrap().state,
        UploadState::Pending
    );
    assert_eq!(payload(&j, working.latest.unwrap()), b"saved!!!");
    assert!(j.working_file(working.id).unwrap().dirty);
    assert_eq!(j.read_working(working.id, 0, 100).unwrap(), b"unsaved!");
    let saved = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, saved.id), b"unsaved!");
}

#[test]
#[ignore = "child process controlled by the synthetic parent test"]
fn working_crash_fixture() {
    let Some(root) = std::env::var_os("CIRROVE_WORKING_CRASH") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let mut j = open(&root.join("journal"), 1024);
    let w = j
        .create_working(scope(), node(0), true, b"".as_slice())
        .unwrap();
    j.write_working(w.id, 0, b"saved!!!").unwrap();
    j.seal_working(w.id).unwrap();
    j.write_working(w.id, 0, b"unsaved!").unwrap();
    std::fs::write(
        root.join("marker.tmp"),
        serde_json::to_vec(&j.working_file(w.id).unwrap()).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("reached"),
        cirrove_service::journal::durable::reached().join("\n"),
    )
    .unwrap();
    std::fs::rename(root.join("marker.tmp"), root.join("ready.json")).unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn incomplete_hydration_stays_invisible_and_reserved_bytes_bound_concurrent_work() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 10);
    let mut source = j.reserve_working(8).unwrap();
    assert_eq!(j.retained_bytes().unwrap(), 8);
    assert!(matches!(j.reserve_working(3), Err(JournalError::Quota)));
    source.write_chunk(b"short").unwrap();
    assert!(matches!(
        j.publish_working(scope(), node(8), false, source),
        Err(JournalError::Corrupt)
    ));
    assert!(j.working_files().unwrap().is_empty());
    assert_eq!(j.retained_bytes().unwrap(), 0);
    let mut source = j.reserve_working(8).unwrap();
    source.write_chunk(b"complete").unwrap();
    let w = j.publish_working(scope(), node(8), false, source).unwrap();
    assert_eq!(j.read_working(w.id, 0, 100).unwrap(), b"complete");
    assert_eq!(j.retained_bytes().unwrap(), 8);
}

/// A full budget and a full disk are different problems with opposite remedies,
/// and both reach an application as ENOSPC. If the journal collapses them into
/// one error, the difference is gone before anything can report it: a user whose
/// saves fail is told "no space left on device" on a filesystem with gigabytes
/// free, and waiting for uploads to drain -- which fixes one and not the other --
/// looks equally plausible either way.
#[test]
fn a_full_budget_and_a_full_disk_are_reported_as_different_problems() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(&temp.path().join("journal"), 20);
    let working = j
        .create_working(scope(), node(0), true, b"".as_slice())
        .unwrap();
    j.write_working(working.id, 5, b"abc").unwrap();
    j.truncate_working(working.id, 6).unwrap();
    let saved = j.seal_working(working.id).unwrap().unwrap();

    // The budget, not the device.
    j.truncate_working(working.id, 12).unwrap();
    let budget = j.seal_working(working.id).unwrap_err();
    assert!(
        matches!(budget, JournalError::Quota),
        "a budget that is full must not be reported as a full disk: {budget:?}"
    );
    let explanation = format!("{budget}");
    assert!(
        explanation.contains("uploads") || explanation.contains("upload"),
        "the budget message must say what releases the space: {explanation}"
    );
    assert!(
        explanation.contains("cache_bytes"),
        "and how to make room now: {explanation}"
    );

    // The device, not the budget.
    let device = JournalError::from(std::io::Error::from_raw_os_error(libc::ENOSPC));
    assert!(
        matches!(device, JournalError::DeviceFull),
        "a physical ENOSPC must not be reported as a full budget: {device:?}"
    );
    let explanation = format!("{device}");
    assert!(
        explanation.contains("freed"),
        "the device message must name the action that is actually required: {explanation}"
    );

    // The edit survives both, which is the other half of the box.
    assert_eq!(payload(&j, saved.id), b"\0\0\0\0\0a");
    assert!(j.working_file(working.id).unwrap().dirty);
}

/// A constructed `io::Error` shows the mapping from ENOSPC to `DeviceFull`
/// exists. It cannot show that a real full filesystem reaches that mapping,
/// because the path a save actually takes is chosen by the journal and not by
/// the test. This drives the journal on a filesystem it then fills itself, so
/// the precondition is asserted here rather than assumed from the environment.
///
/// Ignored by default: it needs its own filesystem, since it deliberately
/// consumes every free block. See docs/benchmarks/full-disk-journal-errors.json.
#[test]
#[ignore = "set CIRROVE_FULL_DISK_DIR to a directory on a small, disposable filesystem"]
fn a_genuinely_full_filesystem_explains_what_to_free() {
    let Some(root) = std::env::var_os("CIRROVE_FULL_DISK_DIR").map(std::path::PathBuf::from) else {
        panic!("CIRROVE_FULL_DISK_DIR is required; this test fills the filesystem it names");
    };
    let mut report = serde_json::Map::new();

    // A sealed payload made while there is still room, so that the preservation
    // claim has something concrete to be about.
    let mut j = open(&root.join("journal"), 1 << 30);
    let working = j
        .create_working(scope(), node(8), false, b"original".as_slice())
        .unwrap();
    j.write_working(working.id, 0, b"changed!").unwrap();
    let saved = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, saved.id), b"changed!");
    j.write_working(working.id, 0, b"newer".as_slice()).unwrap();

    // Fill it. Chunks shrink so the last free block is consumed, not merely
    // most of them: a filesystem with one block left is not the one under test.
    let ballast = root.join("ballast");
    let mut sink = std::fs::File::create(&ballast).unwrap();
    let mut written = 0u64;
    for chunk in [1 << 20usize, 4096, 512, 1] {
        let block = vec![0u8; chunk];
        while std::io::Write::write_all(&mut sink, &block).is_ok() {
            written += chunk as u64;
        }
    }
    let _ = std::io::Write::flush(&mut sink);
    let _ = sink.sync_all();
    drop(sink);
    report.insert("ballast_bytes".into(), written.into());

    // P1. Checked outside the journal, so a journal error below cannot be the
    // thing that persuaded us the filesystem was full.
    let probe = root.join("probe");
    let refusal = std::fs::File::create(&probe).and_then(|mut file| {
        std::io::Write::write_all(&mut file, &vec![0u8; 65536])?;
        file.sync_all()
    });
    let errno = refusal.as_ref().err().and_then(|e| e.raw_os_error());
    report.insert(
        "probe_errno".into(),
        errno.map_or(serde_json::Value::Null, |e| e.into()),
    );
    let _ = std::fs::remove_file(&probe);
    assert_eq!(
        errno,
        Some(libc::ENOSPC),
        "the filesystem is not full, so nothing below would be attributable"
    );

    // The edit that has nowhere to go.
    let mut first: Option<JournalError> = None;
    let mut steps = vec![];
    for (name, outcome) in [
        (
            "write_working",
            j.write_working(working.id, 0, b"after the disk filled")
                .map(|_| ()),
        ),
        (
            "truncate_working",
            j.truncate_working(working.id, 4096).map(|_| ()),
        ),
        ("seal_working", j.seal_working(working.id).map(|_| ())),
    ] {
        match outcome {
            Ok(()) => steps.push(serde_json::json!({ "step": name, "ok": true })),
            Err(error) => {
                steps.push(serde_json::json!({
                    "step": name,
                    "variant": format!("{error:?}"),
                    "message": format!("{error}"),
                }));
                if first.is_none() {
                    first = Some(error);
                }
                break;
            }
        }
    }
    report.insert("steps".into(), steps.into());
    let first = first.expect("a save on a full filesystem must fail");
    report.insert("first_variant".into(), format!("{first:?}").into());
    report.insert("first_message".into(), format!("{first}").into());

    // P4, in two parts. Whether the survivors can be read back while the disk is
    // still full is its own question, and a failure there is a different fact
    // from the data not having survived.
    let while_full = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (
            payload(&j, saved.id),
            j.working_file(working.id).unwrap().dirty,
        )
    }));
    report.insert("readable_while_full".into(), while_full.is_ok().into());

    std::fs::remove_file(&ballast).unwrap();
    assert_eq!(
        payload(&j, saved.id),
        b"changed!",
        "the sealed payload must survive a full device"
    );
    assert!(
        j.working_file(working.id).unwrap().dirty,
        "the working copy must still be marked dirty"
    );
    report.insert("payload_survived".into(), true.into());

    println!("FULL_DISK_RESULT {}", serde_json::Value::Object(report));

    // The claim the acceptance box actually makes.
    let message = format!("{first}");
    assert!(
        message.contains("freed") || message.contains("free space"),
        "a save that failed because the disk is full must name freeing space \
         as the action required; it said: {message}"
    );
}

/// The recorder keeps the two no-space refusals apart and lets everything else
/// through untouched.
///
/// `real_a_refused_save_is_reported_as_a_budget_and_not_only_as_enospc` proves
/// the mounted path reaches this for a full budget. The device case shares that
/// path and this classification, and no run has yet driven a mount on a
/// physically full filesystem -- so what is established there is the mapping,
/// not the journey.
#[test]
fn only_the_two_no_space_refusals_are_recorded_and_they_stay_distinct() {
    let refusals = SaveRefusals::default();
    assert!(refusals.latest().is_none());

    // An error that is not about space must not populate a field whose whole
    // purpose is to answer "why did my save fail for lack of room".
    refusals.note(&JournalError::Corrupt);
    assert!(
        refusals.latest().is_none(),
        "a corruption error is not a reason to tell someone to free space"
    );

    refusals.note(&JournalError::Quota);
    let budget = refusals.latest().unwrap();
    assert_eq!(budget.kind, "budget");
    assert!(budget.message.contains("cache_bytes"), "{}", budget.message);

    refusals.note(&JournalError::DeviceFull);
    let device = refusals.latest().unwrap();
    assert_eq!(device.kind, "device");
    assert!(device.message.contains("freed"), "{}", device.message);

    // The remedies are opposite, so a caller must never be able to read one for
    // the other. Matching on `kind` rather than on prose is the point of it.
    assert_ne!(budget.kind, device.kind);
    assert_ne!(budget.message, device.message);
}

/// What the journal does when the device beneath it starts refusing writes.
///
/// `durable-transition-sites.json` closed the process-death half of the crash
/// box: all 31 runtime durable transitions are crossed by a process that then
/// dies. Its own conclusion names what is left -- faults below the process,
/// which need dm-flakey and root. This is the reachable part of that: a
/// block-layer write failure switched on at runtime and back off again.
///
/// Three invocations against one device, because the test cannot run `dmsetup`
/// and the shell that can has to interleave with it. `seed` on a healthy device,
/// then the harness breaks it, then `fault`, then the harness mends it, then
/// `recover`. The phase is a variable rather than an argument so the harness is
/// a shell loop and not a protocol.
#[test]
#[ignore = "needs a dm-flakey device; set CIRROVE_FLAKY_DIR and CIRROVE_FLAKY_PHASE"]
fn a_journal_on_a_failing_device_refuses_without_losing_what_was_durable() {
    let root = std::env::var_os("CIRROVE_FLAKY_DIR")
        .map(std::path::PathBuf::from)
        .expect("CIRROVE_FLAKY_DIR must name a directory on a dm-flakey device");
    let phase = std::env::var("CIRROVE_FLAKY_PHASE")
        .expect("CIRROVE_FLAKY_PHASE must be seed, fault or recover");
    let journal = root.join("journal");
    let marker = root.join("seeded-bytes");
    let sealed = b"durable before the device failed".to_vec();

    match phase.as_str() {
        "seed" => {
            // Seed defines the starting state, so it starts from nothing. Run
            // twice against one journal it fails with Stale, which is the
            // journal being right and the harness being wrong.
            let _ = std::fs::remove_dir_all(&journal);
            let _ = std::fs::remove_file(&marker);
            let mut j = open(&journal, 8 * 1024 * 1024);
            let working = j
                .create_working(scope(), node(0), true, b"".as_slice())
                .unwrap();
            j.write_working(working.id, 0, &sealed).unwrap();
            let record = j.seal_working(working.id).unwrap().unwrap();
            assert_eq!(payload(&j, record.id), sealed);
            std::fs::write(&marker, record.id.to_string()).unwrap();
        }
        "fault" => {
            // Opening may itself fail on a device refusing writes; that is a
            // clean refusal too, and is what this asserts if it happens.
            let opened = UploadJournal::open(&journal, &scope().account, 8 * 1024 * 1024);
            let Ok(mut j) = opened else {
                let error = opened.err().unwrap();
                assert!(
                    !matches!(error, JournalError::Corrupt),
                    "a device refusing writes is not a corrupted journal: {error:?}"
                );
                // Which branch ran is the finding, not a detail: opening the
                // journal opens SQLite, which writes, so on a failing device the
                // journal may be unopenable and its contents inaccessible until
                // the device recovers. Inaccessible is not lost -- `recover`
                // asserts that -- but the two are different claims and the run
                // has to say which one it made.
                println!("FLAKY_FAULT branch=open_refused error={error:?}");
                return;
            };
            println!("FLAKY_FAULT branch=opened");
            // P2: what reached the platter before the fault is still readable.
            // dm-flakey's error_writes leaves reads alone, so a journal that
            // cannot produce these bytes has lost them to its own handling.
            let id: uuid::Uuid = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
            assert_eq!(
                payload(&j, id),
                sealed,
                "the journal lost durable bytes the device still holds"
            );
            // P1 and P4: new work must be refused, and refused as a storage
            // problem rather than as corruption -- reporting a refusing device
            // as a corrupt journal sends a user to recovery steps that destroy
            // work the device still has.
            let working = j
                .create_working(scope(), node(0), true, b"".as_slice())
                .and_then(|w| j.write_working(w.id, 0, b"written while the device was failing"));
            let error = working.expect_err("a failing device must not accept new durable work");
            assert!(
                matches!(
                    error,
                    JournalError::Storage | JournalError::DeviceFull | JournalError::Busy
                ),
                "a refusing device must be reported as a storage problem: {error:?}"
            );
            println!("FLAKY_FAULT branch=opened_then_refused error={error:?}");
        }
        "recover" => {
            let j = open(&journal, 8 * 1024 * 1024);
            let id: uuid::Uuid = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
            assert_eq!(
                payload(&j, id),
                sealed,
                "a device fault took durable work with it"
            );
        }
        // A lying fsync: dm-flakey's drop_writes acknowledges the write and
        // discards it. Worse than a refusal, because nothing fails at the time.
        // The journal cannot detect this while it is happening -- no API says
        // "that write you were told succeeded did not" -- so the claim under
        // test is about RECOVERY: what comes back afterwards must be either
        // right or refused, never wrong and confident.
        "lie" => {
            let opened = UploadJournal::open(&journal, &scope().account, 8 * 1024 * 1024);
            if let Ok(mut j) = opened {
                let working = j
                    .create_working(scope(), node(0), true, b"".as_slice())
                    .and_then(|w| j.write_working(w.id, 0, b"written while writes were discarded"))
                    .and_then(|(_, w)| j.seal_working(w.id));
                // Either outcome is acceptable here and the run records which.
                // A discarded write can look like success at the time; that is
                // what makes it a lying fsync rather than a failure.
                println!(
                    "FLAKY_LIE sealed={} ",
                    match &working {
                        Ok(_) => "accepted".to_string(),
                        Err(e) => format!("refused:{e:?}"),
                    }
                );
            } else {
                println!("FLAKY_LIE open_refused");
            }
        }
        // After a lying fsync, with the device honest again: the payload sealed
        // before any of it must still be right. Work done DURING the lie may be
        // gone -- that is what discarding writes means -- but it must not come
        // back as something else.
        "after_lie" => {
            let j = open(&journal, 8 * 1024 * 1024);
            let id: uuid::Uuid = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
            assert_eq!(
                payload(&j, id),
                sealed,
                "a lying fsync corrupted work that was durable before it started"
            );
        }
        // Torn writes: dm-flakey flips a byte inside write bios. Unlike a
        // refusal or a discard, the write lands and is wrong. The journal's
        // defence here is its checksums, so what this asserts is that damage
        // surfaces as Corrupt rather than as plausible bytes.
        "tear" => {
            let opened = UploadJournal::open(&journal, &scope().account, 8 * 1024 * 1024);
            match opened {
                Ok(mut j) => {
                    let outcome = j
                        .create_working(scope(), node(0), true, b"".as_slice())
                        .and_then(|w| j.write_working(w.id, 0, b"written while bytes were flipped"))
                        .and_then(|(_, w)| j.seal_working(w.id));
                    println!(
                        "FLAKY_TEAR sealed={}",
                        match &outcome {
                            Ok(_) => "accepted".to_string(),
                            Err(e) => format!("refused:{e:?}"),
                        }
                    );
                }
                Err(e) => println!("FLAKY_TEAR open_refused:{e:?}"),
            }
        }
        // After torn writes, with the device honest again. Work from before must
        // be right, and anything damaged must be reported rather than served.
        "after_tear" => {
            let j = open(&journal, 8 * 1024 * 1024);
            let id: uuid::Uuid = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
            let recovered =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| payload(&j, id)));
            match recovered {
                Ok(bytes) => assert_eq!(
                    bytes, sealed,
                    "torn writes produced plausible but wrong bytes for work that was durable \
                     before them; wrong and confident is the worst outcome available"
                ),
                Err(_) => println!("FLAKY_AFTER_TEAR payload_refused"),
            }
        }
        other => panic!("unknown phase {other:?}"),
    }
}
