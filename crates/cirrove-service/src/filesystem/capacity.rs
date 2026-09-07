//! Explicit synthetic kernel benchmark; reports current retention, not a capacity pass.
#![allow(clippy::unwrap_used)]
use super::*;
use async_trait::async_trait;
use cirrove_auth::{AccessMode, AppRegistration, Identity};
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, ReadProvider,
};
use cirrove_onedrive::DriveInfo;
use std::{path::PathBuf, sync::atomic::AtomicU32, time::Instant};

const PER_DIRECTORY: usize = 1_000;
const PAGE: usize = 1_000;
struct GeneratedLibrary {
    files: usize,
    per_directory: usize,
    revision: AtomicU32,
    content_reads: AtomicU64,
    foreground_requests: AtomicU64,
}
impl GeneratedLibrary {
    fn directories(&self) -> usize {
        self.files.div_ceil(self.per_directory)
    }
    fn node_at(&self, index: usize) -> Node {
        let (id, parent, name, kind) = if index == 0 {
            (
                "root".into(),
                None,
                "Capacity fixture".into(),
                NodeKind::Folder,
            )
        } else if index <= self.directories() {
            let name = format!("directory-{:06}", index - 1);
            (name.clone(), Some("root".into()), name, NodeKind::Folder)
        } else {
            let file = index - self.directories() - 1;
            (
                format!("synthetic-file-identity-{file:08}"),
                Some(format!("directory-{:06}", file / self.per_directory)),
                format!("Projektunterlagen – Übersicht {file:08}.txt"),
                NodeKind::File,
            )
        };
        Node {
            id,
            parent_id: parent,
            name,
            kind,
            size: 0,
            modified_unix: 1_700_000_000,
            etag: Some(format!("revision-{}", self.revision.load(Ordering::SeqCst))),
            content_version: None,
            target: None,
        }
    }
}
#[async_trait]
impl MetadataProvider for GeneratedLibrary {
    fn provider_id(&self) -> &'static str {
        "namespace-capacity-fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let start = cursor
            .and_then(|c| c.0.strip_prefix("page-"))
            .and_then(|c| c.parse::<usize>().ok())
            .unwrap_or(0);
        let total = self.files + self.directories() + 1;
        let end = (start + PAGE).min(total);
        Ok(ChangePage {
            changes: (start..end)
                .map(|i| Change::Upsert(self.node_at(i)))
                .collect(),
            checkpoint: if end == total {
                Checkpoint::Complete(Cursor(format!(
                    "complete-{}",
                    self.revision.load(Ordering::SeqCst)
                )))
            } else {
                Checkpoint::Continue(Cursor(format!("page-{end}")))
            },
        })
    }
}
#[async_trait]
impl ReadProvider for GeneratedLibrary {
    async fn node(&self, _: &Scope, _: &str, _: &CancellationToken) -> Result<Node, ProviderError> {
        self.foreground_requests.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Unavailable)
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        self.foreground_requests.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Unavailable)
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        self.content_reads.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Unavailable)
    }
}
fn account(mount_path: PathBuf) -> crate::accounts::Account {
    crate::accounts::Account {
        id: "00000000-0000-4000-8000-000000000011".into(),
        label: "capacity-fixture".into(),
        registration: AppRegistration {
            client_id: "00000000-0000-4000-8000-000000000012".into(),
            authority: "common".into(),
        },
        identity: Identity {
            tenant_id: "00000000-0000-4000-8000-000000000013".into(),
            subject: "synthetic".into(),
            username: "capacity@example.invalid".into(),
            graph_user_id: "synthetic-user".into(),
            display_name: "Synthetic capacity fixture".into(),
        },
        credential_id: "00000000-0000-4000-8000-000000000014".into(),
        access: AccessMode::ReadOnly,
        drive: DriveInfo {
            id: "capacity-drive".into(),
            name: "Capacity fixture".into(),
            drive_type: "business".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: "root".into(),
        mount_path,
        enabled: true,
        poll_seconds: 30,
        cache_bytes: 16 * 1024 * 1024,
    }
}
fn process_memory() -> serde_json::Value {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap();
    let value = |source: &str, key: &str| -> u64 {
        source
            .lines()
            .find(|line| line.starts_with(key))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    };
    serde_json::json!({
        "rss_kib": value(&status, "VmRSS:"), "peak_rss_kib": value(&status, "VmHWM:"),
        "pss_kib": value(&rollup, "Pss:"), "anonymous_pss_kib": value(&rollup, "Pss_Anon:")
    })
}
fn namespace_sample(inner: &Inner, phase: &str, seconds: f64) -> serde_json::Value {
    let views = inner.views.lock().unwrap();
    let retained = views.len();
    let capacity = views.capacity();
    drop(views);
    let directories = inner.directories.lock().unwrap();
    let handles = directories.len();
    let entries: u64 = directories.values().map(|v| v.snapshot.len()).sum();
    drop(directories);
    let (snapshot_bytes, snapshot_reservations) = inner.directory_budget.usage();
    serde_json::json!({
        "phase": phase, "seconds": seconds, "retained_views": retained,
        "view_map_capacity": capacity, "open_directory_handles": handles,
        "directory_snapshot_entries": entries,
        "snapshot_logical_bytes": snapshot_bytes, "snapshot_reservations": snapshot_reservations,
        "open_files": inner.files.lock().unwrap().len(),
        "memory": process_memory(),
    })
}
fn report(value: serde_json::Value) {
    println!("CIRROVE_NAMESPACE_CAPACITY {value}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; listed entries must not become mount-lifetime views"]
async fn real_directory_listing_releases_unlooked_up_projections() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Arc::new(GeneratedLibrary {
        files: 3_000,
        per_directory: PER_DIRECTORY,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    });
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("capacity-drive");
    crate::refresh(provider.as_ref(), &scope, &engine.db, false, &engine.cancel)
        .await
        .unwrap();
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    let path = mount.clone();
    let held = tokio::task::spawn_blocking(move || {
        for directory in 0..3 {
            let entries =
                std::fs::read_dir(path.join(format!("directory-{directory:06}"))).unwrap();
            assert_eq!(
                entries.fold(0, |count, entry| {
                    entry.unwrap();
                    count + 1
                }),
                1_000
            );
        }
        std::fs::File::open(
            path.join("directory-000000/Projektunterlagen – Übersicht 00000000.txt"),
        )
        .unwrap()
    })
    .await
    .unwrap();
    let old_inode = held.metadata().unwrap().ino();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !inner.directories.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        inner.views.lock().unwrap().len() <= 5,
        "plain listings retained thousands of views"
    );
    let original = inner.view(old_inode).unwrap();
    provider.revision.store(2, Ordering::SeqCst);
    crate::refresh(provider.as_ref(), &scope, &engine.db, false, &engine.cancel)
        .await
        .unwrap();
    engine.changed.notify_one();
    let path = mount.clone();
    let newest = tokio::task::spawn_blocking(move || {
        let entries = std::fs::read_dir(path.join("directory-000000")).unwrap();
        assert_eq!(
            entries.fold(0, |count, entry| {
                entry.unwrap();
                count + 1
            }),
            1_000
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let file = std::fs::File::open(
                path.join("directory-000000/Projektunterlagen – Übersicht 00000000.txt"),
            )
            .unwrap();
            if file.metadata().unwrap().ino() != old_inode {
                break file;
            }
            assert!(
                Instant::now() < deadline,
                "remote revision invalidation did not arrive"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    })
    .await
    .unwrap();
    assert_eq!(held.metadata().unwrap().ino(), old_inode);
    assert_ne!(newest.metadata().unwrap().ino(), old_inode);
    assert_eq!(
        inner.view(old_inode).unwrap().node.etag,
        original.node.etag,
        "a listing replaced the view belonging to an old open file"
    );
    assert!(inner.views.lock().unwrap().len() <= 6);
    assert_eq!(provider.foreground_requests.load(Ordering::SeqCst), 0);
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    drop((held, newest));
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; validates FORGET and open-file view lifetimes"]
async fn real_resolved_file_views_retire_after_kernel_and_open_references() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Arc::new(GeneratedLibrary {
        files: 300,
        per_directory: PER_DIRECTORY,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    });
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("capacity-drive");
    crate::refresh(provider.as_ref(), &scope, &engine.db, false, &engine.cancel)
        .await
        .unwrap();
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    let path = mount.join("directory-000000");
    let held = tokio::task::spawn_blocking(move || {
        for entry in std::fs::read_dir(&path).unwrap() {
            // Explicit metadata lookup (plain READDIR alone owns no kernel reference).
            assert_eq!(std::fs::metadata(entry.unwrap().path()).unwrap().len(), 0);
        }
        std::fs::File::open(path.join("Projektunterlagen – Übersicht 00000000.txt")).unwrap()
    })
    .await
    .unwrap();
    let old_inode = held.metadata().unwrap().ino();
    assert!(inner.views.lock().unwrap().len() > 250);
    provider.revision.store(2, Ordering::SeqCst);
    crate::refresh(provider.as_ref(), &scope, &engine.db, false, &engine.cancel)
        .await
        .unwrap();
    engine.changed.notify_one();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = inner
            .views
            .lock()
            .unwrap()
            .values()
            .filter(|view| view.node.kind == NodeKind::File)
            .count();
        if remaining == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{remaining} file views survived invalidation"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        inner.view(old_inode).unwrap().node.etag.as_deref(),
        Some("revision-1")
    );
    assert_eq!(held.metadata().unwrap().ino(), old_inode);
    let path = mount.join("directory-000000/Projektunterlagen – Übersicht 00000000.txt");
    let newest = tokio::task::spawn_blocking(move || std::fs::File::open(path).unwrap())
        .await
        .unwrap();
    assert_ne!(newest.metadata().unwrap().ino(), old_inode);
    drop((held, newest));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        engine.changed.notify_one();
        if inner.files.lock().unwrap().is_empty()
            && inner
                .views
                .lock()
                .unwrap()
                .values()
                .all(|v| v.node.kind != NodeKind::File)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "closed and forgotten files were not reclaimed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(provider.foreground_requests.load(Ordering::SeqCst), 0);
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit 500k-file synthetic FUSE/RSS benchmark; no cloud; not a release acceptance pass"]
async fn namespace_capacity_baseline() {
    let files = std::env::var("CIRROVE_NAMESPACE_FILES")
        .map_or(500_000, |value| value.parse::<usize>().unwrap());
    assert!(
        (1_000..=500_000).contains(&files),
        "fixture size must be 1,000..=500,000"
    );
    let per_directory = std::env::var("CIRROVE_NAMESPACE_PER_DIRECTORY")
        .map_or(PER_DIRECTORY, |value| value.parse::<usize>().unwrap());
    assert!((1..=files).contains(&per_directory));
    let provider = Arc::new(GeneratedLibrary {
        files,
        per_directory,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    });
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("capacity-drive");
    let started = Instant::now();
    crate::refresh(provider.as_ref(), &scope, &engine.db, false, &engine.cancel)
        .await
        .unwrap();
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    report(serde_json::json!({
        "fixture": "synthetic kernel FUSE; generated metadata, no credentials or content reads",
        "files": files, "directories": provider.directories(), "files_per_directory": per_directory,
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        "interpretation": "baseline measurement only; no memory capacity gate is asserted",
    }));
    report(namespace_sample(
        &inner,
        "indexed_baseline",
        started.elapsed().as_secs_f64(),
    ));
    for pass in 1..=3 {
        if pass > 1 {
            provider.revision.store(pass, Ordering::SeqCst);
            crate::refresh(provider.as_ref(), &scope, &engine.db, false, &engine.cancel)
                .await
                .unwrap();
            // Normal invalidation machinery participates, without another background feed.
            engine.changed.notify_one();
        }
        let path = mount.clone();
        let directories = provider.directories();
        let sample_inner = inner.clone();
        let started = Instant::now();
        tokio::task::spawn_blocking(move || {
            assert_eq!(
                std::fs::read_dir(&path)
                    .unwrap()
                    .try_fold(0, |count, entry| entry.map(|_| count + 1))
                    .unwrap(),
                directories
            );
            let mut total = 0;
            for directory in 0..directories {
                let opened = Instant::now();
                let mut entries =
                    std::fs::read_dir(path.join(format!("directory-{directory:06}"))).unwrap();
                let first = entries.next().unwrap().unwrap();
                assert!(
                    first
                        .file_name()
                        .to_string_lossy()
                        .starts_with("Projektunterlagen")
                );
                total += 1;
                if directory == 0 {
                    report(namespace_sample(
                        &sample_inner,
                        &format!("first_entry_revision_{pass}"),
                        opened.elapsed().as_secs_f64(),
                    ));
                }
                for entry in entries {
                    let entry = entry.unwrap();
                    assert!(
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with("Projektunterlagen")
                    );
                    total += 1;
                }
            }
            assert_eq!(total, files);
        })
        .await
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        // RELEASEDIR removes the map entry before its blocking close finishes;
        // the final in-flight page can also still own the immutable snapshot.
        while !inner.directories.lock().unwrap().is_empty()
            || inner.directory_budget.usage() != (0, 0)
        {
            assert!(
                Instant::now() < deadline,
                "directory releases did not finish"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(inner.directory_budget.usage(), (0, 0));
        report(namespace_sample(
            &inner,
            &format!("closed_after_revision_{pass}"),
            started.elapsed().as_secs_f64(),
        ));
        parents::settle(&inner, 1).await;
        report(namespace_sample(
            &inner,
            &format!("after_invalidation_revision_{pass}"),
            started.elapsed().as_secs_f64(),
        ));
    }
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        provider.foreground_requests.load(Ordering::SeqCst),
        0,
        "indexed traversal requested uncached metadata"
    );
    // Stop and join only this owned temporary mount; application state is untouched.
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; cached name lookup in one 50k-file directory"]
async fn real_indexed_name_lookup_avoids_materializing_the_directory() {
    let files = std::env::var("CIRROVE_NAME_LOOKUP_FILES")
        .map_or(50_000, |value| value.parse::<usize>().unwrap());
    assert!((16..=500_000).contains(&files));
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Arc::new(GeneratedLibrary {
        files,
        per_directory: files,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    });
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("capacity-drive");
    crate::refresh(provider.as_ref(), &scope, &engine.db, false, &engine.cancel)
        .await
        .unwrap();
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    let before = process_memory();
    let started = Instant::now();
    tokio::task::spawn_blocking(move || {
        let directory = mount.join("directory-000000");
        for file in (0..16).map(|i| i * (files - 1) / 15) {
            let path = directory.join(format!("Projektunterlagen – Übersicht {file:08}.txt"));
            assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
        }
        assert_eq!(
            std::fs::metadata(directory.join("absent.txt"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
    })
    .await
    .unwrap();
    let elapsed = started.elapsed();
    println!(
        "CIRROVE_NAMED_LOOKUP {}",
        serde_json::json!({
            "fixture": "actual kernel FUSE; indexed synthetic metadata; no cloud account",
            "build": if cfg!(debug_assertions) { "debug" } else { "release" },
            "files_in_directory": files, "operations": 17,
            "elapsed_ms": elapsed.as_secs_f64() * 1000.0,
            "memory_before": before, "memory_after": process_memory(),
            "retained_views": inner.views.lock().unwrap().len(),
            "snapshot_reservations": inner.directory_budget.usage().1,
            "provider_foreground_requests": provider.foreground_requests.load(Ordering::SeqCst),
            "content_reads": provider.content_reads.load(Ordering::SeqCst),
        })
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "indexed lookup exceeded the cached navigation bound"
    );
    assert_eq!(inner.directory_budget.usage(), (0, 0));
    assert!(inner.views.lock().unwrap().len() <= 18);
    assert_eq!(provider.foreground_requests.load(Ordering::SeqCst), 0);
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    parents::settle(&inner, 1).await;
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

mod parents;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic FUSE; deep directories, held snapshots and duplicate aliases"]
async fn real_directory_ancestry_retires_after_last_kernel_snapshot_and_file_user() {
    parents::directories_and_aliases().await;
}

mod cold;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "actual synthetic FUSE; cold publication, offline revisit and remount"]
async fn real_cold_directory_pages_publish_with_bounded_memory() {
    cold::mounted().await;
}
