//! Kernel reads through the actual OneDrive adapter, using only loopback data.
//! Metadata is transactionally seeded; background indexing is outside this test.
use super::*;
use crate::{accounts::Account, engine::Engine, filesystem::CloudFs};
use anyhow::{Context, ensure};
use cirrove_auth::{AccessMode, AppRegistration, Identity};
use cirrove_core::{Change, ChangePage, Checkpoint, Cursor};
use cirrove_onedrive::DriveInfo;
use cirrove_store::Store;
use std::{fs::File, os::unix::fs::FileExt, time::Duration};

const SIZE: u64 = 32 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(5);

fn account(mount_path: std::path::PathBuf) -> Account {
    Account {
        id: "account".into(),
        label: "synthetic-window-fixture".into(),
        registration: AppRegistration {
            client_id: "00000000-0000-4000-8000-000000000002".into(),
            authority: "common".into(),
        },
        identity: Identity {
            tenant_id: "00000000-0000-4000-8000-000000000003".into(),
            subject: "synthetic".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: "user".into(),
            display_name: "Synthetic fixture".into(),
        },
        credential_id: "00000000-0000-4000-8000-000000000004".into(),
        access: AccessMode::ReadOnly,
        drive: DriveInfo {
            id: "drive".into(),
            name: "Fixture".into(),
            drive_type: "business".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: "root".into(),
        mount_path,
        enabled: true,
        poll_seconds: 30,
        cache_bytes: 64 * 1024 * 1024,
    }
}

struct Mounted {
    engine: Arc<Engine>,
    session: Option<crate::filesystem::CloudSession>,
    file: Arc<File>,
    other: Arc<File>,
    server: Server,
    control: Arc<Control>,
    temp: tempfile::TempDir,
}
impl Drop for Mounted {
    fn drop(&mut self) {
        // Also runs on a failed assertion. CloudSession owns only this temporary
        // mount and disconnects it even while these test descriptors remain open.
        self.engine.cancel.cancel();
    }
}
impl Mounted {
    async fn new(strong: bool) -> anyhow::Result<Self> {
        let control = Arc::new(Control {
            strong,
            pause_body: AtomicBool::new(true),
            pause_final: AtomicBool::new(true),
            ..Default::default()
        });
        Self::configured(SIZE, 64 * 1024 * 1024, control).await
    }
    async fn configured(size: u64, quota: u64, control: Arc<Control>) -> anyhow::Result<Self> {
        let temp = tempfile::tempdir()?;
        let mount = temp.path().join("mount");
        std::fs::create_dir(&mount)?;
        let server = controlled_server(size, control.clone()).await;
        let mut account = account(mount.clone());
        account.cache_bytes = quota;
        let engine = Engine::new(
            account,
            Arc::new(server.provider.clone()),
            temp.path().join("state"),
        )
        .await?;
        let files = control.files.clone();
        let (db, scope) = (engine.db.clone(), engine.scope("drive"));
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut store = Store::open(db)?;
            let cursor = store.begin(&scope, true)?;
            let changes = [
                ("root", None, NodeKind::Folder, 0),
                ("file", Some("root"), NodeKind::File, size),
                ("folder", Some("root"), NodeKind::Folder, 0),
                ("other", Some("folder"), NodeKind::File, 16 * 1024),
            ]
            .into_iter()
            .chain(
                files
                    .iter()
                    .map(|(id, (size, _))| (id.as_str(), Some("root"), NodeKind::File, *size)),
            )
            .map(|(id, parent, kind, size)| {
                Change::Upsert(Node {
                    id: id.into(),
                    name: id.into(),
                    parent_id: parent.map(str::to_owned),
                    kind,
                    size,
                    modified_unix: 1_700_000_000,
                    etag: Some("meta".into()),
                    content_version: Some("v1".into()),
                    target: None,
                })
            })
            .collect();
            store.stage(
                &scope,
                cursor.as_ref(),
                &ChangePage {
                    changes,
                    checkpoint: Checkpoint::Complete(Cursor("synthetic".into())),
                },
            )?;
            Ok(())
        })
        .await??;
        let session = CloudFs::new(engine.clone())?.mount(&mount)?;
        let (file, other) = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
            Ok((
                File::open(mount.join("file"))?,
                File::open(mount.join("folder/other"))?,
            ))
        })
        .await??;
        Ok(Self {
            engine,
            session: Some(session),
            file: Arc::new(file),
            other: Arc::new(other),
            server,
            control,
            temp,
        })
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        self.engine.stop().await;
        let session = self.session.take().context("session already closed")?;
        tokio::time::timeout(
            DEADLINE,
            tokio::task::spawn_blocking(move || session.umount_and_join()),
        )
        .await
        .context("unmount stalled with open descriptors")???;
        // Anonymous partial windows can retain a queued blocking write briefly.
        tokio::time::timeout(DEADLINE, async {
            while self.engine.cache.window_stats().staging_reserved_bytes != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("staging reservation survived cancelled mount")?;
        ensure!(
            std::fs::read_dir(self.temp.path().join("mount"))?
                .next()
                .is_none(),
            "temporary mount did not detach"
        );
        Ok(())
    }
}

