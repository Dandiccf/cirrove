//! Capacity of a writable mount.
//!
//! Every other fixture in this module mounts read-only: `account` asks for
//! `AccessMode::ReadOnly` and each mount calls `CloudFs::new`. That leaves the
//! writable path with no capacity, memory or retirement coverage at all, and it
//! is not the same path. A writable mount keys its inodes differently --
//! `Inner::inode_key` drops the content revision and size, so revisions of one
//! file collapse onto a single inode rather than each getting its own -- and it
//! puts a `Writeback` overlay in front of every listing. Namespace retention
//! measured read-only does not transfer to it.
//!
//! Deliberately smaller than the churn fixtures. This exists to show that the
//! writable regime retires and stays bounded, not to re-measure the numbers those
//! fixtures already produce; a half-hour run would buy nothing extra here.
use super::*;
use crate::journal::UploadJournal;
use std::sync::Mutex;

/// The same synthetic account, writable and disabled.
///
/// `Writeback::new` refuses anything else: experimental writes require an account
/// that is explicitly writable AND not enabled, so a live account can never reach
/// this path by configuration alone. The fixture satisfies the guard rather than
/// bypassing it.
fn writable_account(mount_path: PathBuf) -> crate::accounts::Account {
    let mut account = account(mount_path);
    account.access = AccessMode::ReadWrite;
    account.enabled = false;
    account
}

pub(super) async fn run() {
    let files = 20_000;
    let per_directory = 1_000;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Arc::new(GeneratedLibrary {
        files,
        per_directory,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    });
    let engine = Engine::new(
        writable_account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("capacity-drive");
    crate::refresh(provider.as_ref(), &scope, &engine.db, false, &engine.cancel)
        .await
        .unwrap();
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(
            &temp.path().join("journal"),
            &engine.account.id,
            1024 * 1024,
        )
        .unwrap(),
    ));
    // The writable filesystem without its transfer and mutation workers: this
    // measures the namespace, and starting workers would add upload machinery
    // that has nothing to do with what is being asked.
    let fs = CloudFs::new_experimental_writable(engine.clone(), journal)
        .await
        .unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    let started = Instant::now();
    report(namespace_sample(&inner, "indexed_baseline", 0.0));

    for pass in 1..=3 {
        let root = mount.clone();
        let directories = files / per_directory;
        let visited = tokio::task::spawn_blocking(move || {
            let mut seen = 0usize;
            for directory in 0..directories {
                let path = root.join(format!("directory-{directory:06}"));
                for entry in std::fs::read_dir(&path).unwrap() {
                    let entry = entry.unwrap();
                    // stat every child: a plain readdir never resolves a view,
                    // and resolved views are the thing under measurement.
                    assert!(entry.metadata().unwrap().len() == 0);
                    seen += 1;
                }
            }
            seen
        })
        .await
        .unwrap();
        assert_eq!(visited, files);
        report(namespace_sample(
            &inner,
            "traversed",
            started.elapsed().as_secs_f64(),
        ));

        // Writable mounts collapse a file's revisions onto one inode, so a new
        // revision must not mint views the read-only path would have minted.
        provider.revision.fetch_add(1, Ordering::SeqCst);
        inner.engine.changed.notify_one();
        parents::settle(&inner, 1).await;
        report(namespace_sample(
            &inner,
            "released",
            started.elapsed().as_secs_f64(),
        ));
        assert_eq!(
            inner.views.lock().unwrap().len(),
            1,
            "pass {pass} did not retire to the root"
        );
    }

    assert_eq!(
        provider.content_reads.load(Ordering::SeqCst),
        0,
        "a metadata traversal must not read content"
    );
    assert_eq!(
        provider.foreground_requests.load(Ordering::SeqCst),
        0,
        "an indexed library must not reach the provider during traversal"
    );
    session.join().unwrap();
    engine.stop().await;
}
