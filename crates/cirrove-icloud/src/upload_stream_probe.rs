//! Explicit developer-only body boundary. Never compiled into the normal daemon.
//! Counts bytes handed to the HTTP body; it does not claim remote acceptance.
use std::{
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::Notify;
static BOUNDARY: OnceLock<Arc<UploadStreamBoundary>> = OnceLock::new();

pub struct UploadStreamBoundary {
    name: String,
    size: u64,
    after: u64,
    reached: AtomicU64,
    notify: Notify,
}
impl UploadStreamBoundary {
    fn new(name: String, size: u64, after: u64) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !name.is_empty() && after >= 65536 && after < size.saturating_sub(65536),
            "invalid upload stream boundary"
        );
        Ok(Self {
            name,
            size,
            after,
            reached: AtomicU64::new(0),
            notify: Notify::new(),
        })
    }
    pub async fn wait_reached(&self) -> u64 {
        loop {
            let notified = self.notify.notified();
            let reached = self.reached.load(Ordering::Acquire);
            if reached != 0 {
                return reached;
            }
            notified.await;
        }
    }
}
/// Install once in an isolated probe process, for its exact name and size only.
pub fn install_upload_stream_boundary(
    name: String,
    size: u64,
    after: u64,
) -> anyhow::Result<Arc<UploadStreamBoundary>> {
    let boundary = Arc::new(UploadStreamBoundary::new(name, size, after)?);
    BOUNDARY
        .set(boundary.clone())
        .map_err(|_| anyhow::anyhow!("upload stream boundary already installed"))?;
    Ok(boundary)
}
pub(crate) fn reader<R: AsyncRead + Unpin>(inner: R, name: &str, size: u64) -> ProbeReader<R> {
    let boundary = BOUNDARY
        .get()
        .filter(|v| v.name == name && v.size == size)
        .cloned();
    ProbeReader {
        inner,
        boundary,
        yielded: 0,
    }
}
pub(crate) struct ProbeReader<R> {
    inner: R,
    boundary: Option<Arc<UploadStreamBoundary>>,
    yielded: u64,
}
impl<R: AsyncRead + Unpin> AsyncRead for ProbeReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let this = self.get_mut();
        if let Some(boundary) = &this.boundary
            && this.yielded >= boundary.after
        {
            boundary.reached.store(this.yielded, Ordering::Release);
            boundary.notify.notify_one();
            // Hold the unfinished request until this probe process is stopped.
            return Poll::Pending;
        }
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) {
            this.yielded += (buf.filled().len() - before) as u64;
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    #[tokio::test]
    async fn stream_yields_prefix_then_stalls_without_yielding_the_remainder() {
        let boundary =
            Arc::new(UploadStreamBoundary::new("owned".into(), 262144, 65536).expect("boundary"));
        let mut reader = ProbeReader {
            inner: std::io::Cursor::new(vec![42; 262144]),
            boundary: Some(boundary.clone()),
            yielded: 0,
        };
        let mut first = vec![0; 65536];
        reader.read_exact(&mut first).await.expect("prefix");
        assert!(first.iter().all(|b| *b == 42));
        assert_eq!(boundary.reached.load(Ordering::Acquire), 0);
        let mut rest = [0; 65536];
        let read = reader.read(&mut rest);
        tokio::pin!(read);
        tokio::select! {
            result=&mut read => panic!("boundary unexpectedly completed: {result:?}"),
            count=boundary.wait_reached() => assert_eq!(count,65536),
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut read)
                .await
                .is_err()
        );
        assert!(rest.iter().all(|b| *b == 0));
    }
    #[tokio::test]
    async fn uninstrumented_reader_is_transparent() {
        let expected = vec![23; 131089];
        let mut reader = ProbeReader {
            inner: std::io::Cursor::new(expected.clone()),
            boundary: None,
            yielded: 0,
        };
        let mut actual = Vec::new();
        reader
            .read_to_end(&mut actual)
            .await
            .expect("ordinary stream");
        assert_eq!(actual, expected);
    }
    #[test]
    fn boundary_must_leave_a_nonempty_prefix_and_remainder() {
        for (size, after) in [
            (65536, 65536),
            (262144, 0),
            (262144, 262144),
            (262144, 262143),
        ] {
            assert!(UploadStreamBoundary::new("owned".into(), size, after).is_err());
        }
    }
}
