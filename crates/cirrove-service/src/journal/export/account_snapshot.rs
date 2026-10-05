//! Feature-only opaque snapshot of one explicitly owned, stopped fixture account.
//! No SQLite, vault, ZIP, provider, migration or restoration API is called.
use super::*;
use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    max_depth: u32,
    max_entries: u32,
    max_file_bytes: u64,
    max_total_bytes: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    run: Uuid,
    account: Uuid,
    root: PathBuf,
    accounts_sha256: String,
    quiescence_sha256: String,
    limits: Limits,
    parent_manifest_sha256: Option<String>,
    deadline_unix: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Producer {
    pid: u32,
    start_ticks: String,
    exe_sha256: String,
    argv_sha256: String,
    exit_code: Option<i32>,
    signal: Option<i32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Quiescence {
    version: u32,
    run: Uuid,
    mount_namespace: String,
    producers: Vec<Producer>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Stamp {
    dev: u64,
    ino: u64,
    uid: u32,
    mode: u32,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
    nlink: u64,
}
impl Stamp {
    fn of(file: &File) -> Result<Self> {
        let m = file.metadata()?;
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            uid: m.uid(),
            mode: m.mode(),
            len: m.len(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_nsec: m.ctime_nsec(),
            nlink: m.nlink(),
        })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryProof {
    path: PathBuf,
    directory: bool,
    source: Stamp,
    sha256: Option<String>,
}
struct Entry {
    proof: EntryProof,
    file: File,
}
struct Pinned {
    file: File,
    stamp: Stamp,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    state: String,
    run: Uuid,
    account: Uuid,
    restorability_verified: bool,
    storage_formats_verified: bool,
    registration_sha256: String,
    accounts_sha256: String,
    quiescence_sha256: String,
    parent_manifest_sha256: Option<String>,
    total_bytes: u64,
    entries: Vec<EntryProof>,
}
fn hash_shape(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| JournalError::Stale)
}
fn proc_bytes(path: &str, cap: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(std::io::Error::other("local observation limit"));
    }
    Ok(bytes)
}
fn alias(dir: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()))
}
fn safe_dir(file: &File) -> Result<()> {
    let m = file.metadata()?;
    let stats = rustix::fs::fstatfs(file).map_err(std::io::Error::from)?;
    if !m.is_dir()
        || m.uid() != std::fs::metadata("/proc/self")?.uid()
        || m.permissions().mode() & 0o077 != 0
        || stats.f_type == libc::FUSE_SUPER_MAGIC
        || stats.f_type == libc::TMPFS_MAGIC
        || stats.f_type as u64 == 0x858458f6
    {
        return Err(JournalError::Storage);
    }
    Ok(())
}
fn existing(dir: &File, name: &std::ffi::OsStr, is_dir: bool) -> Result<File> {
    let mut flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NONBLOCK;
    if is_dir {
        flags |= rustix::fs::OFlags::DIRECTORY;
    }
    let fd = rustix::fs::openat(dir, name, flags, rustix::fs::Mode::empty())
        .map_err(std::io::Error::from)?;
    let file = File::from(fd);
    if is_dir {
        safe_dir(&file)?;
    } else {
        owned_private(&file)?;
        if file.metadata()?.nlink() != 1 {
            return Err(JournalError::Storage);
        }
    }
    Ok(file)
}
// Opaque account data may be readable by others (SQLite creates 0644 files),
// but every ancestor remains UID-owned and private. Control inputs and lock
// admission continue to use existing() and its stricter owned_private guard.
fn existing_data(dir: &File, name: &std::ffi::OsStr) -> Result<File> {
    safe_dir(dir)?;
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NONBLOCK;
    let fd = rustix::fs::openat(dir, name, flags, rustix::fs::Mode::empty())
        .map_err(std::io::Error::from)?;
    let file = File::from(fd);
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != std::fs::metadata("/proc/self")?.uid()
        || meta.nlink() != 1
        || meta.permissions().mode() & 0o7033 != 0
    {
        return Err(JournalError::Storage);
    }
    Ok(file)
}
fn digest(file: &File, size: u64) -> Result<String> {
    let before = Stamp::of(file)?;
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut total = 0u64;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let remaining = size
            .saturating_sub(total)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let n = reader.read(&mut buffer[..remaining])?;
        if n == 0 {
            break;
        }
        total = total.checked_add(n as u64).ok_or(JournalError::Corrupt)?;
        if total > size {
            return Err(JournalError::Stale);
        }
        hash.update(&buffer[..n]);
    }
    if total != size || Stamp::of(file)? != before {
        return Err(JournalError::Stale);
    }
    Ok(hex::encode(hash.finalize()))
}
fn pin(dir: &File, name: &str, max: u64, expected: &str) -> Result<Pinned> {
    if !hash_shape(expected) {
        return Err(JournalError::Intent);
    }
    let file = existing(dir, name.as_ref(), false)?;
    let stamp = Stamp::of(&file)?;
    if stamp.len > max {
        return Err(JournalError::Storage);
    }
    let sha256 = digest(&file, stamp.len)?;
    if sha256 != expected {
        return Err(JournalError::Stale);
    }
    Ok(Pinned {
        file,
        stamp,
        sha256,
    })
}
fn pinned_bytes(p: &Pinned, cap: u64) -> Result<Vec<u8>> {
    let mut reader = p.file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    reader.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != p.stamp.len
        || bytes.len() as u64 > cap
        || Stamp::of(&p.file)? != p.stamp
        || hex::encode(Sha256::digest(&bytes)) != p.sha256
    {
        return Err(JournalError::Stale);
    }
    Ok(bytes)
}
fn unchanged(p: &Pinned) -> Result<()> {
    if Stamp::of(&p.file)? != p.stamp || digest(&p.file, p.stamp.len)? != p.sha256 {
        return Err(JournalError::Stale);
    }
    Ok(())
}
fn lock(dir: &File, name: &str) -> Result<Arc<JournalOwner>> {
    let file = existing(dir, name.as_ref(), false)?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            JournalError::Busy
        } else {
            JournalError::Storage
        }
    })?;
    Ok(JournalOwner::acquired(file))
}
fn quiescent(reg: &Registration, proof: &Quiescence) -> Result<()> {
    if proof.version != 1
        || proof.run != reg.run
        || proof.producers.is_empty()
        || proof.producers.len() > 32
        || proof.mount_namespace != std::fs::read_link("/proc/self/ns/mnt")?.to_string_lossy()
        || now()? > reg.deadline_unix
    {
        return Err(JournalError::Stale);
    }
    let mut seen = BTreeSet::new();
    for p in &proof.producers {
        if p.pid == 0
            || !seen.insert((p.pid, p.start_ticks.clone()))
            || p.start_ticks.is_empty()
            || p.start_ticks.len() > 24
            || !p.start_ticks.bytes().all(|b| b.is_ascii_digit())
            || !hash_shape(&p.exe_sha256)
            || !hash_shape(&p.argv_sha256)
            || !matches!(
                (p.exit_code, p.signal),
                (Some(0..=255), None) | (None, Some(1..=64))
            )
        {
            return Err(JournalError::Intent);
        }
        match proc_bytes(&format!("/proc/{}/stat", p.pid), 65536) {
            Ok(bytes) => {
                let text = std::str::from_utf8(&bytes).map_err(|_| JournalError::Storage)?;
                let tail = text.rsplit_once(") ").ok_or(JournalError::Storage)?.1;
                let start = tail
                    .split_whitespace()
                    .nth(19)
                    .ok_or(JournalError::Storage)?;
                if start == p.start_ticks {
                    return Err(JournalError::Busy);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    unmounted(&reg.root)
}
fn unmounted(root: &Path) -> Result<()> {
    let mounts = proc_bytes("/proc/self/mountinfo", 4 * 1024 * 1024)?;
    let mounts = std::str::from_utf8(&mounts).map_err(|_| JournalError::Storage)?;
    let prefix = root.to_str().ok_or(JournalError::Intent)?;
    for row in mounts.lines() {
        let path = row.split_whitespace().nth(4).ok_or(JournalError::Storage)?;
        if path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|s| s.starts_with('/'))
        {
            return Err(JournalError::Busy);
        }
    }
    Ok(())
}

fn children(dir: &File) -> Result<Vec<std::ffi::OsString>> {
    let mut names = std::fs::read_dir(alias(dir))?
        .map(|entry| entry.map(|e| e.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}
fn inventory(
    dir: File,
    relative: PathBuf,
    limits: &Limits,
    entries: &mut Vec<Entry>,
    total: &mut u64,
    seen: &mut BTreeSet<(u64, u64)>,
) -> Result<()> {
    safe_dir(&dir)?;
    if relative.components().count() as u32 > limits.max_depth
        || entries.len() >= limits.max_entries as usize
    {
        return Err(JournalError::Quota);
    }
    let stamp = Stamp::of(&dir)?;
    if !seen.insert((stamp.dev, stamp.ino)) {
        return Err(JournalError::Storage);
    }
    let root_dev = entries.first().map_or(stamp.dev, |e| e.proof.source.dev);
    if stamp.dev != root_dev {
        return Err(JournalError::Storage);
    }
    entries.push(Entry {
        proof: EntryProof {
            path: relative.clone(),
            directory: true,
            source: stamp,
            sha256: None,
        },
        file: dir,
    });
    let index = entries.len() - 1;
    for name in children(&entries[index].file)? {
        if name.to_str().is_none() {
            return Err(JournalError::Storage);
        }
        let path = relative.join(&name);
        if entries.len() >= limits.max_entries as usize
            || path.components().count() as u32 > limits.max_depth
        {
            return Err(JournalError::Quota);
        }
        let meta = std::fs::symlink_metadata(alias(&entries[index].file).join(&name))?;
        if meta.is_dir() {
            let child = existing(&entries[index].file, &name, true)?;
            inventory(child, path, limits, entries, total, seen)?;
        } else {
            if !meta.is_file() {
                return Err(JournalError::Storage);
            }
            // No file name/type whitelist: retained ciphertext, temporary and unknown files survive.
            let file = existing_data(&entries[index].file, &name)?;
            let stamp = Stamp::of(&file)?;
            if stamp.dev != root_dev
                || !seen.insert((stamp.dev, stamp.ino))
                || stamp.len > limits.max_file_bytes
            {
                return Err(JournalError::Storage);
            }
            *total = total.checked_add(stamp.len).ok_or(JournalError::Quota)?;
            if *total > limits.max_total_bytes {
                return Err(JournalError::Quota);
            }
            let hash = digest(&file, stamp.len)?;
            entries.push(Entry {
                proof: EntryProof {
                    path,
                    directory: false,
                    source: stamp,
                    sha256: Some(hash),
                },
                file,
            });
        }
    }
    Ok(())
}
fn validate_inventory(entries: &[Entry]) -> Result<()> {
    for e in entries {
        if Stamp::of(&e.file)? != e.proof.source {
            return Err(JournalError::Stale);
        }
        if e.proof.directory {
            let mut expected = entries
                .iter()
                .filter(|v| {
                    v.proof.path != e.proof.path
                        && v.proof.path.parent() == Some(e.proof.path.as_path())
                })
                .map(|v| {
                    v.proof
                        .path
                        .file_name()
                        .map(|n| n.to_os_string())
                        .ok_or(JournalError::Corrupt)
                })
                .collect::<Result<Vec<_>>>()?;
            expected.sort();
            if children(&e.file)? != expected {
                return Err(JournalError::Stale);
            }
        } else if digest(&e.file, e.proof.source.len)?
            != e.proof.sha256.as_deref().ok_or(JournalError::Corrupt)?
        {
            return Err(JournalError::Stale);
        }
    }
    Ok(())
}
fn create_dir(parent: &File, name: &std::ffi::OsStr) -> Result<File> {
    rustix::fs::mkdirat(parent, name, rustix::fs::Mode::from_raw_mode(0o700))
        .map_err(std::io::Error::from)?;
    let dir = existing(parent, name, true)?;
    parent.sync_all()?;
    Ok(dir)
}
fn copy_into(
    file: &File,
    stamp: &Stamp,
    hash: &str,
    source_root: &Path,
    parent: &File,
    destination: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let prepared = prepare_local_copy(
        reader,
        source_root,
        stamp.len,
        Some(hash),
        destination,
        cancel,
        |_| {},
    )?;
    let actual = Stamp::of(&prepared.directory)?;
    let expected = Stamp::of(parent)?;
    if actual.dev != expected.dev || actual.ino != expected.ino {
        return Err(JournalError::Stale);
    }
    prepared.publish(cancel)?;
    let name = destination.file_name().ok_or(JournalError::Intent)?;
    let copied = existing_data(parent, name)?;
    copied.set_permissions(std::fs::Permissions::from_mode(stamp.mode & 0o777))?;
    copied.sync_all()?;
    parent.sync_all()?;
    Ok(())
}
/// Only this strict owned-fixture dispatch can reach the byte-copy primitive.
/// Run synchronously; if called through spawn_blocking, this closure owns all leases.
pub fn owned_account_snapshot(
    registration: &Path,
    registration_sha256: &str,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    let parent = registration.parent().ok_or(JournalError::Intent)?;
    let selected_run = parent
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("cirrove-owned-snapshot-"))
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(JournalError::Intent)?;
    if parent != Path::new(&format!("/var/tmp/cirrove-owned-snapshot-{selected_run}"))
        || registration.file_name() != Some(std::ffi::OsStr::new("registration.json"))
    {
        return Err(JournalError::Intent);
    }
    // Reject any mounted alias before opening even the registered private input.
    unmounted(parent)?;
    let root = directory(parent)?;
    safe_dir(&root)?;
    let pinned_reg = pin(&root, "registration.json", 65536, registration_sha256)?;
    let reg: Registration = serde_json::from_slice(&pinned_bytes(&pinned_reg, 65536)?)?;
    if reg.version != 1
        || reg.run != selected_run
        || reg.root.as_path() != Path::new(&format!("/var/tmp/cirrove-owned-snapshot-{}", reg.run))
        || parent != reg.root.as_path()
        || reg.limits.max_depth == 0
        || reg.limits.max_depth > 32
        || reg.limits.max_entries == 0
        || reg.limits.max_entries > 512
        || reg.limits.max_file_bytes == 0
        || reg.limits.max_file_bytes > 64 * 1024 * 1024
        || reg.limits.max_total_bytes == 0
        || reg.limits.max_total_bytes > 1024 * 1024 * 1024
        || reg.deadline_unix < now()?
        || reg.deadline_unix > now()?.saturating_add(900)
    {
        return Err(JournalError::Intent);
    }
    let pinned_quiescence = pin(&root, "quiescence.json", 65536, &reg.quiescence_sha256)?;
    let proof: Quiescence = serde_json::from_slice(&pinned_bytes(&pinned_quiescence, 65536)?)?;
    quiescent(&reg, &proof)?;
    let state = existing(&root, "state".as_ref(), true)?;
    let mut leases = Vec::new();
    leases.push(lock(&state, "daemon.lock")?);
    leases.push(lock(&state, "settings.lock")?);
    leases.push(lock(&state, &format!("operation-{}.lock", reg.account))?);
    let pinned_accounts = pin(&state, "accounts.json", 1024 * 1024, &reg.accounts_sha256)?;
    let context: serde_json::Value =
        serde_json::from_slice(&pinned_bytes(&pinned_accounts, 1024 * 1024)?)?;
    let accounts = context
        .get("accounts")
        .and_then(|v| v.as_array())
        .ok_or(JournalError::Intent)?;
    if context.get("version").and_then(|v| v.as_u64()) != Some(2)
        || accounts.len() != 1
        || accounts[0].get("id").and_then(|v| v.as_str()) != Some(reg.account.to_string().as_str())
    {
        return Err(JournalError::Account);
    }
    let accounts_dir = existing(&state, "accounts".as_ref(), true)?;
    let account = existing(&accounts_dir, reg.account.to_string().as_ref(), true)?;
    leases.push(lock(&account, "owner.lock")?);
    let journal = existing(&account, "journal".as_ref(), true)?;
    leases.push(lock(&journal, "owner.lock")?);
    let lease_stamps = leases
        .iter()
        .map(|l| Stamp::of(l.file()))
        .collect::<Result<Vec<_>>>()?;
    let root_stamp = Stamp::of(&root)?;
    let state_stamp = Stamp::of(&state)?;
    let accounts_stamp = Stamp::of(&accounts_dir)?;
    let mut entries = Vec::new();
    let mut total = 0;
    let mut seen = BTreeSet::new();
    inventory(
        account,
        PathBuf::new(),
        &reg.limits,
        &mut entries,
        &mut total,
        &mut seen,
    )?;
    let parent_manifest = match reg.parent_manifest_sha256.as_deref() {
        Some(hash) => {
            let p = pin(&root, "parent-manifest.json", 1024 * 1024, hash)?;
            let m: Manifest = serde_json::from_slice(&pinned_bytes(&p, 1024 * 1024)?)?;
            if m.version != 1
                || m.account != reg.account
                || m.state != "byte_snapshot_complete"
                || m.restorability_verified
            {
                return Err(JournalError::Intent);
            }
            Some(p)
        }
        None => None,
    };
    quiescent(&reg, &proof)?;
    validate_inventory(&entries)?;
    let output = create_dir(&root, "snapshot".as_ref())?;
    let tree_parent = create_dir(&output, "account".as_ref())?;
    let copied_account = create_dir(&tree_parent, reg.account.to_string().as_ref())?;
    let context_dir = create_dir(&output, "context".as_ref())?;
    let source_path = reg
        .root
        .join("state/accounts")
        .join(reg.account.to_string());
    let mut destination_dirs = std::collections::BTreeMap::new();
    destination_dirs.insert(PathBuf::new(), copied_account);
    for e in &entries {
        if e.proof.path.as_os_str().is_empty() {
            continue;
        }
        let parent = e.proof.path.parent().ok_or(JournalError::Corrupt)?;
        let dest_parent = destination_dirs.get(parent).ok_or(JournalError::Corrupt)?;
        let name = e.proof.path.file_name().ok_or(JournalError::Corrupt)?;
        if e.proof.directory {
            let d = create_dir(dest_parent, name)?;
            destination_dirs.insert(e.proof.path.clone(), d);
        } else {
            let dest = reg
                .root
                .join("snapshot/account")
                .join(reg.account.to_string())
                .join(&e.proof.path);
            copy_into(
                &e.file,
                &e.proof.source,
                e.proof.sha256.as_deref().ok_or(JournalError::Corrupt)?,
                &source_path,
                dest_parent,
                &dest,
                cancel,
            )?;
        }
    }
    copy_into(
        &pinned_accounts.file,
        &pinned_accounts.stamp,
        &pinned_accounts.sha256,
        &source_path,
        &context_dir,
        &reg.root.join("snapshot/context/accounts.json"),
        cancel,
    )?;
    quiescent(&reg, &proof)?;
    validate_inventory(&entries)?;
    unchanged(&pinned_reg)?;
    unchanged(&pinned_quiescence)?;
    unchanged(&pinned_accounts)?;
    if let Some(p) = &parent_manifest {
        unchanged(p)?;
    }
    if Stamp::of(&state)? != state_stamp || Stamp::of(&accounts_dir)? != accounts_stamp {
        return Err(JournalError::Stale);
    }
    // Creating snapshot changes root directory stamps, so compare identity/mode only.
    let after_root = Stamp::of(&root)?;
    if after_root.dev != root_stamp.dev
        || after_root.ino != root_stamp.ino
        || after_root.uid != root_stamp.uid
        || after_root.mode != root_stamp.mode
    {
        return Err(JournalError::Stale);
    }
    if Stamp::of(&directory(&reg.root.join("state"))?)? != state_stamp
        || Stamp::of(&directory(&source_path)?)? != entries[0].proof.source
    {
        return Err(JournalError::Stale);
    }
    for (lease, stamp) in leases.iter().zip(&lease_stamps) {
        if Stamp::of(lease.file())? != *stamp {
            return Err(JournalError::Stale);
        }
    }
    let current_root = directory(&reg.root)?;
    let identity = Stamp::of(&current_root)?;
    if identity.dev != root_stamp.dev || identity.ino != root_stamp.ino {
        return Err(JournalError::Stale);
    }
    for (dir, name, p) in [
        (&root, "registration.json", &pinned_reg),
        (&root, "quiescence.json", &pinned_quiescence),
        (&state, "accounts.json", &pinned_accounts),
    ] {
        if Stamp::of(&existing(dir, name.as_ref(), false)?)? != p.stamp {
            return Err(JournalError::Stale);
        }
    }
    if let Some(p) = &parent_manifest
        && Stamp::of(&existing(&root, "parent-manifest.json".as_ref(), false)?)? != p.stamp
    {
        return Err(JournalError::Stale);
    }
    let operation_name = format!("operation-{}.lock", reg.account);
    let current_account = directory(&source_path)?;
    for (i, (dir, name)) in [
        (&state, "daemon.lock"),
        (&state, "settings.lock"),
        (&state, operation_name.as_str()),
        (&current_account, "owner.lock"),
        (&journal, "owner.lock"),
    ]
    .into_iter()
    .enumerate()
    {
        if Stamp::of(&existing(dir, name.as_ref(), false)?)? != lease_stamps[i] {
            return Err(JournalError::Stale);
        }
    }
    for e in entries.iter().rev().filter(|e| e.proof.directory) {
        let d = destination_dirs
            .get(&e.proof.path)
            .ok_or(JournalError::Corrupt)?;
        d.set_permissions(std::fs::Permissions::from_mode(e.proof.source.mode & 0o777))?;
        d.sync_all()?;
    }
    context_dir.sync_all()?;
    tree_parent.sync_all()?;
    output.sync_all()?;
    let copied_path = reg
        .root
        .join("snapshot/account")
        .join(reg.account.to_string());
    let copied_root = directory(&copied_path)?;
    let expected_root = destination_dirs
        .get(Path::new(""))
        .ok_or(JournalError::Corrupt)?;
    let copied_identity = Stamp::of(&copied_root)?;
    let expected_identity = Stamp::of(expected_root)?;
    if copied_identity.dev != expected_identity.dev || copied_identity.ino != expected_identity.ino
    {
        return Err(JournalError::Stale);
    }
    let mut copied_entries = Vec::new();
    let mut copied_total = 0;
    let mut copied_seen = BTreeSet::new();
    inventory(
        copied_root,
        PathBuf::new(),
        &reg.limits,
        &mut copied_entries,
        &mut copied_total,
        &mut copied_seen,
    )?;
    if copied_total != total || copied_entries.len() != entries.len() {
        return Err(JournalError::Corrupt);
    }
    for (a, b) in entries.iter().zip(&copied_entries) {
        if a.proof.path != b.proof.path
            || a.proof.directory != b.proof.directory
            || a.proof.sha256 != b.proof.sha256
            || a.proof.source.mode & 0o777 != b.proof.source.mode & 0o777
            || (!a.proof.directory && a.proof.source.len != b.proof.source.len)
        {
            return Err(JournalError::Corrupt);
        }
    }
    let copied_context = pin(
        &context_dir,
        "accounts.json",
        1024 * 1024,
        &pinned_accounts.sha256,
    )?;
    unchanged(&copied_context)?;
    let visible_output = existing(&root, "snapshot".as_ref(), true)?;
    let visible = Stamp::of(&visible_output)?;
    let held = Stamp::of(&output)?;
    if visible.dev != held.dev || visible.ino != held.ino {
        return Err(JournalError::Stale);
    }
    quiescent(&reg, &proof)?;
    validate_inventory(&entries)?;
    unchanged(&pinned_reg)?;
    unchanged(&pinned_quiescence)?;
    unchanged(&pinned_accounts)?;
    if cancel.is_cancelled() {
        return Err(JournalError::Stale);
    }
    let manifest = Manifest {
        version: 1,
        state: "byte_snapshot_complete".into(),
        run: reg.run,
        account: reg.account,
        restorability_verified: false,
        storage_formats_verified: false,
        registration_sha256: registration_sha256.into(),
        accounts_sha256: reg.accounts_sha256,
        quiescence_sha256: reg.quiescence_sha256,
        parent_manifest_sha256: reg.parent_manifest_sha256,
        total_bytes: total,
        entries: entries.into_iter().map(|e| e.proof).collect(),
    };
    let manifest_path = alias(&output).join("snapshot-manifest.json");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(manifest_path)?;
    file.write_all(&serde_json::to_vec(&manifest)?)?;
    file.sync_all()?;
    output.sync_all()?;
    root.sync_all()?;
    // leases intentionally remain live through the final durable manifest.
    Ok(reg.root.join("snapshot/snapshot-manifest.json"))
}
