//! Current-format native checkpoint restoration from a stopped synthetic account.
//! Original, stopped collector source and immutable image are never consumer DBs.
use super::*;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const EXACT: &str = "journal::package_replacement::worker_transport::native_final_tests::derived_snapshot_tests::native_snapshot_real_tls_checkpoint_restores_on_distinct_copy_without_replay";
const CHILD_ROOT: &str = "CIRROVE_SYNTHETIC_NATIVE_SNAPSHOT_ROOT";
type Tree = BTreeMap<String, (bool, u64, String, u32)>;

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn file_digest(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = std::io::Read::read(&mut file, &mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    hex::encode(hasher.finalize())
}
fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn durable(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
fn bytes_json(path: &Path, value: &serde_json::Value) {
    durable(path, &serde_json::to_vec(value).unwrap());
}
fn tree(root: &Path) -> Tree {
    fn visit(root: &Path, path: &Path, depth: usize, out: &mut Tree, total: &mut u64) {
        assert!(depth <= 16);
        let meta = fs::symlink_metadata(path).unwrap();
        assert!(!meta.file_type().is_symlink());
        assert_eq!(meta.uid(), fs::metadata("/proc/self").unwrap().uid());
        let name = path
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(out.len() < 256);
        if meta.is_dir() {
            out.insert(
                name,
                (true, 0, String::new(), meta.permissions().mode() & 0o777),
            );
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, depth + 1, out, total);
            }
        } else {
            assert!(meta.is_file() && meta.nlink() == 1 && meta.len() <= 64 * 1024 * 1024);
            *total += meta.len();
            assert!(*total <= 128 * 1024 * 1024);
            let bytes = fs::read(path).unwrap();
            assert_eq!(bytes.len() as u64, meta.len());
            out.insert(
                name,
                (
                    false,
                    meta.len(),
                    digest(&bytes),
                    meta.permissions().mode() & 0o777,
                ),
            );
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, 0, &mut result, &mut 0);
    result
}
fn copy_tree(source: &Path, destination: &Path) {
    let before = tree(source);
    fn copy(source: &Path, destination: &Path) {
        let meta = fs::symlink_metadata(source).unwrap();
        assert!(!meta.file_type().is_symlink());
        if meta.is_dir() {
            private_dir(destination);
            for entry in fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                copy(&entry.path(), &destination.join(entry.file_name()));
            }
            fs::set_permissions(
                destination,
                fs::Permissions::from_mode(meta.permissions().mode() & 0o777),
            )
            .unwrap();
            File::open(destination).unwrap().sync_all().unwrap();
        } else {
            assert!(meta.is_file() && meta.nlink() == 1);
            let bytes = fs::read(source).unwrap();
            durable(destination, &bytes);
            fs::set_permissions(
                destination,
                fs::Permissions::from_mode(meta.permissions().mode() & 0o777),
            )
            .unwrap();
            File::open(destination).unwrap().sync_all().unwrap();
        }
    }
    copy(source, destination);
    assert_eq!(tree(source), before);
    assert_eq!(tree(destination), before);
}
fn synthetic_keys(operation: Uuid, value: u8) -> Arc<WrappingKeys> {
    // Known synthetic fixture key. Never desktop credentials; never output it.
    let keys = Arc::new(WrappingKeys::default());
    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode([value; 32]);
    keys.0.lock().unwrap().insert(
        format!("upload/{operation}"),
        SecretString::from(format!("icloud-seal-v1:{encoded}")),
    );
    keys
}
struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            if child.try_wait().unwrap().is_none() {
                child.kill().unwrap();
            }
            child.wait().unwrap();
        }
    }
}
async fn producer(root: PathBuf) {
    let run = Uuid::parse_str(
        root.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("cirrove-owned-snapshot-")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        root,
        PathBuf::from(format!("/var/tmp/cirrove-owned-snapshot-{run}"))
    );
    let fixture = Fixture::new_seeded(true).await;
    let marker = fixture.withheld().await;
    assert_eq!(counts(&fixture.server), (1, 1, 1, 1, 1));
    let wrapping = fixture
        .keys
        .load(&format!("upload/{}", fixture.row.id))
        .await
        .unwrap()
        .unwrap();
    assert!(wrapping.expose_secret().starts_with("icloud-seal-v1:"));
    let key_file = root.join("synthetic-wrapping-key.json");
    bytes_json(
        &key_file,
        &json!({"version":1,"account":fixture.account.id,"operation":fixture.row.id,"purpose":format!("upload/{}",fixture.row.id),"sealed_key":wrapping.expose_secret()}),
    );
    fs::set_permissions(&key_file, fs::Permissions::from_mode(0o400)).unwrap();
    File::open(&key_file).unwrap().sync_all().unwrap();
    let wrapping_key_sha = digest(&fs::read(&key_file).unwrap());
    // Key remains a separate explicitly synthetic prerequisite, not part of the snapshot.
    // The actual producer performs HTTPS staging+handoff and owns the source.
    // Close SQLite and all writer handles before the coherent source byte copy.
    let Fixture {
        state,
        directory,
        _staging,
        engine,
        context,
        account,
        row,
        server,
        keys,
        completed,
        folder: _,
    } = fixture;
    drop(context);
    drop(engine);
    drop(server);
    drop(keys);
    let original = state.path().join("accounts").join(&account.id);
    let original_before = tree(&original);
    let target_state = root.join("state");
    private_dir(&target_state);
    private_dir(&target_state.join("accounts"));
    let source = target_state.join("accounts").join(&account.id);
    copy_tree(&original, &source);
    assert_eq!(tree(&original), original_before);
    for p in [
        target_state.join("daemon.lock"),
        target_state.join("settings.lock"),
        target_state.join(format!("operation-{}.lock", account.id)),
        source.join("owner.lock"),
    ] {
        if !p.exists() {
            durable(&p, b"");
        }
    }
    bytes_json(
        &target_state.join("accounts.json"),
        &json!({"version":2,"accounts":[account]}),
    );
    bytes_json(
        &root.join("native-loss-marker.json"),
        &serde_json::to_value(&marker).unwrap(),
    );
    let checkpoint = source
        .join("upload-checkpoints")
        .join(row.id.to_string())
        .join("checkpoint.sealed");
    let checkpoint_sha = digest(&fs::read(checkpoint).unwrap());
    let original_path = state.keep();
    let retained_marker_directory = directory.keep();
    let retained_staging = _staging.keep();
    let ready = json!({"version":1,"run":run,"account":account,"operation":row.id,"request":request(&row),"completed":completed,"original_source":original_path,"retained_marker_directory":retained_marker_directory,"retained_staging":retained_staging,"checkpoint_sha256":checkpoint_sha,"wrapping_key_file_sha256":wrapping_key_sha,"native_phase":"handoff-install-armed","actual_tls_mutations":[1,1,1,1,1]});
    bytes_json(&root.join("producer-ready.tmp"), &ready);
    fs::rename(
        root.join("producer-ready.tmp"),
        root.join("producer-ready.json"),
    )
    .unwrap();
    File::open(&root).unwrap().sync_all().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !root.join("producer-release").exists() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // No writer remains; OS exit preserves the actual producer identity.
    std::process::exit(0);
}
fn derived_state(root: &Path, name: &str, account: &str, image: &Path) -> PathBuf {
    let state = root.join(name);
    private_dir(&state);
    private_dir(&state.join("accounts"));
    copy_tree(image, &state.join("accounts").join(account));
    state
}

