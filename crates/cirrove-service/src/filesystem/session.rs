//! Owned Linux FUSE lifecycle. Lazy unmount alone does not disconnect clients
//! retaining open files or directories, so joining its request loop can hang.
use cirrove_core::CancellationToken;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
    thread::JoinHandle,
};

pub struct CloudSession {
    guard: Option<JoinHandle<io::Result<()>>>,
    unmounter: fuser::SessionUnmounter,
    disconnect: File,
    cancel: CancellationToken,
}

impl CloudSession {
    pub(super) fn start(
        mut session: fuser::Session<super::CloudFs>,
        path: &Path,
        source: &str,
        cancel: CancellationToken,
    ) -> io::Result<Self> {
        // Retain an open control descriptor for this connection's lifetime. Never
        // discover a connection by a reused path/account ID during shutdown.
        let disconnect = connection_control(session.as_fd(), path, source)?;
        let unmounter = session.unmount_callable();
        let guard = std::thread::Builder::new()
            .name("cirrove-fuse".into())
            .spawn(move || session.run())?;
        Ok(Self {
            guard: Some(guard),
            unmounter,
            disconnect,
            cancel,
        })
    }

    fn detach(&mut self) -> io::Result<()> {
        self.cancel.cancel();
        // Take the mount out of fuser's shared unmount guard BEFORE disconnecting
        // it. This also prevents a second unmount when its request loop exits.
        let unmounted = self.unmounter.unmount();
        let disconnected = self.disconnect.write_all(b"1");
        unmounted.and(disconnected)
    }

    /// Call on a blocking worker, after the account's provider workers stop.
    /// These mounts are read-only; writable mounts must first seal/drain edits.
    pub fn umount_and_join(mut self) -> io::Result<()> {
        self.detach()?;
        self.guard
            .take()
            .ok_or_else(|| io::Error::other("FUSE session already joined"))?
            .join()
            .map_err(|_| io::Error::other("FUSE session thread panicked"))?
    }

    /// Also disconnect a lazily ejected mount whose application handles survive.
    pub fn join(self) -> io::Result<()> {
        self.umount_and_join()
    }
}

impl Drop for CloudSession {
    fn drop(&mut self) {
        if self.guard.is_some() && self.detach().is_err() {
            tracing::warn!("Cirrove FUSE connection could not be closed");
        }
    }
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Cirrove requires an accessible FUSE control filesystem at /sys/fs/fuse/connections",
    )
}

fn connection_control(fd: BorrowedFd<'_>, path: &Path, source: &str) -> io::Result<File> {
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
    if !mounts.lines().any(|line| {
        line.split_whitespace().nth(4) == Some("/sys/fs/fuse/connections")
            && line
                .split_once(" - ")
                .is_some_and(|(_, fs)| fs.starts_with("fusectl "))
    }) {
        return Err(unsupported());
    }
    let mut info = String::new();
    File::open(format!("/proc/self/fdinfo/{}", fd.as_raw_fd()))?
        .take(65537)
        .read_to_string(&mut info)?;
    if info.len() > 65536 {
        return Err(unsupported());
    }
    let id = match info
        .lines()
        .find_map(|line| line.strip_prefix("fuse_connection:"))
    {
        Some(id) => id.trim().parse::<u32>().map_err(|_| unsupported())?,
        // Older kernels expose the connection as this mount's anonymous device
        // minor number. Capture it now, while our live session owns its mount.
        None => mount_connection(&mounts, path, source).ok_or_else(unsupported)?,
    };
    if id == 0 {
        return Err(unsupported());
    }
    let root = Path::new("/sys/fs/fuse/connections");
    let control = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(id.to_string()).join("abort"))
        .map_err(|_| unsupported())?;
    let meta = control.metadata()?;
    if !meta.is_file()
        || meta.uid() != std::fs::metadata("/proc/self")?.uid()
        || meta.dev() != std::fs::metadata(root)?.dev()
    {
        return Err(unsupported());
    }
    Ok(control)
}

fn mount_connection(mounts: &str, path: &Path, source: &str) -> Option<u32> {
    let path = path
        .to_string_lossy()
        .replace('\\', "\\134")
        .replace(' ', "\\040")
        .replace('\t', "\\011")
        .replace('\n', "\\012");
    let mut found = None;
    for line in mounts.lines() {
        let (fields, filesystem) = line.split_once(" - ")?;
        let fields: Vec<_> = fields.split_whitespace().collect();
        if fields.get(4).copied() != Some(path.as_str()) {
            continue;
        }
        let mut filesystem = filesystem.split_whitespace();
        if found.is_some()
            || filesystem.next() != Some("fuse.cirrove")
            || filesystem.next() != Some(source)
        {
            return None;
        }
        let (major, minor) = fields.get(2)?.split_once(':')?;
        if major != "0" {
            return None;
        }
        found = Some(minor.parse().ok()?);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn older_kernel_identity_requires_the_exact_unique_owned_mount() {
        let owned = "1 0 0:91 / /tmp/Cloud\\040drive ro - fuse.cirrove cirrove:a ro\n";
        let path = Path::new("/tmp/Cloud drive");
        assert_eq!(mount_connection(owned, path, "cirrove:a"), Some(91));
        assert_eq!(mount_connection(owned, path, "cirrove:b"), None);
        assert_eq!(
            mount_connection(owned, Path::new("/tmp/elsewhere"), "cirrove:a"),
            None
        );
        assert_eq!(mount_connection(&owned.repeat(2), path, "cirrove:a"), None);
        assert_eq!(
            mount_connection(
                &owned.replace("fuse.cirrove", "fuse.rclone"),
                path,
                "cirrove:a"
            ),
            None
        );
    }
}
