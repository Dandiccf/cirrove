#![allow(clippy::unwrap_used)]
use super::*;
use std::sync::Condvar;
use std::time::Duration;

pub(in crate::filesystem::writeback) struct Probe {
    entered: tokio::sync::mpsc::UnboundedSender<(Uuid, CancellationToken)>,
    released: Mutex<bool>,
    changed: Condvar,
}
impl Probe {
    pub(super) fn pause(&self, id: Uuid, cancel: &CancellationToken) -> crate::journal::Result<()> {
        self.entered
            .send((id, cancel.clone()))
            .map_err(|_| JournalError::Storage)?;
        let released = self.released.lock().map_err(|_| JournalError::Storage)?;
        let (released, _) = self
            .changed
            .wait_timeout_while(released, Duration::from_secs(10), |v| !*v)
            .map_err(|_| JournalError::Storage)?;
        if !*released {
            return Err(JournalError::Storage);
        }
        Ok(())
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
fn install(
    writer: &Writeback,
) -> (
    Arc<Probe>,
    tokio::sync::mpsc::UnboundedReceiver<(Uuid, CancellationToken)>,
) {
    let (entered, receive) = tokio::sync::mpsc::unbounded_channel();
    let probe = Arc::new(Probe {
        entered,
        released: Mutex::new(false),
        changed: Condvar::new(),
    });
    *writer.sealing.probe.lock().unwrap() = Some(probe.clone());
    (probe, receive)
}
async fn entered(
    receive: &mut tokio::sync::mpsc::UnboundedReceiver<(Uuid, CancellationToken)>,
) -> (Uuid, CancellationToken) {
    tokio::time::timeout(Duration::from_secs(5), receive.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn dirty(writer: &Writeback, id: Uuid) {
    writer.truncate(id, 0).await.unwrap();
    writer
        .write(
            id,
            0,
            crate::native_import::synthetic_package_archive("Owned.pages/Document", b"dirty"),
            false,
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn native_seal_abandoned_worker_cancels_but_retains_owned_permits_until_terminal() {
    let (_temp, writer, file) = super::super::native_seal_tests::fixture();
    let writer = Arc::new(writer);
    writer.refresh_projection().await.unwrap();
    dirty(&writer, file.id).await;
    let (probe, mut receive) = install(&writer);
    let owned = writer.clone();
    let task = tokio::spawn(async move { owned.seal(file.id).await });
    let (id, cancel) = entered(&mut receive).await;
    assert_eq!(id, file.id);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        cancel.is_cancelled(),
        "abandoned seal did not cancel blocking validation"
    );
    assert_eq!(
        writer.sealing.slots.available_permits(),
        1,
        "aborting waiter released permit while worker was still alive"
    );
    let gate = writer.sealing.working(id).unwrap();
    assert!(gate.try_lock().is_err());
    probe.release();
    let _finished = tokio::time::timeout(Duration::from_secs(5), gate.lock())
        .await
        .unwrap();
    assert_eq!(writer.sealing.slots.available_permits(), 2);
    let journal = writer.journal.lock().unwrap();
    assert!(journal.working_file(id).unwrap().dirty);
    assert!(journal.list(0, 10).unwrap().is_empty());
}
#[tokio::test]
async fn native_seal_workers_are_bounded_and_same_generation_fsyncs_coalesce() {
    let (_temp, writer, file) = super::super::native_seal_tests::fixture();
    let writer = Arc::new(writer);
    writer.refresh_projection().await.unwrap();
    dirty(&writer, file.id).await;
    let ordinary = writer
        .create(
            file.scope.clone(),
            Node {
                id: String::new(),
                parent_id: Some("root".into()),
                name: "ordinary.txt".into(),
                kind: NodeKind::File,
                size: 0,
                modified_unix: 0,
                etag: None,
                content_version: None,
                target: None,
                package: false,
            },
        )
        .await
        .unwrap();
    writer
        .write(ordinary.id, 0, b"ordinary".to_vec(), false)
        .await
        .unwrap();
    let mut third_node = ordinary.node.clone();
    third_node.id.clear();
    third_node.name = "third.txt".into();
    let third = writer.create(file.scope.clone(), third_node).await.unwrap();
    writer
        .write(third.id, 0, b"third".to_vec(), false)
        .await
        .unwrap();
    let (probe, mut receive) = install(&writer);
    let mut tasks = Vec::new();
    for id in [file.id, file.id, ordinary.id, third.id] {
        let owned = writer.clone();
        tasks.push(tokio::spawn(async move { owned.seal(id).await }));
    }
    let a = entered(&mut receive).await.0;
    let b = entered(&mut receive).await.0;
    assert_ne!(a, b, "same working generation entered blocking work twice");
    assert_eq!(writer.sealing.slots.available_permits(), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), receive.recv())
            .await
            .is_err(),
        "third distinct file exceeded the two-worker limit"
    );
    probe.release();
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert_eq!(writer.sealing.slots.available_permits(), 2);
    let journal = writer.journal.lock().unwrap();
    let rows = journal.list(0, 10).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .filter(|r| !r.representation.is_file_bytes())
            .count(),
        1
    );
}