#[tokio::test]
async fn native_snapshot_real_tls_checkpoint_restores_on_distinct_copy_without_replay() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        producer(PathBuf::from(root)).await;
        return;
    }
    // Retain all evidence. No TempDir cleanup, moves or source DB opens.
    let run = Uuid::new_v4();
    let root = PathBuf::from(format!("/var/tmp/cirrove-owned-snapshot-{run}"));
    private_dir(&root);
    println!("native_snapshot_fixture_root={}", root.display());
    let child_log_path = root.join("producer.log");
    let child_log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&child_log_path)
        .unwrap();
    let mut child = OwnedChild(Some(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", EXACT, "--nocapture"])
            .env(CHILD_ROOT, &root)
            .stdout(Stdio::from(child_log.try_clone().unwrap()))
            .stderr(Stdio::from(child_log))
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(60);
    while !root.join("producer-ready.json").exists() {
        assert!(Instant::now() < deadline);
        assert!(child.0.as_mut().unwrap().try_wait().unwrap().is_none());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid = child.0.as_ref().unwrap().id();
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let ticks = stat
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .to_owned();
    let exe_sha = file_digest(Path::new(&format!("/proc/{pid}/exe")));
    let argv_sha = digest(&fs::read(format!("/proc/{pid}/cmdline")).unwrap());
    let ready: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("producer-ready.json")).unwrap()).unwrap();
    assert_eq!(ready["run"], run.to_string());
    assert_eq!(ready["actual_tls_mutations"], json!([1, 1, 1, 1, 1]));
    durable(&root.join("producer-release"), b"");
    let status = loop {
        if let Some(s) = child.0.as_mut().unwrap().try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(status.code(), Some(0));
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    let account: Account = serde_json::from_value(ready["account"].clone()).unwrap();
    let operation = Uuid::parse_str(ready["operation"].as_str().unwrap()).unwrap();
    let expected_request: cirrove_core::upload::UploadRequest =
        serde_json::from_value(ready["request"].clone()).unwrap();
    let marker: LossMarker =
        serde_json::from_slice(&fs::read(root.join("native-loss-marker.json")).unwrap()).unwrap();
    assert_eq!(marker.operation, operation);
    assert!(marker.request == expected_request);
    let source = root.join("state/accounts").join(&account.id);
    let source_before = tree(&source);
    let original = PathBuf::from(ready["original_source"].as_str().unwrap())
        .join("accounts")
        .join(&account.id);
    let original_before = tree(&original);
    assert_eq!(original_before, source_before);
    let quiescence = json!({"version":1,"run":run,"mount_namespace":fs::read_link("/proc/self/ns/mnt").unwrap().to_str().unwrap(),"producers":[{"pid":pid,"start_ticks":ticks,"exe_sha256":exe_sha,"argv_sha256":argv_sha,"exit_code":0}]});
    bytes_json(&root.join("quiescence.json"), &quiescence);
    let reg = json!({"version":1,"run":run,"account":account.id,"root":root,"accounts_sha256":digest(&fs::read(root.join("state/accounts.json")).unwrap()),"quiescence_sha256":digest(&fs::read(root.join("quiescence.json")).unwrap()),"limits":{"max_depth":16,"max_entries":256,"max_file_bytes":67108864,"max_total_bytes":134217728},"parent_manifest_sha256":null,"deadline_unix":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()+120});
    let registration = root.join("registration.json");
    bytes_json(&registration, &reg);
    crate::journal::owned_account_snapshot(
        &registration,
        &digest(&fs::read(&registration).unwrap()),
        &CancellationToken::new(),
    )
    .unwrap();
    let image = root.join("snapshot/account").join(&account.id);
    let image_before = tree(&image);
    assert_eq!(image_before, source_before);
    let derived = derived_state(&root, "derived-consumer-state", &account.id, &image);
    let wrongkey = derived_state(&root, "derived-wrong-key", &account.id, &image);
    let tampered = derived_state(&root, "derived-tampered", &account.id, &image);
    let key = format!("upload/{operation}");
    let key_file = root.join("synthetic-wrapping-key.json");
    let key_bytes = fs::read(&key_file).unwrap();
    assert_eq!(
        digest(&key_bytes),
        ready["wrapping_key_file_sha256"].as_str().unwrap()
    );
    assert_eq!(
        fs::symlink_metadata(&key_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o400
    );
    let key_record: serde_json::Value = serde_json::from_slice(&key_bytes).unwrap();
    assert_eq!(key_record.as_object().unwrap().len(), 5);
    assert_eq!(key_record["version"], 1);
    assert_eq!(key_record["account"], account.id);
    assert_eq!(key_record["operation"], operation.to_string());
    assert_eq!(key_record["purpose"], key);
    let actual_keys = Arc::new(WrappingKeys::default());
    let actual_value = key_record["sealed_key"].as_str().unwrap();
    assert!(actual_value.starts_with("icloud-seal-v1:") && actual_value.len() < 256);
    actual_keys
        .0
        .lock()
        .unwrap()
        .insert(key.clone(), SecretString::from(actual_value.to_owned()));
    let badvault = SealedUploadCheckpointVault::with_test_key_vault(
        &wrongkey,
        &account.id,
        synthetic_keys(operation, 8),
    )
    .unwrap();
    assert!(badvault.load(&key).await.is_err());
    let tamper_path = tampered
        .join("accounts")
        .join(&account.id)
        .join("upload-checkpoints")
        .join(operation.to_string())
        .join("checkpoint.sealed");
    let mut broken = fs::read(&tamper_path).unwrap();
    let last = broken.len() - 1;
    broken[last] ^= 1;
    fs::write(&tamper_path, &broken).unwrap();
    let tampervault = SealedUploadCheckpointVault::with_test_key_vault(
        &tampered,
        &account.id,
        actual_keys.clone(),
    )
    .unwrap();
    assert!(tampervault.load(&key).await.is_err());
    let vault = Arc::new(
        SealedUploadCheckpointVault::with_test_key_vault(
            &derived,
            &account.id,
            actual_keys.clone(),
        )
        .unwrap(),
    );
    // Preservation is asserted before this desired-success endpoint too.
    assert_eq!(tree(&original), original_before);
    assert_eq!(tree(&source), source_before);
    assert_eq!(tree(&image), image_before);
    let saved = vault.load(&key).await.unwrap().unwrap();
    let checkpoint = derived
        .join("accounts")
        .join(&account.id)
        .join("upload-checkpoints")
        .join(operation.to_string())
        .join("checkpoint.sealed");
    assert_eq!(
        digest(&fs::read(checkpoint).unwrap()),
        ready["checkpoint_sha256"].as_str().unwrap()
    );
    for (relative, version) in [
        ("journal/uploads.db", crate::journal::JOURNAL_SCHEMA),
        ("metadata.db", 8u32),
    ] {
        let db = rusqlite::Connection::open_with_flags(
            derived.join("accounts").join(&account.id).join(relative),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.execute_batch("PRAGMA query_only=ON").unwrap();
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            version
        );
    }
    let sealed = crate::journal::RecoveryJournal::open(
        &derived.join("accounts").join(&account.id).join("journal"),
        &account.id,
    )
    .unwrap();
    let pending = sealed.native_validation_upload(operation).unwrap();
    assert_eq!(pending.state, UploadState::VerifyRequired);
    assert!(pending.attempt.is_none());
    assert_eq!(pending.session_key, Some(operation));
    assert!(request(&pending) == expected_request);
    assert!(pending.remote.is_none() && pending.package_completion.is_none());
    assert!(pending.working_file.is_none());
    let exported = root.join("derived-sealed-b.zip");
    let receipt = sealed
        .local_export_source(operation)
        .unwrap()
        .copy_to(&exported, &CancellationToken::new(), |_| {})
        .unwrap();
    assert_eq!(receipt.size, expected_request.size);
    assert_eq!(receipt.sha256, expected_request.sha256);
    assert_eq!(
        fs::read(exported).unwrap(),
        archive("Source.numbers", false, false)
    );
    drop(sealed);
    // Recreate only the synthetic remote service's actually observed final state.
    // The actual producer proved these flags/counters; no receipt is fabricated.
    let server = Server::start(
        Plan {
            target_name: marker.receipt.current.name.clone(),
            staged_name: format!("staged-by-cirrove-{operation}.numbers"),
        },
        archive("Source.numbers", false, false),
        false,
        false,
        false,
        None,
    )
    .await;
    {
        let mut s = server.state.lock().unwrap();
        s.registered = true;
        s.trashed = true;
        s.installed = true;
    }
    let metadata = Arc::new(Metadata {
        original: marker.receipt.original.clone(),
        state: Mutex::new(Some(server.state.clone())),
    });
    let engine = Engine::new(account.clone(), metadata, derived.clone())
        .await
        .unwrap();
    let context = WriteContext::open(&engine, &derived).await.unwrap();
    let router = crate::icloud_writes::ICloudWriteProvider::new(&account, &context)
        .unwrap()
        .synthetic_native_transport(server.client.clone());
    let adapter = router
        .native_package_adapter(&operation.to_string(), &expected_request, Some(&saved))
        .await
        .unwrap();
    assert_eq!(
        adapter
            .native_checkpoint_diagnostic(&operation.to_string(), &expected_request, &saved)
            .unwrap()["phase"],
        "handoff-install-armed"
    );
    let before = counts(&server);
    let requests_before = server.state.lock().unwrap().requests;
    assert!(
        adapter
            .native_checkpoint_diagnostic(&Uuid::new_v4().to_string(), &expected_request, &saved)
            .is_err()
    );
    assert!(
        router
            .native_package_adapter(&Uuid::new_v4().to_string(), &expected_request, Some(&saved))
            .await
            .is_err()
    );
    assert_eq!(server.state.lock().unwrap().requests, requests_before);
    let completed: Vec<UploadRecord> = serde_json::from_value(ready["completed"].clone()).unwrap();
    let guard = Arc::new(
        Guard::new(
            router,
            expected_request.clone(),
            context.journal(),
            completed,
            root.clone(),
            marker.run,
            Mode::Recover {
                operation,
                checkpoint_sha256: marker.checkpoint_sha256.clone(),
                receipt: Box::new(marker.receipt.clone()),
            },
        )
        .unwrap(),
    );
    let worker = TransferWorker::new(
        context.journal(),
        guard.clone(),
        vault.clone(),
        CancellationToken::new(),
    );
    let result = tokio::time::timeout(Duration::from_secs(30), worker.run_once())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.state, UploadState::Uploaded);
    assert_eq!(result.id, operation);
    assert_eq!(counts(&server), before);
    assert!(server.state.lock().unwrap().requests > requests_before);
    let inspection_count = guard.counts.inspections.load(Ordering::SeqCst);
    assert_eq!(inspection_count, 1);
    assert_eq!(guard.counts.refused_uploads.load(Ordering::SeqCst), 0);
    assert_eq!(guard.counts.refused_namespace.load(Ordering::SeqCst), 0);
    let recovered = context.journal().lock().unwrap().get(operation).unwrap();
    assert_eq!(recovered.state, UploadState::Uploaded);
    let receipt = recovered.native_replacement_receipt().unwrap();
    assert_eq!(receipt.0, &marker.receipt.original);
    assert_eq!(receipt.1, &marker.receipt.current);
    assert_eq!(receipt.2, &marker.receipt.backup);
    assert_eq!(
        recovered.package_completion,
        Some(marker.receipt.current_semantic)
    );
    assert!(vault.load(&key).await.unwrap().is_none());
    assert_eq!(recovered.session_key, Some(operation));
    let filesystem =
        crate::filesystem::CloudFs::new_experimental_writable(engine.clone(), context.journal())
            .await
            .unwrap();
    let control = filesystem.write_control().unwrap();
    assert!(control.publish_completed_package().await.unwrap());
    assert!(
        matches!(context.journal().lock().unwrap().package_publication_status(operation).unwrap(),crate::journal::PackagePublicationStatus::Present(node) if node==marker.receipt.current)
    );
    assert_eq!(counts(&server), before);
    drop(control);
    drop(filesystem);
    drop(worker);
    drop(guard);
    drop(context);
    drop(engine);
    assert_eq!(tree(&original), original_before);
    assert_eq!(tree(&source), source_before);
    assert_eq!(tree(&image), image_before);
    assert_eq!(fs::read(&key_file).unwrap(), key_bytes);
    bytes_json(
        &root.join("native-derived-restoration-result.json"),
        &json!({
            "version": 1, "run": run, "status": "passed",
            "journal_format": crate::journal::JOURNAL_SCHEMA, "metadata_format": 8,
            "producer_exit_code": status.code(), "producer_pid": pid,
            "exact_producer_gone_before_collection": true,
            "source_image_and_original_bytes_modes_preserved": true,
            "separate_synthetic_key_file_unchanged": true,
            "derived_native_checkpoint_restored": true,
            "inspection_requests": inspection_count,
            "consumer_mutation_counts_before": before,
            "consumer_mutation_counts_after": counts(&server),
            "normal_metadata_publication_verified": true,
            "installed_actions": false, "cloud_requests": 0,
            "full_gate488_closed": false
        }),
    );
}
