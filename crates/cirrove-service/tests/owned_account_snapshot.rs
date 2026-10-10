#![cfg(feature = "icloud-write-probe")]
#![allow(clippy::unwrap_used)]
//! Actual CLI contract. No collector API is referenced, so baseline compiles.
//! The child is an entry point, not an independently meaningful acceptance arm.
use base64::Engine;
use cirrove_auth::CredentialVault;
use cirrove_core::{
    CancellationToken, Change, ChangePage, Checkpoint, Cursor, Node, NodeKind, Scope,
    reads::NativeArchiveBinding,
};
use cirrove_service::journal::UploadJournal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
const ARCHIVE_B: &str = "UEsDBBQAAAAAAPYWRV3WzmNDGwAAABsAAAAUAAAAT3duZWQucGFnZXMvRG9jdW1lbnRvd24gc3ludGhldGljIG5hdGl2ZSBzYXZlIEJQSwECFAMUAAAAAAD2FkVd1s5jQxsAAAAbAAAAFAAAAAAAAAAAAAAAgAEAAAAAT3duZWQucGFnZXMvRG9jdW1lbnRQSwUGAAAAAAEAAQBCAAAATQAAAAAA";
const ARCHIVE_C: &str = "UEsDBBQAAAAAAPYWRV1A/mQ0GwAAABsAAAAUAAAAT3duZWQucGFnZXMvRG9jdW1lbnRvd24gc3ludGhldGljIG5hdGl2ZSBzYXZlIENQSwECFAMUAAAAAAD2FkVdQP5kNBsAAAAbAAAAFAAAAAAAAAAAAAAAgAEAAAAAT3duZWQucGFnZXMvRG9jdW1lbnRQSwUGAAAAAAEAAQBCAAAATQAAAAAA";
const ARCHIVE: &str = "UEsDBBQAAAAAAMwVRV0FexhAHAAAABwAAAAUAAAAT3duZWQucGFnZXMvRG9jdW1lbnRvd24gc3ludGhldGljIG5hdGl2ZSBjb250ZW50UEsBAhQDFAAAAAAAzBVFXQV7GEAcAAAAHAAAABQAAAAAAAAAAAAAAIABAAAAAE93bmVkLnBhZ2VzL0RvY3VtZW50UEsFBgAAAAABAAEAQgAAAE4AAAAAAA==";
fn digest(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn write(path: &Path, b: &[u8]) {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    f.write_all(b).unwrap();
    f.sync_all().unwrap();
}
fn file_digest(path: &Path) -> String {
    use std::io::Read;
    let mut file = File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    hex::encode(hash.finalize())
}
fn dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn ticks(pid: u32) -> String {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    text.rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .into()
}
fn tree(path: &Path) -> BTreeMap<String, (bool, u64, String, u32)> {
    fn visit(root: &Path, p: &Path, out: &mut BTreeMap<String, (bool, u64, String, u32)>) {
        let mut rows = fs::read_dir(p)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        rows.sort();
        for entry in rows {
            let m = fs::symlink_metadata(&entry).unwrap();
            let key = entry
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if m.is_dir() {
                out.insert(
                    key,
                    (true, 0, String::new(), m.permissions().mode() & 0o777),
                );
                visit(root, &entry, out);
            } else {
                assert!(m.is_file());
                let b = fs::read(&entry).unwrap();
                out.insert(
                    key,
                    (
                        false,
                        b.len() as u64,
                        digest(&b),
                        m.permissions().mode() & 0o777,
                    ),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(path, path, &mut out);
    out
}
struct Keys(Mutex<Option<String>>);
#[async_trait::async_trait]
impl CredentialVault for Keys {
    async fn load(&self, _: &str) -> anyhow::Result<Option<secrecy::SecretString>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| secrecy::SecretString::from(s.clone())))
    }
    async fn save(&self, _: &str, v: secrecy::SecretString) -> anyhow::Result<()> {
        use secrecy::ExposeSecret;
        *self.0.lock().unwrap() = Some(v.expose_secret().to_owned());
        Ok(())
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}
#[test]
fn owned_account_snapshot_fixture_child() {
    let Some(root) = std::env::var_os("CIRROVE_OWNED_SNAPSHOT_FIXTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
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
    let state = root.join("state");
    let account = state.join("accounts").join(run.to_string());
    let scope = Scope {
        account: run.to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(ARCHIVE)
        .unwrap();
    write(&root.join("fixture.zip"), &bytes);
    let receipt = cirrove_icloud::PackageDownload {
        size: bytes.len() as u64,
        sha256: digest(&bytes),
    };
    let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
        &File::open(root.join("fixture.zip")).unwrap(),
        &receipt,
        "Owned.pages",
        2,
        &CancellationToken::new(),
    )
    .unwrap();
    let source = Node {
        id: "FILE::com.apple.CloudDocs::synthetic-original".into(),
        parent_id: Some("FOLDER::com.apple.CloudDocs::synthetic-parent".into()),
        name: "Owned.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 1,
        etag: Some("synthetic-v1".into()),
        content_version: None,
        target: None,
        package: true,
    };
    let archive = Node {
        id: format!("icloud-artifact:{}", source.id),
        parent_id: Some(source.id.clone()),
        name: source.name.clone(),
        kind: NodeKind::File,
        size: bytes.len() as u64,
        modified_unix: 0,
        etag: None,
        content_version: Some(format!(
            "icloud-artifact-v2:{}",
            json!({"source_etag":"synthetic-v1","source_parent":source.parent_id,"source_size":17,"sha256":receipt.sha256})
        )),
        target: None,
        package: false,
    };
    let mut journal =
        UploadJournal::open(&account.join("journal"), &run.to_string(), 1 << 20).unwrap();
    let mut hydration = journal
        .reserve_native_working(NativeArchiveBinding {
            scope: scope.clone(),
            archive,
            source: source.clone(),
            semantic,
        })
        .unwrap();
    hydration.write_chunk(&bytes).unwrap();
    let working = journal
        .publish_native_working(hydration.validate(&CancellationToken::new()).unwrap())
        .unwrap();
    let seal = |j: &mut UploadJournal| {
        let capture = j
            .capture_native_working(working.id)
            .unwrap()
            .capture(&CancellationToken::new())
            .unwrap();
        j.seal_captured_native_working(capture, &CancellationToken::new())
            .unwrap()
    };
    let bytes_b = base64::engine::general_purpose::STANDARD
        .decode(ARCHIVE_B)
        .unwrap();
    let bytes_c = base64::engine::general_purpose::STANDARD
        .decode(ARCHIVE_C)
        .unwrap();
    journal.truncate_working(working.id, 0).unwrap();
    journal.write_working(working.id, 0, &bytes_b).unwrap();
    let first = seal(&mut journal);
    journal.truncate_working(working.id, 0).unwrap();
    journal.write_working(working.id, 0, &bytes_c).unwrap();
    let second = seal(&mut journal);
    assert_eq!(second.base.as_ref().unwrap().predecessor, first.id);
    let dirty = b"incomplete native archive\0retained byte tail\xff";
    journal.truncate_working(working.id, 0).unwrap();
    journal.write_working(working.id, 0, dirty).unwrap();
    let generation = journal.working_file(working.id).unwrap().generation;
    File::open(account.join("journal/working").join(working.id.to_string()))
        .unwrap()
        .sync_all()
        .unwrap();
    let vault = cirrove_icloud::SealedUploadCheckpointVault::with_test_key_vault(
        &state,
        &run.to_string(),
        Arc::new(Keys(Mutex::new(None))),
    )
    .unwrap();
    // Real ciphertext production; plaintext is synthetic, not a native wire checkpoint.
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(vault.save(
            &format!("upload/{}", first.id),
            "opaque synthetic checkpoint fixture".into(),
        ))
        .unwrap();
    write(
        &account.join("icloud-session.sealed"),
        b"opaque session-path sentinel; no authentication claim",
    );
    let extra = account.join("native-trash-checkpoints");
    dir(&extra);
    let extra = extra.join(Uuid::new_v4().to_string());
    dir(&extra);
    write(
        &extra.join("checkpoint.sealed"),
        b"unreferenced opaque checkpoint bytes",
    );
    write(
        &account.join(".icloud-session-retained.tmp"),
        b"interrupted opaque temporary",
    );
    dir(&account.join("cache"));
    write(&account.join("cache/own-block"), b"owned cache bytes");
    write(
        &account.join("blocks.db"),
        b"opaque block-index sentinel; not SQLite",
    );
    dir(&account.join("icloud-artifacts"));
    dir(&account.join("icloud-artifacts/empty"));
    fs::set_permissions(
        account.join("icloud-artifacts/empty"),
        fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    let mut store = cirrove_store::Store::open(account.join("metadata.db")).unwrap();
    store.begin(&scope, false).unwrap();
    store
        .stage(
            &scope,
            None,
            &ChangePage {
                changes: vec![Change::Upsert(source.clone())],
                checkpoint: Checkpoint::Complete(Cursor("synthetic-completed".into())),
            },
        )
        .unwrap();
    store.begin(&scope, false).unwrap();
    let mut updated = source.clone();
    updated.id = "FILE::com.apple.CloudDocs::synthetic-staged".into();
    updated.name = "staged-owned.pages".into();
    store
        .stage(
            &scope,
            Some(&Cursor("synthetic-completed".into())),
            &ChangePage {
                changes: vec![Change::Delete { id: source.id }, Change::Upsert(updated)],
                checkpoint: Checkpoint::Continue(Cursor("synthetic-next".into())),
            },
        )
        .unwrap();
    write(&root.join("producer-ready.json"),serde_json::to_vec(&json!({"first":first.id,"second":second.id,"working":working.id,"generation":generation,"sealed_sha256":digest(&bytes_b),"dirty_sha256":digest(dirty)})).unwrap().as_slice());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !root.join("producer-release").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    // Deliberately preserve real DB WAL: no SQLite Drop/checkpoint. OS releases owners.
    std::process::exit(0);
}
struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take()
            && matches!(child.try_wait(), Ok(None))
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
struct Fixture {
    root: PathBuf,
    run: Uuid,
    registration: PathBuf,
    source: PathBuf,
    _child: OwnedChild,
}
// No TempDir or recursive cleanup: fixture evidence remains for root supervision.
impl Fixture {
    fn new(hold: bool) -> Self {
        let run = Uuid::new_v4();
        let root = PathBuf::from(format!("/var/tmp/cirrove-owned-snapshot-{run}"));
        dir(&root);
        dir(&root.join("state"));
        dir(&root.join("state/accounts"));
        let source = root.join("state/accounts").join(run.to_string());
        dir(&source);
        dir(&source.join("journal"));
        dir(&root.join("mount"));
        for lock in [
            root.join("state/daemon.lock"),
            root.join("state/settings.lock"),
            root.join("state").join(format!("operation-{run}.lock")),
            source.join("owner.lock"),
            source.join("journal/owner.lock"),
        ] {
            write(&lock, b"");
        }
        write(
            &root.join("state/accounts.json"),
            serde_json::to_vec(
                &json!({"version":2,"accounts":[{"id":run,"provider":"synthetic-owned-fixture"}]}),
            )
            .unwrap()
            .as_slice(),
        );
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "owned_account_snapshot_fixture_child",
                "--nocapture",
            ])
            .env("CIRROVE_OWNED_SNAPSHOT_FIXTURE_ROOT", &root);
        let mut child = OwnedChild(Some(command.spawn().unwrap()));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !root.join("producer-ready.json").exists() {
            assert!(Instant::now() < deadline);
            assert!(child.0.as_mut().unwrap().try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = child.0.as_mut().unwrap().id();
        let start_ticks = ticks(pid);
        let exe = fs::read_link(format!("/proc/{pid}/exe")).unwrap();
        let exe_sha256 = file_digest(&exe);
        let argv_sha256 = digest(&fs::read(format!("/proc/{pid}/cmdline")).unwrap());
        if !hold {
            write(&root.join("producer-release"), b"");
            assert!(child.0.as_mut().unwrap().wait().unwrap().success());
        }
        let proof = json!({"version":1,"run":run,"mount_namespace":fs::read_link("/proc/self/ns/mnt").unwrap().to_str().unwrap(),"producers":[{"pid":pid,"start_ticks":start_ticks,"exe_sha256":exe_sha256,"argv_sha256":argv_sha256,"exit_code":0}]});
        write(
            &root.join("quiescence.json"),
            &serde_json::to_vec(&proof).unwrap(),
        );
        let reg = json!({"version":1,"run":run,"account":run,"root":root,"accounts_sha256":digest(&fs::read(root.join("state/accounts.json")).unwrap()),"quiescence_sha256":digest(&fs::read(root.join("quiescence.json")).unwrap()),"limits":{"max_depth":16,"max_entries":256,"max_file_bytes":67108864,"max_total_bytes":268435456},"parent_manifest_sha256":null,"deadline_unix":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()+900});
        let registration = root.join("registration.json");
        write(&registration, &serde_json::to_vec(&reg).unwrap());
        Self {
            root,
            run,
            registration,
            source,
            _child: child,
        }
    }
    fn invoke(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cirrove"))
            .args(["owned-account-snapshot", "--registration"])
            .arg(&self.registration)
            .arg("--sha256")
            .arg(digest(&fs::read(&self.registration).unwrap()))
            .output()
            .unwrap()
    }
    fn manifest(&self) -> PathBuf {
        self.root.join("snapshot/snapshot-manifest.json")
    }
    fn assert_refused(&self) {
        let before = tree(&self.source);
        let result = self.invoke();
        assert!(!result.status.success());
        assert!(!self.manifest().exists());
        assert_eq!(tree(&self.source), before);
    }
}
#[test]
fn owned_account_snapshot_actual_cli_preserves_full_native_opaque_tree() {
    let f = Fixture::new(false);
    let before = tree(&f.source);
    let result = f.invoke();
    assert_eq!(tree(&f.source), before); // source invariance checked even at original RED
    if !result.status.success() {
        assert_eq!(
            result.status.code(),
            Some(2),
            "unexpected pre-implementation endpoint"
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains("unrecognized subcommand"));
        assert!(!f.manifest().exists());
    }
    assert!(
        result.status.success(),
        "missing actual owned-account-snapshot CLI workflow"
    );
    let manifest: Value = serde_json::from_slice(&fs::read(f.manifest()).unwrap()).unwrap();
    assert_eq!(manifest["state"], "byte_snapshot_complete");
    assert_eq!(manifest["restorability_verified"], false);
    assert_eq!(manifest["account"], f.run.to_string());
    let copied = f.root.join("snapshot/account").join(f.run.to_string());
    assert_eq!(
        tree(&copied),
        before,
        "full opaque source tree must be copied independently of collector manifest"
    );
    // Do not open immutable copied SQLite; logical/reader validation is a separate arm.
    assert!(before.keys().any(|s| s.starts_with("upload-checkpoints/")));
    assert!(before.contains_key("metadata.db-wal"));
    assert!(before.contains_key("journal/uploads.db-wal"));
    assert_eq!(
        fs::read(f.root.join("snapshot/context/accounts.json")).unwrap(),
        fs::read(f.root.join("state/accounts.json")).unwrap()
    );
}
#[test]
fn owned_account_snapshot_existing_root_and_account_locks_refuse_busy_or_missing() {
    for index in [0, 1, 2, 4] {
        let f = Fixture::new(false);
        let paths = [
            f.root.join("state/daemon.lock"),
            f.root.join("state/settings.lock"),
            f.root
                .join("state")
                .join(format!("operation-{}.lock", f.run)),
            f.source.join("owner.lock"),
            f.source.join("journal/owner.lock"),
        ];
        let lock = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&paths[index])
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        f.assert_refused();
        fs2::FileExt::unlock(&lock).unwrap();
        // Preserve rather than delete the owned lock, and prove collector never recreates it.
        fs::rename(&paths[index], f.root.join("retained-removed-lock")).unwrap();
        f.assert_refused();
        assert!(!paths[index].exists());
    }
}
#[test]
fn owned_account_snapshot_busy_account_owner_refuses() {
    let f = Fixture::new(false);
    let owner = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(f.source.join("owner.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&owner).unwrap();
    let before = tree(&f.source);
    let result = f.invoke();
    assert_eq!(tree(&f.source), before);
    assert!(
        !result.status.success(),
        "busy account owner must refuse before snapshot publication"
    );
    assert!(!f.manifest().exists());
    fs2::FileExt::unlock(&owner).unwrap();
}
#[test]
fn owned_account_snapshot_quiescence_and_registered_context_refuse() {
    let mut f = Fixture::new(true);
    assert!(f._child.0.as_mut().unwrap().try_wait().unwrap().is_none());
    f.assert_refused(); // actual registered producer still alive
    assert!(f._child.0.as_mut().unwrap().try_wait().unwrap().is_none());
    // End this case before creating other fixtures: a shadowed binding keeps
    // its producer alive until the function ends, past its own deadline on CI.
    write(&f.root.join("producer-release"), b"");
    assert!(f._child.0.as_mut().unwrap().wait().unwrap().success());
    drop(f);
    let f = Fixture::new(false);
    let path = f.root.join("state/accounts.json");
    fs::write(&path, b"changed synthetic context").unwrap();
    f.assert_refused();
    let f = Fixture::new(false);
    let mut reg: Value = serde_json::from_slice(&fs::read(&f.registration).unwrap()).unwrap();
    reg["account"] = json!(Uuid::new_v4());
    fs::write(&f.registration, serde_json::to_vec(&reg).unwrap()).unwrap();
    f.assert_refused();
}
#[test]
fn owned_account_snapshot_unsafe_tree_and_bounds_refuse_complete_manifest() {
    for arm in 0..4 {
        let f = Fixture::new(false);
        match arm {
            0 => std::os::unix::fs::symlink(
                f.root.join("fixture.zip"),
                f.source.join("foreign-link"),
            )
            .unwrap(),
            1 => fs::hard_link(
                f.source.join("icloud-session.sealed"),
                f.root.join("hardlink-alias"),
            )
            .unwrap(),
            2 => dir(&f.root.join("snapshot")),
            _ => {
                let mut reg: Value =
                    serde_json::from_slice(&fs::read(&f.registration).unwrap()).unwrap();
                reg["limits"]["max_total_bytes"] = json!(1);
                fs::write(&f.registration, serde_json::to_vec(&reg).unwrap()).unwrap();
            }
        }
        // Symlink case intentionally uses a different source inventory helper.
        let result = f.invoke();
        assert!(!result.status.success());
        assert!(!f.manifest().exists());
    }
}

#[test]
fn owned_account_snapshot_data_and_control_permissions_refuse_unsafe_modes() {
    // Valid SQLite data modes come from Store::open unchanged in the positive
    // fixture. These intentionally unsafe synthetic mutations must refuse.
    for mode in [0o664, 0o606, 0o601, 0o610, 0o4600] {
        let f = Fixture::new(false);
        fs::set_permissions(
            f.source.join("metadata.db"),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        f.assert_refused();
    }
    // Readable opaque data must not relax private control admission.
    for relative in [
        "registration.json".to_string(),
        "quiescence.json".to_string(),
        "state/accounts.json".to_string(),
        "state/daemon.lock".to_string(),
        "state/settings.lock".to_string(),
    ] {
        let f = Fixture::new(false);
        fs::set_permissions(f.root.join(relative), fs::Permissions::from_mode(0o644)).unwrap();
        f.assert_refused();
    }
    for account_lock in ["owner.lock", "journal/owner.lock"] {
        let f = Fixture::new(false);
        fs::set_permissions(
            f.source.join(account_lock),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        f.assert_refused();
    }
    let f = Fixture::new(false);
    fs::set_permissions(
        f.root
            .join("state")
            .join(format!("operation-{}.lock", f.run)),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    f.assert_refused();
}