type Reader = tokio::task::JoinHandle<std::io::Result<Vec<u8>>>;
fn read(file: Arc<File>, offset: u64) -> Reader {
    tokio::task::spawn_blocking(move || {
        let mut bytes = vec![0; 4096];
        file.read_exact_at(&mut bytes, offset)?;
        Ok(bytes)
    })
}
async fn bytes(reader: Reader, expected: u8) -> anyhow::Result<()> {
    let bytes = tokio::time::timeout(DEADLINE, reader).await???;
    ensure!(
        bytes.iter().all(|b| *b == expected),
        "incorrect mounted bytes"
    );
    Ok(())
}
async fn signal(notify: &Notify) -> anyhow::Result<()> {
    tokio::time::timeout(DEADLINE, notify.notified()).await?;
    Ok(())
}

mod navigation;
use navigation::navigate;

#[derive(Debug, Clone, Copy)]
enum Outcome {
    Valid,
    Changed,
    Cancelled,
}

async fn scenario(outcome: Outcome, strong: bool) -> anyhow::Result<()> {
    let mut mounted = Mounted::new(strong).await?;
    let mut readers = Vec::new();
    let mut samples = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(25), async {
        // The first actual syscall establishes the version-bound session. The
        // next block must create an 8 MiB streamed window, not a plain range.
        bytes(read(mounted.file.clone(), 17), b'A').await?;
        ensure!(mounted.server.graph.load(Ordering::SeqCst) == 2);
        readers.push(read(mounted.file.clone(), BLOCK_SIZE as u64 + 17));
        signal(&mounted.control.body_entered).await.context("window never streamed")?;
        readers.push(read(mounted.file.clone(), 2 * BLOCK_SIZE as u64 + 33));
        ensure!(mounted.engine.cache.window_stats().staging_reserved_bytes == 2 * BLOCK_SIZE);

        // Both another file and a distant range of the same file must remain
        // independent. Two pending blocks already occupy two content loaders.
        bytes(read(mounted.other.clone(), 0), b'B').await?;
        bytes(read(mounted.file.clone(), 7 * BLOCK_SIZE as u64 + 19), b'A').await?;
        for _ in 0..20 {
            navigate(&mounted, &mut samples).await?;
            ensure!(readers.iter().all(|reader| !reader.is_finished()), "partial window was exposed");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        ensure!(mounted.server.content.load(Ordering::SeqCst) == 4, "overlap started a duplicate download");
        ensure!(mounted.engine.cache.window_stats().validated_windows == 0);

        if matches!(outcome, Outcome::Cancelled) {
            mounted.engine.stop().await;
        } else {
            mounted.control.release_body.notify_one();
            if !strong {
                signal(&mounted.control.final_entered).await.context("window never reached final Graph check")?;
                // The complete body has now reached the staging sink, but the final
                // Graph comparison has not returned. No syscall may receive it yet.
                for _ in 0..10 {
                    navigate(&mounted, &mut samples).await?;
                    ensure!(readers.iter().all(|reader| !reader.is_finished()), "unvalidated window was exposed");
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                ensure!(mounted.engine.cache.window_stats().validated_windows == 0);
                mounted.control.changed.store(matches!(outcome, Outcome::Changed), Ordering::SeqCst);
                mounted.control.release_final.notify_one();
            }
        }
        for reader in &mut readers {
            let result = tokio::time::timeout(DEADLINE, reader).await??;
            match outcome {
                Outcome::Valid => ensure!(result?.iter().all(|byte| *byte == b'A')),
                Outcome::Changed => ensure!(result.unwrap_err().raw_os_error() == Some(libc::ESTALE)),
                Outcome::Cancelled => ensure!(result.unwrap_err().raw_os_error() == Some(libc::ENODEV)),
            }
        }
        let stats = mounted.engine.cache.window_stats();
        ensure!(stats.validated_windows == u64::from(matches!(outcome, Outcome::Valid)));
        ensure!(mounted.server.graph.load(Ordering::SeqCst) == if strong { 4 } else if matches!(outcome, Outcome::Cancelled) { 7 } else { 8 });
        ensure!(mounted.server.content.load(Ordering::SeqCst) == 4);
        if matches!(outcome, Outcome::Changed) {
            let entries = std::fs::read_dir(&mounted.engine.cache.path)?.collect::<Result<Vec<_>, _>>()?;
            // Only the primed block and the two independent reads are durable;
            // neither block of the rejected window may have been published.
            ensure!(entries.len() == 3, "rejected window left a cache publication");
        }
        samples.sort_by(f64::total_cmp);
        println!("CIRROVE_KERNEL_WINDOWS {}", serde_json::json!({
            "fixture": "actual kernel FUSE + loopback OneDrive adapter; cached metadata, no background indexing or cloud account",
            "strong": strong, "build_profile": build_profile(), "outcome": format!("{outcome:?}"), "window_bytes": 2 * BLOCK_SIZE,
            "graph_gets": mounted.server.graph.load(Ordering::SeqCst),
            "content_gets": mounted.server.content.load(Ordering::SeqCst),
            "staging": stats, "directory_samples": samples.len(),
            "directory_p50_ms": samples[(samples.len()-1)/2],
            "directory_p95_ms": samples[(samples.len()-1)*95/100],
            "directory_max_ms": samples.last(),
        }));
        Ok::<_, anyhow::Error>(())
    }).await.context("kernel window scenario deadline").and_then(|r| r);
    // Always cancel and detach before reporting failure. Open descriptors are
    // intentionally retained through shutdown, including on the valid path.
    let cleanup = mounted.close().await;
    result?;
    cleanup
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse and fusermount3; synthetic loopback only"]
async fn real_window_coalesces_and_preserves_navigation_until_validated() -> anyhow::Result<()> {
    scenario(Outcome::Valid, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse and fusermount3; synthetic loopback only"]
async fn real_window_rejects_changed_content_without_publishing() -> anyhow::Result<()> {
    scenario(Outcome::Changed, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse and fusermount3; synthetic loopback only"]
async fn real_window_cancels_and_unmounts_with_open_readers() -> anyhow::Result<()> {
    scenario(Outcome::Cancelled, false).await
}

mod workloads;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse and fusermount3; synthetic loopback only"]
async fn real_strong_window_coalesces_and_keeps_navigation_available() -> anyhow::Result<()> {
    scenario(Outcome::Valid, true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse and fusermount3; synthetic loopback only"]
async fn real_strong_window_cancellation_discards_partial_staging() -> anyhow::Result<()> {
    scenario(Outcome::Cancelled, true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires FUSE and Python SQLite; only an isolated metadata database"]
async fn real_cached_navigation_does_not_wait_for_metadata_writer() -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut mounted = Mounted::configured(SIZE, 64 * 1024 * 1024, Arc::default()).await?;
    // Opening `other` during setup already allocated its inode. Hold a writer
    // in a separate process while ordinary read_dir/stat requests use that map.
    let mut writer = tokio::process::Command::new("python3")
        .args(["-u", "-c", "import sqlite3,sys; db=sqlite3.connect(sys.argv[1],isolation_level=None); db.execute('BEGIN IMMEDIATE'); print('held',flush=True); sys.stdin.read(1); db.execute('ROLLBACK')"])
        .arg(&mounted.engine.db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = writer.stdin.take().context("writer stdin")?;
    let mut output = BufReader::new(writer.stdout.take().context("writer stdout")?).lines();
    let result = async {
        ensure!(tokio::time::timeout(DEADLINE, output.next_line()).await??.as_deref() == Some("held"));
        let mut samples = Vec::new();
        for _ in 0..5 {
            navigate(&mounted, &mut samples).await?;
        }
        ensure!(mounted.server.graph.load(Ordering::SeqCst) == 0);
        ensure!(mounted.server.content.load(Ordering::SeqCst) == 0);
        println!("CIRROVE_CACHED_WRITER_NAV {}", serde_json::json!({"fixture":"actual kernel directory reads while a separate process holds the SQLite writer; synthetic only", "directory_samples_ms":samples}));
        Ok::<_, anyhow::Error>(())
    }.await;
    // Release the lock and detach our mount before returning an assertion error.
    let release = input.write_all(b"x").await;
    let exited = tokio::time::timeout(DEADLINE, writer.wait()).await;
    let cleanup = mounted.close().await;
    result?;
    release?;
    ensure!(exited??.success(), "synthetic metadata writer failed");
    cleanup
}
