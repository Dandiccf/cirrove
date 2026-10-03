//! Inherited descriptors must not outlive the final logical journal owner.
use super::*;
use std::{
    os::fd::AsRawFd,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
struct Inheritor(Child);
impl Drop for Inheritor {
    fn drop(&mut self) {
        // Only this synthetic child's exact PID is stopped; never a pattern.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn inherit(owner: &File) -> Inheritor {
    // Model the real fork-before-exec descriptor inheritance without unsafe
    // fork in the multithreaded Rust test process. The helper never uses SQLite
    // or takes/unlocks the lock; it only holds the inherited descriptor alive.
    let flags = rustix::io::fcntl_getfd(owner).expect("owner descriptor flags");
    rustix::io::fcntl_setfd(owner, flags & !rustix::io::FdFlags::CLOEXEC)
        .expect("synthetic inheritance seam");
    let spawned = Command::new("python3")
        .args(["-I", "-S", "-c",
            "import os,sys; os.fstat(int(sys.argv[1])); sys.stdout.buffer.write(b'ready'); sys.stdout.buffer.flush(); sys.stdin.buffer.read(1)"])
        .arg(owner.as_raw_fd().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    rustix::io::fcntl_setfd(owner, flags).expect("restore close-on-exec");
    let mut child = Inheritor(spawned.expect("synthetic descriptor holder"));
    let mut stdout = child.0.stdout.take().expect("readiness pipe");
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut ready = [0; 5];
        let okay = stdout.read_exact(&mut ready).is_ok() && ready == *b"ready";
        let _ = tx.send(okay);
    });
    assert!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("bounded child readiness")
    );
    reader.join().expect("readiness reader");
    child
}
#[test]
fn final_journal_lease_releases_inherited_lock_but_live_reservation_keeps_it() {
    let temp = tempfile::tempdir().expect("private fixture");
    let root = temp.path().join("journal");
    let mut journal = UploadJournal::open(&root, "owned", 1024).expect("journal");
    let source = journal.reserve_working(1).expect("reserved working source");
    let mut child = inherit(journal._owner.file());
    drop(journal);
    assert!(
        matches!(
            RecoveryJournal::open(&root, "owned"),
            Err(JournalError::Busy)
        ),
        "a live reservation must retain exclusive journal ownership"
    );
    drop(source);
    assert!(
        child.0.try_wait().expect("child status").is_none(),
        "holder must remain alive"
    );
    let recovery = RecoveryJournal::open(&root, "owned")
        .expect("final logical owner must release lock despite unrelated inherited descriptor");
    assert!(matches!(
        UploadJournal::open(&root, "owned", 1024),
        Err(JournalError::Busy)
    ));
    drop(recovery);
    drop(UploadJournal::open(&root, "owned", 1024).expect("idle journal reopens"));
}
