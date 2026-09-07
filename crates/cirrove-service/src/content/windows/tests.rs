#![allow(clippy::unwrap_used)]
use super::*;

#[tokio::test]
async fn staging_is_unlinked_bounded_and_checked_before_each_block() {
    let temp = tempfile::tempdir().unwrap();
    let staging = Staging::new(temp.path().to_path_buf(), 32 * 1024 * 1024);
    let mut writer = staging
        .writer(staging.reserve(2 * BLOCK_SIZE).unwrap())
        .await
        .unwrap();
    assert!(staging.reserve(2 * BLOCK_SIZE).is_none());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    for _ in 0..2 * BLOCK_SIZE as usize / CHUNK_LIMIT {
        writer.write_chunk(&[b'A'; CHUNK_LIMIT]).await.unwrap();
    }
    let window = writer.finish(BLOCK_SIZE as u64).unwrap();
    assert_eq!(
        window
            .read(BLOCK_SIZE as u64, BLOCK_SIZE)
            .await
            .unwrap()
            .len(),
        BLOCK_SIZE as usize
    );
    window
        .file
        .file
        .write_all_at(b"corrupted", BLOCK_SIZE as u64)
        .unwrap();
    assert!(matches!(
        window.read(2 * BLOCK_SIZE as u64, BLOCK_SIZE).await,
        Err(ProviderError::Protocol("staged window checksum mismatch"))
    ));
    drop(window);
    assert_eq!(staging.stats().staging_reserved_bytes, 0);
}

#[tokio::test]
async fn incomplete_and_oversized_windows_never_become_readable() {
    let temp = tempfile::tempdir().unwrap();
    let staging = Staging::new(temp.path().to_path_buf(), 32 * 1024 * 1024);
    let mut writer = staging
        .writer(staging.reserve(2 * BLOCK_SIZE).unwrap())
        .await
        .unwrap();
    assert!(writer.write_chunk(&vec![0; CHUNK_LIMIT + 1]).await.is_err());
    writer.write_chunk(b"partial").await.unwrap();
    assert!(writer.finish(0).is_err());
    assert_eq!(staging.stats().staging_reserved_bytes, 0);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn kernel_enospc_aborts_staging_without_publishing_or_leaking_reservations() {
    let temp = tempfile::tempdir().unwrap();
    let staging = Staging::new(temp.path().to_path_buf(), 32 * 1024 * 1024);
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    assert_eq!(
        full.write_all_at(b"test", 0).unwrap_err().raw_os_error(),
        Some(libc::ENOSPC)
    );
    let reservation = staging.reserve(2 * BLOCK_SIZE).unwrap();
    let mut writer = Writer {
        file: Arc::new(ReservedFile {
            file: full,
            _permit: reservation.permit,
        }),
        length: reservation.length,
        written: 0,
        hash: Sha256::new(),
        hashes: vec![],
    };
    assert!(writer.write_chunk(b"test").await.is_err());
    assert!(writer.finish(0).is_err());
    assert_eq!(staging.stats().staging_reserved_bytes, 0);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn cancelled_async_write_keeps_its_reservation_until_the_blocking_job_finishes() {
    use std::future::Future;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let staging = Staging::new(temp.path().to_path_buf(), 32 * 1024 * 1024);
        let mut writer = staging
            .writer(staging.reserve(2 * BLOCK_SIZE).unwrap())
            .await
            .unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        entered_rx.await.unwrap();
        let mut write = Box::pin(writer.write_chunk(b"pending"));
        std::future::poll_fn(|cx| {
            assert!(write.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(write);
        drop(writer);
        // The queued positional write owns the file and its byte reservation.
        assert_eq!(staging.stats().staging_reserved_bytes, 2 * BLOCK_SIZE);
        assert!(staging.reserve(2 * BLOCK_SIZE).is_none());
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        tokio::task::spawn_blocking(|| ()).await.unwrap();
        assert_eq!(staging.stats().staging_reserved_bytes, 0);
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    });
}

mod graph;
