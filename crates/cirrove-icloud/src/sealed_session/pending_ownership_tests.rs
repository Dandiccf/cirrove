#![allow(clippy::unwrap_used)]
use super::*;
use std::{cell::RefCell, sync::mpsc, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Created,
    Published,
}
type Hook = Box<dyn FnMut(Step) -> Result<()>>;
thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}
pub(super) fn at(step: Step) -> Result<()> {
    HOOK.with(|hook| match hook.borrow_mut().as_mut() {
        Some(hook) => hook(step),
        None => Ok(()),
    })
}
struct ResetHook;
impl Drop for ResetHook {
    fn drop(&mut self) {
        HOOK.with(|h| *h.borrow_mut() = None);
    }
}
fn hook(value: impl FnMut(Step) -> Result<()> + 'static) -> ResetHook {
    HOOK.with(|h| *h.borrow_mut() = Some(Box::new(value)));
    ResetHook
}
fn pending(store: &SealedSessionVault) -> PathBuf {
    store
        .account_dir
        .join(format!("{}.pending", store.temp_prefix))
}
fn fixture(root: &Path, account: &str) -> SealedSessionVault {
    let store = SealedSessionVault::new(root, account).unwrap();
    store
        .write_sealed("synthetic", &[7; 32], &SecretString::from("original"))
        .unwrap();
    store
}
fn only_final(store: &SealedSessionVault) {
    let names: Vec<_> = fs::read_dir(&store.account_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from(store.file_name)]);
}

#[test]
fn concurrent_pending_writer_cannot_remove_owner_slot() {
    let root = tempfile::tempdir().unwrap();
    let account = Uuid::new_v4().to_string();
    let store = fixture(root.path(), &account);
    let (created_tx, created_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let owner = scope.spawn(|| {
            let _hook = hook(move |step| {
                if step == Step::Created {
                    created_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                }
                Ok(())
            });
            store.write_existing("synthetic", &[7; 32], &SecretString::from("owner"))
        });
        created_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let before = fs::symlink_metadata(pending(&store)).unwrap();
        let refused =
            store.write_existing("synthetic", &[7; 32], &SecretString::from("competitor"));
        let after = fs::symlink_metadata(pending(&store));
        release_tx.send(()).unwrap();
        let completed = owner.join().unwrap();
        assert!(refused.is_err());
        let after = after.expect("refused competing writer must retain original owner's slot");
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert_eq!(after.mode() & 0o777, 0o600);
        completed.unwrap();
    });
    assert!(
        store
            .read_sealed("synthetic", &[7; 32])
            .unwrap()
            .expose_secret()
            == "owner"
    );
    only_final(&store);
}

#[test]
fn published_writer_error_keeps_next_writer_pending_slot() {
    let root = tempfile::tempdir().unwrap();
    let account = Uuid::new_v4().to_string();
    let store = fixture(root.path(), &account);
    let path = pending(&store);
    let next = path.clone();
    let _hook = hook(move |step| {
        if step == Step::Published {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&next)?;
            file.write_all(b"later writer's unpublished bytes")?;
            file.sync_all()?;
            bail!("synthetic failure after publication");
        }
        Ok(())
    });
    assert!(
        store
            .write_existing("synthetic", &[7; 32], &SecretString::from("published"))
            .is_err()
    );
    assert!(
        store
            .read_sealed("synthetic", &[7; 32])
            .unwrap()
            .expose_secret()
            == "published"
    );
    assert!(
        fs::read(&path).expect("post-rename failure must not remove next writer's slot")
            == b"later writer's unpublished bytes"
    );
}

#[test]
fn unpublished_owned_failure_removes_only_own_pending_slot() {
    let root = tempfile::tempdir().unwrap();
    let account = Uuid::new_v4().to_string();
    let store = fixture(root.path(), &account);
    let before = fs::read(store.path()).unwrap();
    let _hook = hook(|step| {
        if step == Step::Created {
            bail!("synthetic pre-publication failure");
        }
        Ok(())
    });
    assert!(
        store
            .write_existing("synthetic", &[7; 32], &SecretString::from("refused"))
            .is_err()
    );
    assert!(fs::read(store.path()).unwrap() == before);
    only_final(&store);
}

#[test]
fn legacy_random_remnants_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let account = Uuid::new_v4().to_string();
    let store = fixture(root.path(), &account);
    let legacy = store
        .account_dir
        .join(format!("{}-{}.tmp", store.temp_prefix, Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&legacy)
        .unwrap();
    file.write_all(b"historical unpublished bytes").unwrap();
    let before = file.metadata().unwrap();
    store
        .write_existing("synthetic", &[7; 32], &SecretString::from("current"))
        .unwrap();
    assert!(fs::read(&legacy).unwrap() == b"historical unpublished bytes");
    let after = fs::symlink_metadata(legacy).unwrap();
    assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    assert!(!pending(&store).exists());
    assert!(
        store
            .read_sealed("synthetic", &[7; 32])
            .unwrap()
            .expose_secret()
            == "current"
    );
}
