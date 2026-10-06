//! Finite standalone-removal arm. Public admission is real; recovery never admits a new operation.
use super::{Guard, LossMarker, Mode};
use crate::{
    accounts::{Account, Settings},
    engine::Engine,
    icloud_writes::ICloudWriteProvider,
    journal::{MutationRecord, MutationState, RecoveryJournal, UploadRecord, UploadState},
    manager::{Manager, WriteContext},
    mutations::MutationWorker,
};
use anyhow::{Result, ensure};
use cirrove_auth::{AccessMode, AppRegistration, CredentialVault};
use cirrove_core::{
    CancellationToken, Node, NodeKind, Scope,
    mutation::{MutationIntent, MutationReceipt, MutationRequest},
    upload::{PackageSemanticIdentity, PackageSourceLayout, UploadIntent, UploadRepresentation},
};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
const LIMIT: u64 = 64 * 1024 * 1024;
fn record(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    File::open(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("owned record parent missing"))?,
    )?
    .sync_all()?;
    Ok(())
}
pub(super) fn hash(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}
pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(super) fn pin(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
pub(super) fn private(path: &Path, directory: bool, limit: u64) -> Result<File> {
    ensure!(path.is_absolute(), "owned input must be absolute");
    // Descriptor traversal rejects symlinks in ancestors too.
    let parts = path
        .components()
        .skip(1)
        .map(|part| match part {
            std::path::Component::Normal(p) => Ok(p),
            _ => anyhow::bail!("owned input path refused"),
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!parts.is_empty(), "owned input path missing");
    let mut file = File::open("/")?;
    for (index, part) in parts.iter().enumerate() {
        use rustix::fs::{Mode as FsMode, OFlags};
        let last = index + 1 == parts.len();
        let flags = OFlags::RDONLY
            | OFlags::CLOEXEC
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | if !last || directory {
                OFlags::DIRECTORY
            } else {
                OFlags::empty()
            };
        file = File::from(rustix::fs::openat(&file, *part, flags, FsMode::empty())?);
    }
    let metadata = file.metadata()?;
    ensure!(
        metadata.uid() == std::fs::metadata("/proc/self")?.uid()
            && metadata.mode() & 0o077 == 0
            && if directory {
                metadata.is_dir()
            } else {
                metadata.is_file() && metadata.nlink() == 1 && metadata.len() <= limit
            },
        "owned private input refused"
    );
    Ok(file)
}
pub(super) fn bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    private(path, false, limit)?
        .take(limit + 1)
        .read_to_end(&mut result)?;
    ensure!(result.len() as u64 <= limit, "owned input exceeds bound");
    Ok(result)
}
fn file_hash(path: &Path, limit: u64) -> Result<String> {
    let mut file = private(path, false, limit)?;
    let before = file.metadata()?;
    let mut h = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| anyhow::anyhow!("owned length overflow"))?;
        ensure!(size <= before.len(), "owned input grew");
        h.update(&buffer[..n]);
    }
    let after = file.metadata()?;
    ensure!(
        size == before.len()
            && after.len() == before.len()
            && after.dev() == before.dev()
            && after.ino() == before.ino()
            && after.mtime() == before.mtime()
            && after.mtime_nsec() == before.mtime_nsec(),
        "owned input changed during hash"
    );
    Ok(hex::encode(h.finalize()))
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Source {
    pub(super) path: PathBuf,
    pub(super) size: u64,
    pub(super) sha256: String,
    pub(super) source_layout: PackageSourceLayout,
    #[serde(deserialize_with = "required_root")]
    pub(super) root: Option<String>,
    pub(super) semantic: PackageSemanticIdentity,
}
fn required_root<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Deserialize::deserialize(d)
}
impl Source {
    pub(super) fn check(&self, directory: &Path) -> Result<()> {
        ensure!(
            self.path == directory.join("source.numbers")
                && self.source_layout == PackageSourceLayout::FlatNumbers
                && self.root.is_none()
                && self.size > 0
                && self.size <= LIMIT
                && pin(&self.sha256)
                && self.semantic.version == 2
                && self.semantic.validate().is_ok(),
            "explicit flat Numbers source refused"
        );
        let file = private(&self.path, false, LIMIT)?;
        ensure!(
            file.metadata()?.mode() & 0o7777 == 0o400
                && file.metadata()?.len() == self.size
                && file_hash(&self.path, LIMIT)? == self.sha256,
            "owned source bytes changed"
        );
        let proof = cirrove_icloud::package_flat_archive_semantic_identity_v2(
            &file,
            &cirrove_icloud::PackageDownload {
                size: self.size,
                sha256: self.sha256.clone(),
            },
            &CancellationToken::new(),
        )?;
        ensure!(
            proof == self.semantic,
            "owned source full semantic content changed"
        );
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    run: Uuid,
    account: Uuid,
    label: String,
    session_directory: PathBuf,
    tmpdir: PathBuf,
    settings_sha256: String,
    authorization_sha256: String,
    executable_sha256: String,
    started_unix: u64,
    deadline_unix: u64,
    parent_creation: Uuid,
    import: Uuid,
    parent: Node,
    original: Node,
    source: Source,
    preflight_sha256: String,
    recovery: Option<Recovery>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recovery {
    operation: Uuid,
    marker_sha256: String,
    checkpoint_cipher_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Authorization {
    version: u32,
    run: Uuid,
    account: Uuid,
    root: PathBuf,
    scope: String,
    deadline_unix: u64,
    standing_user_write_authorization: bool,
}
impl Registration {
    fn scope(&self) -> Scope {
        Scope {
            account: self.account.to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        }
    }
    fn request(&self) -> MutationRequest {
        MutationRequest {
            scope: self.scope(),
            intent: MutationIntent::TrashNativeDocument {
                before: self.original.clone(),
            },
        }
    }
    fn journal_path(&self) -> PathBuf {
        self.session_directory
            .join("state/accounts")
            .join(self.account.to_string())
            .join("journal")
    }
    fn validate(&self, path: &Path) -> Result<()> {
        let phase = if self.recovery.is_some() {
            "recovery"
        } else {
            "loss"
        };
        ensure!(
            self.version == 1
                && !self.run.is_nil()
                && !self.account.is_nil()
                && self.label == "iCloudNativeTrashLossValidation"
                && self.session_directory == format!("/var/tmp/cirrove-native-trash-{}", self.run)
                && path
                    == self
                        .session_directory
                        .join(format!("{phase}-registration.json"))
                && self.started_unix <= now()
                && now() < self.deadline_unix
                && self
                    .deadline_unix
                    .checked_sub(self.started_unix)
                    .is_some_and(|v| v > 0 && v <= 600)
                && self.parent_creation != self.import
                && !self.parent_creation.is_nil()
                && !self.import.is_nil(),
            "owned finite removal registration refused"
        );
        for digest in [
            &self.settings_sha256,
            &self.authorization_sha256,
            &self.executable_sha256,
            &self.preflight_sha256,
        ] {
            ensure!(pin(digest), "owned removal pin refused");
        }
        ensure!(
            self.parent.id.starts_with("FOLDER::com.apple.CloudDocs::")
                && self.parent.parent_id.as_deref() == Some(cirrove_icloud::ROOT_ID)
                && self.parent.name == format!("Cirrove-Native-Trash-{}", self.run)
                && self.parent.kind == NodeKind::Folder
                && !self.parent.package
                && self.parent.target.is_none()
                && self.original.parent_id.as_ref() == Some(&self.parent.id)
                && self.original.name == format!("Cirrove-Numbers-Trash-{}.numbers", self.run)
                && self.original.id.starts_with("FILE::com.apple.CloudDocs::")
                && self.original.kind == NodeKind::Folder
                && self.original.package
                && self.original.target.is_none()
                && self.original.content_version.is_none(),
            "fresh owned Numbers identities refused"
        );
        self.request().validate()?;
        ensure!(
            self.source.path == self.session_directory.join("source.numbers")
                && self.source.source_layout == PackageSourceLayout::FlatNumbers
                && self.source.root.is_none()
                && self.source.semantic.version == 2,
            "owned source layout refused"
        );
        if let Some(recovery) = &self.recovery {
            ensure!(
                !recovery.operation.is_nil()
                    && recovery.operation != self.import
                    && recovery.operation != self.parent_creation
                    && pin(&recovery.marker_sha256)
                    && pin(&recovery.checkpoint_cipher_sha256),
                "same-operation recovery binding refused"
            );
        }
        Ok(())
    }
}
fn load(path: &Path, digest: &str, recovering: bool) -> Result<(Registration, Account)> {
    ensure!(pin(digest), "registration digest refused");
    let data = bytes(path, 64 * 1024)?;
    ensure!(hash(&data) == digest, "removal registration changed");
    let reg: Registration = serde_json::from_slice(&data)
        .map_err(|_| anyhow::anyhow!("removal registration schema refused"))?;
    reg.validate(path)?;
    ensure!(
        reg.recovery.is_some() == recovering,
        "removal phase refused"
    );
    let root = &reg.session_directory;
    for dir in [
        root.clone(),
        root.join("state"),
        root.join("bin"),
        root.join("mount"),
    ] {
        private(&dir, true, 0)?;
    }
    let auth_data = bytes(&root.join("authorization.json"), 16 * 1024)?;
    ensure!(
        hash(&auth_data) == reg.authorization_sha256,
        "removal authorization changed"
    );
    let auth: Authorization = serde_json::from_slice(&auth_data)
        .map_err(|_| anyhow::anyhow!("removal authorization schema refused"))?;
    ensure!(
        auth.version == 1
            && auth.run == reg.run
            && auth.account == reg.account
            && auth.root == *root
            && auth.scope == "owned-native-trash-confirmation-loss-and-inspection-recovery"
            && auth.deadline_unix == reg.deadline_unix
            && auth.standing_user_write_authorization,
        "owned removal authorization refused"
    );
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    ensure!(
        !mountinfo
            .lines()
            .any(|line| line.split_whitespace().nth(4) == root.join("mount").to_str())
            && matches!(std::fs::symlink_metadata(root.join("control.sock")), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
        "isolated removal mount/control is still present"
    );
    let exe = root.join("bin/native-trash-probe");
    ensure!(
        std::fs::read_link("/proc/self/exe")? == exe
            && file_hash(&exe, 2 * 1024 * 1024 * 1024)? == reg.executable_sha256,
        "removal executable refused"
    );
    let flag = if recovering {
        "--owned-native-trash-recover"
    } else {
        "--owned-native-trash-loss"
    };
    ensure!(
        std::env::args_os().collect::<Vec<_>>()
            == vec![
                exe.into_os_string(),
                flag.into(),
                path.as_os_str().to_owned(),
                digest.into()
            ],
        "exact removal command refused"
    );
    ensure!(
        reg.tmpdir
            == format!(
                "/home/dandiccf/Storage/cirrove-tmp/nt-{}",
                &reg.run.to_string()[..8]
            )
            && std::env::var_os("TMPDIR").as_deref() == Some(reg.tmpdir.as_os_str())
            && std::env::var_os("SQLITE_TMPDIR").as_deref() == Some(reg.tmpdir.as_os_str()),
        "removal disk temporary directory refused"
    );
    let stats = rustix::fs::fstatfs(&private(&reg.tmpdir, true, 0)?)?;
    ensure!(
        stats.f_type != libc::TMPFS_MAGIC
            && stats.f_type != libc::FUSE_SUPER_MAGIC
            && stats.f_type != 0x8584_58f6,
        "removal temporary filesystem refused"
    );
    let settings_data = bytes(&root.join("state/accounts.json"), 256 * 1024)?;
    ensure!(
        hash(&settings_data) == reg.settings_sha256,
        "owned removal settings changed"
    );
    let settings: Settings = serde_json::from_slice(&settings_data)
        .map_err(|_| anyhow::anyhow!("removal settings schema refused"))?;
    ensure!(
        settings.version == 2 && settings.accounts.len() == 1,
        "owned account inventory refused"
    );
    let account = settings
        .accounts
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("owned account missing"))?;
    ensure!(
        account.id == reg.account.to_string()
            && account.label == reg.label
            && account.enabled
            && account.access == AccessMode::ReadWrite
            && matches!(account.registration, AppRegistration::ICloud)
            && account.drive.id == "drive"
            && account.drive.drive_type == "icloud_drive"
            && account.root_id == cirrove_icloud::ROOT_ID
            && account.mount_path == root.join("mount"),
        "removal account scope refused"
    );
    Uuid::parse_str(&account.credential_id)?;
    for dir in [
        root.join("state/accounts"),
        root.join("state/accounts").join(&account.id),
        reg.journal_path(),
        reg.journal_path().join("objects"),
        reg.journal_path().join("working"),
    ] {
        private(&dir, true, 0)?;
    }
    private(
        &root
            .join("state/accounts")
            .join(&account.id)
            .join("icloud-session.sealed"),
        false,
        4 * 1024 * 1024,
    )?;
    reg.source.check(root)?;
    Ok((reg, account))
}
#[derive(Serialize)]
struct Frontier {
    uploads: Vec<UploadRecord>,
    mutations: Vec<MutationRecord>,
    objects: Vec<crate::journal::NamespaceObject>,
    pairs: Vec<(String, String)>,
}
fn frontier(
    reg: &Registration,
    journal: &RecoveryJournal,
    target: Option<Uuid>,
) -> Result<Frontier> {
    let uploads = journal.list(0, 3)?;
    let mutations = journal.native_validation_mutations(0, 3)?;
    let (objects, working, other) = journal.ordinary_validation_inventory()?;
    let auxiliary = journal.native_validation_import_auxiliary_inventory()?;
    let pairs = journal.ordinary_validation_namespace_operations()?;
    ensure!(
        uploads.len() == 1
            && mutations.len() == if target.is_some() { 2 } else { 1 }
            && working.is_empty()
            && journal.native_validation_working_inventory()? == (0, 0)
            && other == (1, 0)
            && auxiliary == (1, 0, if target.is_some() { 3 } else { 2 }),
        "removal complete local inventory changed"
    );
    let folder = mutations
        .iter()
        .find(|row| row.id == reg.parent_creation)
        .ok_or_else(|| anyhow::anyhow!("owned folder operation missing"))?;
    ensure!(
        folder.state == MutationState::Applied
            && folder.attempt.is_none()
            && folder.request.scope == reg.scope()
            && folder.request.intent
                == MutationIntent::CreateFolder {
                    parent: cirrove_icloud::ROOT_ID.into(),
                    name: reg.parent.name.clone()
                }
            && matches!(&folder.receipt, Some(MutationReceipt::Upsert(node)) if node.id == reg.parent.id
            && node.name == reg.parent.name && node.kind == NodeKind::Folder && !node.package
            && node.parent_id.as_deref() == Some(cirrove_icloud::ROOT_ID) && node.target.is_none()),
        "owned parent receipt changed"
    );
    let imported = &uploads[0];
    ensure!(
        imported.id == reg.import
            && imported.scope == reg.scope()
            && imported.state == UploadState::Uploaded
            && imported.attempt.is_none()
            && imported.intent
                == UploadIntent::Create {
                    parent: reg.parent.id.clone(),
                    name: reg.original.name.clone()
                }
            && imported.representation
                == UploadRepresentation::FlatNumbersArchive {
                    semantic: reg.source.semantic.clone()
                }
            && imported.sha256 == reg.source.sha256
            && imported.size == reg.source.size
            && imported.remote.as_ref() == Some(&reg.original)
            && imported.package_completion.as_ref() == Some(&reg.source.semantic)
            && imported.identity_handoff.is_none()
            && imported.base.is_none()
            && imported.working_file.is_none(),
        "owned import authority changed"
    );
    ensure!(
        journal.native_validation_package_publication(reg.import)?
            == crate::journal::PackagePublicationStatus::Present(reg.original.clone()),
        "owned import publication changed"
    );
    let parent = journal
        .ordinary_validation_namespace_for_operation(folder.id)?
        .ok_or_else(|| anyhow::anyhow!("owned parent association missing"))?;
    let remote = parent
        .remote
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("owned parent remote missing"))?;
    let mut expected = reg.parent.clone();
    expected.etag = remote.etag.clone();
    let mut visible = remote.clone();
    visible.id = parent.node.id.clone();
    ensure!(
        objects.len() == 1
            && objects[0].id == parent.id
            && remote == &expected
            && parent.node == visible
            && parent.scope == reg.scope()
            && parent.follows_remote
            && parent.remote_owned
            && !parent.unlinked
            && parent.working_file.is_none()
            && parent.latest.is_none()
            && parent.native_archive.is_none()
            && pairs == vec![(folder.id.to_string(), parent.id.to_string())],
        "owned namespace frontier changed"
    );
    let mut expected_columns = Vec::new();
    for row in &mutations {
        expected_columns.push((
            "mutation".to_owned(),
            row.id.to_string(),
            row.sequence,
            serde_json::to_value(row.state)?
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("mutation state refused"))?
                .to_owned(),
            Some(row.sequence),
            Some(row.state == MutationState::Applied),
        ));
    }
    expected_columns.push((
        "upload".into(),
        imported.id.to_string(),
        imported.sequence,
        "uploaded".into(),
        Some(imported.sequence),
        Some(true),
    ));
    expected_columns.sort_by_key(|row| row.2);
    ensure!(
        journal.ordinary_validation_operation_columns()? == expected_columns,
        "mapped SQL/queue removal frontier changed"
    );
    if let Some(id) = target {
        let (record, _) = journal.native_trash_status(id)?;
        ensure!(
            record.request == reg.request()
                && record.base.is_none()
                && record.working_file.is_none()
                && record.sequence > imported.sequence
                && record.sequence > folder.sequence,
            "exact standalone removal target changed"
        );
    } else {
        ensure!(
            journal.native_validation_incomplete_queue()? == 0,
            "fresh removal frontier is busy"
        );
    }
    Ok(Frontier {
        uploads,
        mutations,
        objects,
        pairs,
    })
}
fn ticks() -> Result<u64> {
    Ok(std::fs::read_to_string("/proc/self/stat")?
        .rsplit_once(')')
        .ok_or_else(|| anyhow::anyhow!("process identity unavailable"))?
        .1
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| anyhow::anyhow!("process ticks unavailable"))?
        .parse()?)
}
fn attempt(reg: &Registration, digest: &str, phase: &str) -> Result<()> {
    record(
        &reg.session_directory.join(format!("{phase}-attempt.json")),
        &serde_json::json!({
            "version":1,"run":reg.run,"pid":std::process::id(),"start_ticks":ticks()?,
            "registration_sha256":digest,"executable_sha256":reg.executable_sha256,
            "argv_sha256":hash(&std::fs::read("/proc/self/cmdline")?),"deadline_unix":reg.deadline_unix,
            "mutation_replay":false
        }),
    )?;
    Ok(())
}
async fn preflight(reg: &Registration, account: &Account) -> Result<()> {
    let path = reg.session_directory.join("preflight-fixture.json");
    let data = bytes(&path, 32 * 1024)?;
    ensure!(
        hash(&data) == reg.preflight_sha256,
        "owned removal preflight changed"
    );
    let v: serde_json::Value = serde_json::from_slice(&data)?;
    ensure!(
        v["run"] == reg.run.to_string()
            && v["account"] == account.id
            && v["session_directory"] == reg.session_directory.to_string_lossy().as_ref()
            && v["settings_sha256"] == reg.settings_sha256
            && v["format"] == "numbers"
            && v["representation"] == "package"
            && v["source"] == reg.source.path.to_string_lossy().as_ref()
            && v["source_size"] == reg.source.size
            && v["source_sha256"] == reg.source.sha256
            && v.get("source_root") == Some(&serde_json::Value::Null)
            && v["semantic"] == serde_json::to_value(&reg.source.semantic)?
            && v["expected_root"] == reg.original.name
            && v["document"]["drivewsid"] == reg.original.id
            && v["document"]["etag"] == reg.original.etag.clone().unwrap_or_default()
            && v["parent"]["drivewsid"] == reg.parent.id,
        "owned independent Numbers readback scope changed"
    );
    super::super::owned_package::icloud_owned_native_trash_fixture_verify(
        &path,
        &reg.preflight_sha256,
    )
    .await?;
    Ok(())
}
async fn loss(path: &Path, digest: &str) -> Result<()> {
    let (reg, account) = load(path, digest, false)?;
    attempt(&reg, digest, "loss")?;
    let read = RecoveryJournal::open(&reg.journal_path(), &account.id)?;
    let before = frontier(&reg, &read, None)?;
    let frozen = serde_json::to_vec(&before)?;
    // Release every journal owner before independent provider I/O.
    drop(read);
    preflight(&reg, &account).await?;
    let read = RecoveryJournal::open(&reg.journal_path(), &account.id)?;
    ensure!(
        serde_json::to_vec(&frontier(&reg, &read, None)?)? == frozen,
        "preflight changed local frontier"
    );
    drop(read);
    // Preflight uses the original finite window, never a freshly reset timer.
    reg.validate(path)?;
    let state = reg.session_directory.join("state");
    let provider_state = state.clone();
    let request = reg.request();
    let directory = reg.session_directory.clone();
    let run = reg.run;
    let expected_account = account.id;
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let deadline = reg.deadline_unix;
    tokio::spawn(async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(deadline.saturating_sub(now()))) => {},
            _ = async { if let Some(signal) = &mut terminate { signal.recv().await; } else { std::future::pending::<()>().await } } => {},
            _ = tokio::signal::ctrl_c() => {}
        }
        shutdown.cancel();
    });
    let (manager, worker) = Manager::start_with_providers(
        state.clone(),
        cancel.clone(),
        Arc::new(move |a| crate::accounts::provider_with_state(a, &provider_state)),
        Some(Arc::new(move |a, context| {
            ensure!(a.id == expected_account, "removal factory account changed");
            Ok(Arc::new(Guard::new(
                Arc::new(ICloudWriteProvider::new(a, context)?),
                request.clone(),
                context.journal(),
                before.uploads.clone(),
                before.mutations.clone(),
                directory.clone(),
                run,
                Mode::Lose,
            )?))
        })),
    );
    let result = crate::serve_managed(
        state.join("metadata.db"),
        reg.session_directory.join("control.sock"),
        cancel.clone(),
        Some(manager),
    )
    .await;
    cancel.cancel();
    worker.await?;
    result?;
    anyhow::bail!("native removal receipt not withheld; inspect retained attempt")
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    operation: Uuid,
    request: MutationRequest,
    account_hash: String,
    semantic: PackageSemanticIdentity,
    phase: String,
}
async fn checkpoint(reg: &Registration, account: &Account, expected: &Recovery) -> Result<()> {
    let dir = reg
        .session_directory
        .join("state/accounts")
        .join(&account.id)
        .join("native-trash-checkpoints")
        .join(expected.operation.to_string());
    private(&dir, true, 0)?;
    ensure!(
        file_hash(&dir.join("checkpoint.sealed"), 4 * 1024 * 1024)?
            == expected.checkpoint_cipher_sha256,
        "sealed native removal checkpoint changed"
    );
    let vault = cirrove_icloud::SealedNativeTrashCheckpointVault::new(
        &reg.session_directory.join("state"),
        &account.id,
    )?;
    let saved = vault
        .load(&format!(
            "icloud-native-trash/{}/{}",
            account.id, expected.operation
        ))
        .await?
        .ok_or_else(|| anyhow::anyhow!("sealed native removal checkpoint unavailable"))?;
    ensure!(
        saved.expose_secret().len() <= 96 * 1024,
        "native removal checkpoint exceeds bound"
    );
    let parsed: Checkpoint = serde_json::from_str(saved.expose_secret())
        .map_err(|_| anyhow::anyhow!("native removal checkpoint schema refused"))?;
    let account_hash = cirrove_icloud::probe_session_key(&account.identity.username)?;
    ensure!(
        parsed.version == 1
            && parsed.operation == expected.operation
            && parsed.request == reg.request()
            && parsed.account_hash
                == account_hash
                    .strip_prefix("icloud-probe-")
                    .unwrap_or_default()
            && parsed.semantic == reg.source.semantic
            && parsed.phase == "may_have_sent",
        "native removal checkpoint authority refused"
    );
    Ok(())
}
async fn recover(path: &Path, digest: &str) -> Result<()> {
    let (reg, account) = load(path, digest, true)?;
    attempt(&reg, digest, "recovery")?;
    let expected = reg
        .recovery
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("recovery binding missing"))?;
    let marker_data = bytes(
        &reg.session_directory
            .join("lost-native-trash-confirmation.json"),
        64 * 1024,
    )?;
    ensure!(
        hash(&marker_data) == expected.marker_sha256,
        "removal loss marker changed"
    );
    let marker: LossMarker = serde_json::from_slice(&marker_data)
        .map_err(|_| anyhow::anyhow!("removal marker schema refused"))?;
    ensure!(
        marker.version == 1
            && marker.run == reg.run
            && marker.operation == expected.operation
            && marker.request == reg.request()
            && !marker.acknowledgement_returned
            && reg.request().accepts(&marker.receipt),
        "same-operation native removal marker refused"
    );
    let loss_attempt: serde_json::Value = serde_json::from_slice(&bytes(
        &reg.session_directory.join("loss-attempt.json"),
        16 * 1024,
    )?)?;
    ensure!(
        loss_attempt["pid"] == marker.pid
            && loss_attempt["run"] == reg.run.to_string()
            && !PathBuf::from(format!("/proc/{}", marker.pid)).try_exists()?,
        "original removal process has not closed"
    );
    let read = RecoveryJournal::open(&reg.journal_path(), &account.id)?;
    let frozen = frontier(&reg, &read, Some(expected.operation))?;
    let (before, absent) = read.native_trash_status(expected.operation)?;
    ensure!(
        before.state == MutationState::Applying
            && before.attempt.is_some()
            && before.receipt.is_none()
            && before.prepared_item.as_deref() == Some(reg.original.id.as_str())
            && Some(marker.prepared_item.as_str()) == before.prepared_item.as_deref()
            && before.verified_content.is_none()
            && !absent
            && read.native_validation_incomplete_queue()? == 1,
        "raw interrupted standalone removal changed"
    );
    drop(read);
    checkpoint(&reg, &account, expected).await?;
    reg.validate(path)?;
    let state = reg.session_directory.join("state");
    let engine = Engine::new(
        account.clone(),
        crate::accounts::provider_with_state(&account, &state)?,
        state.clone(),
    )
    .await?;
    let context = WriteContext::open(&engine, &state).await?;
    let reopened = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("removal journal owner unavailable"))?
        .native_trash_record(expected.operation)?;
    let mut normalized = before.clone();
    normalized.state = MutationState::VerifyRequired;
    normalized.attempt = None;
    ensure!(
        serde_json::to_vec(&reopened)? == serde_json::to_vec(&normalized)?,
        "reopen changed more than interrupted state"
    );
    let baseline_mutations = frozen
        .mutations
        .iter()
        .filter(|row| row.id != expected.operation)
        .cloned()
        .collect();
    let guard = Arc::new(Guard::new(
        Arc::new(ICloudWriteProvider::new(&account, &context)?),
        reg.request(),
        context.journal(),
        frozen.uploads.clone(),
        baseline_mutations,
        reg.session_directory.clone(),
        reg.run,
        Mode::Recover {
            operation: expected.operation,
            receipt: marker.receipt.clone(),
        },
    )?);
    let cancel = CancellationToken::new();
    let worker = MutationWorker::new(context.journal(), guard.clone(), cancel);
    reg.validate(path)?;
    let result = tokio::time::timeout(
        Duration::from_secs(reg.deadline_unix.saturating_sub(now())),
        worker.run_once(),
    )
    .await??
    .ok_or_else(|| anyhow::anyhow!("same removal operation not selected"))?;
    ensure!(
        result.id == expected.operation && result.state == MutationState::Applied,
        "native removal inspection did not finish"
    );
    let counts = guard.counts.value();
    ensure!(
        guard.counts.mutations.load(Ordering::SeqCst) == 0
            && guard.counts.preparations.load(Ordering::SeqCst) == 0
            && guard.counts.refused_mutations.load(Ordering::SeqCst) == 0
            && guard.counts.refused_uploads.load(Ordering::SeqCst) == 0
            && guard.counts.reconciliations.load(Ordering::SeqCst) == 1,
        "native removal recovery used unexpected provider entrypoints"
    );
    drop(worker);
    drop(guard);
    drop(context);
    // Existing metadata-only repair performs its ordered read with no writer alive.
    ensure!(
        engine
            .repair_native_trash_metadata_once_at(reg.journal_path())
            .await?,
        "native removal metadata job unavailable"
    );
    let read = RecoveryJournal::open(&reg.journal_path(), &account.id)?;
    let (after, absent) = read.native_trash_status(expected.operation)?;
    let completed = frontier(&reg, &read, Some(expected.operation))?;
    ensure!(
        after.state == MutationState::Applied
            && after.attempt.is_none()
            && after.receipt.as_ref() == Some(&marker.receipt)
            && after.request == before.request
            && after.prepared_item == before.prepared_item
            && absent
            && read.native_validation_incomplete_queue()? == 0
            && serde_json::to_vec(&completed.uploads)? == serde_json::to_vec(&frozen.uploads)?
            && serde_json::to_vec(&completed.objects)? == serde_json::to_vec(&frozen.objects)?
            && completed.pairs == frozen.pairs
            && completed
                .mutations
                .iter()
                .find(|row| row.id == reg.parent_creation)
                .map(serde_json::to_vec)
                .transpose()?
                == frozen
                    .mutations
                    .iter()
                    .find(|row| row.id == reg.parent_creation)
                    .map(serde_json::to_vec)
                    .transpose()?,
        "native removal completion changed protected frontier"
    );
    drop(read);
    checkpoint(&reg, &account, expected).await?;
    super::verify::verify_removed(
        &state,
        &account,
        &reg.request(),
        &reg.source.semantic,
        &reg.source.path,
        reg.source.size,
        &reg.source.sha256,
        &reg.session_directory,
    )
    .await?;
    reg.source.check(&reg.session_directory)?;
    ensure!(
        file_hash(&state.join("accounts.json"), 256 * 1024)? == reg.settings_sha256
            && file_hash(path, 64 * 1024)? == digest,
        "removal settings/registration changed"
    );
    record(
        &reg.session_directory.join("recovery-completed.json"),
        &serde_json::json!({
            "version":1,"run":reg.run,"operation":expected.operation,"same_operation":true,"typed_receipt":marker.receipt,
            "counts":counts,"metadata_absence_confirmed":true,"full_Trash_semantic_verified":true,
            "original_source_preserved":true,"cloud_mutation_replayed":false,"mounted":false,
            "public_true_readonly_watch_and_fresh_remount_required":true
        }),
    )?;
    engine.stop().await;
    Ok(())
}
pub async fn icloud_native_trash_loss(path: &Path, digest: &str) -> Result<()> {
    loss(path, digest).await.map_err(|_| {
        anyhow::anyhow!("native removal loss validation refused; inspect private attempt")
    })
}
pub async fn icloud_native_trash_recover(path: &Path, digest: &str) -> Result<()> {
    let (reg, _) = load(path, digest, true)
        .map_err(|_| anyhow::anyhow!("native removal recovery registration refused"))?;
    tokio::time::timeout(
        Duration::from_secs(reg.deadline_unix.saturating_sub(now())),
        recover(path, digest),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!("native removal original deadline reached; inspect retained attempt")
    })?
    .map_err(|_| {
        anyhow::anyhow!("native removal inspection-only recovery refused; inspect private attempt")
    })
}
#[cfg(test)]
mod tests;
