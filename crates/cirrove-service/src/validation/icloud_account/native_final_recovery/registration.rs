//! Finite, digest-pinned private inputs. No flag reaches provider I/O before these guards.
use super::*;
use crate::{
    accounts::{Account, Settings},
    engine::Engine,
    filesystem::CloudFs,
    journal::{MutationState, RecoveryJournal},
    manager::{Manager, WriteContext},
    transfers::TransferWorker,
};
use cirrove_auth::{AccessMode, AppRegistration};
use cirrove_core::{
    mutation::{MutationIntent, MutationReceipt},
    upload::{PackageSourceLayout, UploadIntent, UploadRepresentation},
};
use std::{
    io::Read,
    os::unix::fs::MetadataExt,
    time::{SystemTime, UNIX_EPOCH},
};
const LIMIT: u64 = 64 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: PathBuf,
    size: u64,
    sha256: String,
    #[serde(deserialize_with = "required_source_root")]
    root: Option<String>,
    #[serde(default, skip_serializing_if = "wrapped_source")]
    source_layout: PackageSourceLayout,
    semantic: PackageSemanticIdentity,
}
fn required_source_root<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(deserializer)
}
fn wrapped_source(layout: &PackageSourceLayout) -> bool {
    *layout == PackageSourceLayout::Wrapped
}
impl Source {
    fn check_layout(&self) -> Result<()> {
        ensure!(
            matches!(
                (self.source_layout, self.root.as_deref()),
                (PackageSourceLayout::Wrapped, Some("Source.numbers"))
                    | (PackageSourceLayout::FlatNumbers, None)
            ),
            "native source layout refused"
        );
        Ok(())
    }
    fn import_representation(&self) -> UploadRepresentation {
        match self.source_layout {
            PackageSourceLayout::Wrapped => UploadRepresentation::PackageArchive {
                expected_root: self.root.clone().unwrap_or_default(),
                semantic: self.semantic.clone(),
            },
            PackageSourceLayout::FlatNumbers => UploadRepresentation::FlatNumbersArchive {
                semantic: self.semantic.clone(),
            },
            // check_layout rejects this format before this Numbers-only driver runs.
            PackageSourceLayout::FlatPages => UploadRepresentation::FlatPagesArchive {
                semantic: self.semantic.clone(),
            },
        }
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
    source_a: Source,
    source_b: Source,
    preflight_sha256: String,
    recovery: Option<Recovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pause_seconds: Option<u64>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recovery {
    operation: Uuid,
    marker_sha256: String,
    checkpoint_cipher_sha256: String,
}
#[derive(Serialize, Deserialize)]
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
fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn private(path: &Path, directory: bool, max: u64) -> Result<File> {
    let uid = std::fs::metadata("/proc/self")?.uid();
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        !metadata.file_type().is_symlink()
            && metadata.uid() == uid
            && metadata.mode() & 0o077 == 0
            && if directory {
                metadata.is_dir()
            } else {
                metadata.is_file() && metadata.nlink() == 1 && metadata.len() <= max
            },
        "owned private input refused"
    );
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.dev() == metadata.dev() && opened.ino() == metadata.ino(),
        "owned input identity changed"
    );
    Ok(file)
}
fn bytes(path: &Path, max: u64) -> Result<Vec<u8>> {
    let mut value = Vec::new();
    private(path, false, max)?
        .take(max + 1)
        .read_to_end(&mut value)?;
    ensure!(value.len() as u64 <= max, "owned input exceeds bound");
    Ok(value)
}
fn source(source: &Source, root: &Path, name: &str) -> Result<()> {
    source.check_layout()?;
    ensure!(
        source.path == root.join(name)
            && source.size > 0
            && source.size <= LIMIT
            && hex_digest(&source.sha256)
            && source.semantic.version == 2,
        "native source shape refused"
    );
    let mut file = private(&source.path, false, LIMIT)?;
    let size = file.metadata()?.len();
    let raw = bounded_hash(&mut file, LIMIT)?;
    ensure!(
        size == source.size && raw == source.sha256,
        "native source bytes changed"
    );
    let receipt = cirrove_icloud::PackageDownload { size, sha256: raw };
    let proof = match source.source_layout {
        PackageSourceLayout::Wrapped => {
            cirrove_icloud::package_archive_semantic_identity_versioned(
                &file,
                &receipt,
                source
                    .root
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("native source root missing"))?,
                2,
                &CancellationToken::new(),
            )?
        }
        PackageSourceLayout::FlatNumbers => {
            cirrove_icloud::package_flat_archive_semantic_identity_v2(
                &file,
                &receipt,
                &CancellationToken::new(),
            )?
        }
        PackageSourceLayout::FlatPages => anyhow::bail!("native source layout refused"),
    };
    ensure!(proof == source.semantic, "native source v2 content changed");
    Ok(())
}
fn bounded_hash(file: &mut File, max: u64) -> Result<String> {
    let before = file.metadata()?;
    ensure!(before.len() <= max, "native file exceeds bound");
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| anyhow::anyhow!("native input length overflow"))?;
        ensure!(total <= before.len(), "native input grew");
        hash.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    ensure!(
        total == before.len()
            && after.len() == before.len()
            && after.dev() == before.dev()
            && after.ino() == before.ino()
            && after.mtime() == before.mtime()
            && after.mtime_nsec() == before.mtime_nsec(),
        "native input changed during hash"
    );
    Ok(hex::encode(hash.finalize()))
}
fn file_hash(path: &Path, max: u64) -> Result<String> {
    bounded_hash(&mut private(path, false, max)?, max)
}
impl Registration {
    fn scope(&self) -> Scope {
        Scope {
            account: self.account.to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        }
    }
    fn request(&self) -> UploadRequest {
        UploadRequest {
            scope: self.scope(),
            intent: UploadIntent::Replace {
                item: self.original.id.clone(),
                expected_etag: self.original.etag.clone().unwrap_or_default(),
            },
            representation: match self.source_b.source_layout {
                PackageSourceLayout::Wrapped => UploadRepresentation::PackageReplacementArchive {
                    expected_root: self.source_b.root.clone().unwrap_or_default(),
                    semantic: self.source_b.semantic.clone(),
                    original: Box::new(self.original.clone()),
                    original_semantic: self.source_a.semantic.clone(),
                },
                PackageSourceLayout::FlatNumbers => {
                    UploadRepresentation::FlatNumbersReplacementArchive {
                        semantic: self.source_b.semantic.clone(),
                        original: Box::new(self.original.clone()),
                        original_semantic: self.source_a.semantic.clone(),
                    }
                }
                // validate calls the unchanged Numbers-only source layout fence first.
                PackageSourceLayout::FlatPages => {
                    UploadRepresentation::FlatPagesReplacementArchive {
                        semantic: self.source_b.semantic.clone(),
                        original: Box::new(self.original.clone()),
                        original_semantic: self.source_a.semantic.clone(),
                    }
                }
            },
            size: self.source_b.size,
            sha256: self.source_b.sha256.clone(),
        }
    }
    fn validate(&self, path: &Path) -> Result<()> {
        self.source_a.check_layout()?;
        self.source_b.check_layout()?;
        ensure!(
            self.source_a.source_layout == self.source_b.source_layout,
            "native source layouts differ"
        );
        ensure!(
            self.version == 1
                && !self.run.is_nil()
                && !self.account.is_nil()
                && self.label == "iCloudNativeFinalValidation"
                && self.session_directory == format!("/var/tmp/cirrove-native-final-{}", self.run)
                && path
                    == self.session_directory.join(if self.recovery.is_some() {
                        "recovery-registration.json"
                    } else {
                        "loss-registration.json"
                    }),
            "native final registration scope refused"
        );
        ensure!(
            self.parent_creation != self.import
                && !self.parent_creation.is_nil()
                && !self.import.is_nil()
                && self.started_unix <= now()
                && now() < self.deadline_unix
                && self
                    .deadline_unix
                    .checked_sub(self.started_unix)
                    .is_some_and(|v| v > 0 && v <= 1800),
            "native finite window refused"
        );
        for pin in [
            &self.settings_sha256,
            &self.authorization_sha256,
            &self.executable_sha256,
            &self.preflight_sha256,
        ] {
            ensure!(hex_digest(pin), "native pin refused");
        }
        ensure!(
            self.parent.id.starts_with("FOLDER::com.apple.CloudDocs::")
                && self.parent.parent_id.as_deref() == Some(cirrove_icloud::ROOT_ID)
                && self.parent.name == format!("Cirrove-Native-{}", self.run)
                && self.parent.kind == NodeKind::Folder
                && !self.parent.package
                && self.parent.target.is_none()
                && self.original.parent_id.as_ref() == Some(&self.parent.id)
                && self.original.name == format!("Cirrove-Numbers-Parent-{}.numbers", self.run),
            "owned native identities refused"
        );
        self.request().validate()?;
        ensure!(
            self.source_a.sha256 != self.source_b.sha256
                && self.source_a.semantic != self.source_b.semantic,
            "native sources do not differ"
        );
        if let Some(seconds) = self.pause_seconds {
            ensure!(
                self.recovery.is_none()
                    && (1..=120).contains(&seconds)
                    && self
                        .deadline_unix
                        .checked_sub(self.started_unix)
                        .is_some_and(|span| span > 0 && span <= 600)
                    && self
                        .deadline_unix
                        .checked_sub(45)
                        .is_some_and(|end| now() < end),
                "native pre-Trash pause scope refused"
            );
        }
        if let Some(recovery) = &self.recovery {
            ensure!(
                !recovery.operation.is_nil()
                    && recovery.operation != self.import
                    && recovery.operation != self.parent_creation
                    && hex_digest(&recovery.marker_sha256)
                    && hex_digest(&recovery.checkpoint_cipher_sha256),
                "native recovery binding refused"
            );
        }
        Ok(())
    }
}
fn load(path: &Path, digest: &str, recovery: bool) -> Result<(Registration, Account)> {
    ensure!(hex_digest(digest), "native registration pin refused");
    let data = bytes(path, 64 * 1024)?;
    ensure!(hash(&data) == digest, "native registration changed");
    let registration: Registration = serde_json::from_slice(&data)
        .map_err(|_| anyhow::anyhow!("native registration schema refused"))?;
    registration.validate(path)?;
    ensure!(
        registration.recovery.is_some() == recovery,
        "native phase refused"
    );
    let root = &registration.session_directory;
    private(root, true, 0)?;
    private(&root.join("state"), true, 0)?;
    private(&root.join("bin"), true, 0)?;
    private(&root.join("mount"), true, 0)?;
    let authorization_bytes = bytes(&root.join("authorization.json"), 16 * 1024)?;
    ensure!(
        hash(&authorization_bytes) == registration.authorization_sha256,
        "native authorization changed"
    );
    let authorization: Authorization = serde_json::from_slice(&authorization_bytes)
        .map_err(|_| anyhow::anyhow!("native authorization schema refused"))?;
    ensure!(
        authorization.version == 1
            && authorization.run == registration.run
            && authorization.account == registration.account
            && authorization.root == *root
            && authorization.scope
                == "owned-native-package-final-confirmation-loss-and-inspection-recovery"
            && authorization.deadline_unix == registration.deadline_unix
            && authorization.standing_user_write_authorization,
        "native owned authorization refused"
    );
    let mount = root.join("mount");
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    ensure!(
        !mountinfo
            .lines()
            .any(|line| line.split_whitespace().nth(4) == mount.to_str()),
        "native owned mount is still present"
    );
    ensure!(
        matches!(std::fs::symlink_metadata(root.join("control.sock")),Err(error) if error.kind()==std::io::ErrorKind::NotFound),
        "native control socket is still present"
    );
    let executable = root.join("bin/native-final-probe");
    ensure!(
        std::fs::read_link("/proc/self/exe")? == executable
            && file_hash(&executable, 2 * 1024 * 1024 * 1024)? == registration.executable_sha256,
        "native probe binary refused"
    );
    let expected = vec![
        executable.into_os_string(),
        if recovery {
            "--owned-native-final-recover".into()
        } else {
            "--owned-native-final-loss".into()
        },
        path.as_os_str().to_owned(),
        digest.into(),
    ];
    ensure!(
        std::env::args_os().collect::<Vec<_>>() == expected,
        "native exact command refused"
    );
    ensure!(
        registration.tmpdir
            == format!(
                "/home/dandiccf/Storage/cirrove-tmp/nf-{}",
                &registration.run.to_string()[..8]
            )
            && std::env::var_os("TMPDIR").as_deref() == Some(registration.tmpdir.as_os_str())
            && std::env::var_os("SQLITE_TMPDIR").as_deref()
                == Some(registration.tmpdir.as_os_str()),
        "native disk temporary directory refused"
    );
    let tmp = private(&registration.tmpdir, true, 0)?;
    let stats = rustix::fs::fstatfs(&tmp)?;
    ensure!(
        stats.f_type != libc::TMPFS_MAGIC
            && stats.f_type != libc::FUSE_SUPER_MAGIC
            && stats.f_type != 0x8584_58f6,
        "native temporary filesystem refused"
    );
    let settings_data = bytes(&root.join("state/accounts.json"), 256 * 1024)?;
    ensure!(
        hash(&settings_data) == registration.settings_sha256,
        "native settings changed"
    );
    let settings: Settings = serde_json::from_slice(&settings_data)
        .map_err(|_| anyhow::anyhow!("native settings schema refused"))?;
    ensure!(
        settings.version == 2 && settings.accounts.len() == 1,
        "native account inventory refused"
    );
    let account = settings
        .accounts
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("native account absent"))?;
    ensure!(
        account.id == registration.account.to_string()
            && account.label == registration.label
            && account.enabled
            && account.access == AccessMode::ReadWrite
            && matches!(account.registration, AppRegistration::ICloud)
            && account.drive.id == "drive"
            && account.drive.drive_type == "icloud_drive"
            && account.root_id == cirrove_icloud::ROOT_ID
            && account.mount_path == root.join("mount"),
        "native account scope refused"
    );
    Uuid::parse_str(&account.credential_id)?;
    let accounts = root.join("state/accounts");
    private(&accounts, true, 0)?;
    let owned = accounts.join(&account.id);
    private(&owned, true, 0)?;
    private(&owned.join("journal"), true, 0)?;
    private(&owned.join("journal/objects"), true, 0)?;
    private(&owned.join("journal/working"), true, 0)?;
    private(&owned.join("icloud-session.sealed"), false, 4 * 1024 * 1024)?;
    if let Some(recovery) = &registration.recovery {
        private(&owned.join("upload-checkpoints"), true, 0)?;
        private(
            &owned
                .join("upload-checkpoints")
                .join(recovery.operation.to_string()),
            true,
            0,
        )?;
    }
    source(&registration.source_a, root, "source-a.numbers")?;
    source(&registration.source_b, root, "source-b.numbers")?;
    Ok((registration, account))
}
fn journal_path(registration: &Registration) -> PathBuf {
    registration
        .session_directory
        .join("state/accounts")
        .join(registration.account.to_string())
        .join("journal")
}
fn initial(registration: &Registration, journal: &RecoveryJournal) -> Result<Vec<UploadRecord>> {
    let mutations = journal.native_validation_mutations(0, 2)?;
    ensure!(mutations.len() == 1, "native folder inventory changed");
    let folder = &mutations[0];
    ensure!(
        folder.id == registration.parent_creation
            && folder.state == MutationState::Applied
            && folder.attempt.is_none()
            && folder.request.scope == registration.scope()
            && folder.request.intent
                == MutationIntent::CreateFolder {
                    parent: cirrove_icloud::ROOT_ID.into(),
                    name: registration.parent.name.clone()
                }
            && matches!(&folder.receipt,Some(MutationReceipt::Upsert(node)) if node.id==registration.parent.id&&node.name==registration.parent.name&&node.kind==NodeKind::Folder&&!node.package&&node.parent_id.as_deref()==Some(cirrove_icloud::ROOT_ID)&&node.target.is_none()&&node.size==0&&node.content_version.is_none()),
        "native folder receipt changed"
    );
    let rows = journal.list(0, 4)?;
    let imported = rows
        .iter()
        .find(|row| row.id == registration.import)
        .ok_or_else(|| anyhow::anyhow!("native import missing"))?;
    ensure!(
        imported.state == UploadState::Uploaded
            && imported.scope == registration.scope()
            && imported.intent
                == UploadIntent::Create {
                    parent: registration.parent.id.clone(),
                    name: registration.original.name.clone()
                }
            && imported.representation == registration.source_a.import_representation()
            && imported.size == registration.source_a.size
            && imported.sha256 == registration.source_a.sha256
            && imported.remote.as_ref() == Some(&registration.original)
            && imported.package_completion.as_ref() == Some(&registration.source_a.semantic)
            && imported.attempt.is_none()
            && imported.base.is_none()
            && imported.working_file.is_none()
            && imported.identity_handoff.is_none(),
        "native import binding changed"
    );
    ensure!(
        matches!(journal.native_validation_package_publication(imported.id)?,crate::journal::PackagePublicationStatus::Present(node) if node==registration.original),
        "native original publication missing"
    );
    ensure!(
        journal.native_validation_working_inventory()? == (0, 0),
        "native explicit arm has working bytes"
    );
    if registration.recovery.is_none() {
        ensure!(
            rows.len() == 1
                && journal.native_validation_incomplete_queue()? == 0
                && journal.native_validation_import_auxiliary_inventory()? == (1, 0, 2),
            "native fresh frontier changed"
        );
    } else {
        ensure!(rows.len() == 2, "native recovery inventory changed");
    }
    Ok(vec![imported.clone()])
}
fn process_ticks() -> Result<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    Ok(stat
        .rsplit_once(')')
        .ok_or_else(|| anyhow::anyhow!("native process identity unavailable"))?
        .1
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| anyhow::anyhow!("native process ticks unavailable"))?
        .parse()?)
}
fn attempt(registration: &Registration, digest: &str, phase: &str) -> Result<()> {
    record(
        &registration
            .session_directory
            .join(format!("{phase}-attempt.json")),
        &serde_json::json!({"version":1,"run":registration.run,"pid":std::process::id(),"start_ticks":process_ticks()?,"registration_sha256":digest,"executable_sha256":registration.executable_sha256,"argv_sha256":hash(&std::fs::read("/proc/self/cmdline")?),"started_unix":now(),"deadline_unix":registration.deadline_unix,"mutation_replay":false}),
    )
}
async fn loss(path: &Path, digest: &str) -> Result<()> {
    let (registration, account) = load(path, digest, false)?;
    attempt(&registration, digest, "loss")?;
    let recovery = RecoveryJournal::open(&journal_path(&registration), &account.id)?;
    let completed = initial(&registration, &recovery)?;
    let frozen = serde_json::to_vec(&frontier(&registration, &recovery, None)?)?;
    let fixture = registration
        .session_directory
        .join("preflight-fixture.json");
    let fixture_data = bytes(&fixture, 32 * 1024)?;
    ensure!(
        hash(&fixture_data) == registration.preflight_sha256,
        "native original fixture changed"
    );
    let value: serde_json::Value = serde_json::from_slice(&fixture_data)?;
    ensure!(
        value["run"] == registration.run.to_string()
            && value["account"] == account.id
            && value["session_directory"]
                == registration.session_directory.to_string_lossy().as_ref()
            && value["settings_sha256"] == registration.settings_sha256
            && value["format"] == "numbers"
            && value["representation"] == "package"
            && value["source"] == registration.source_a.path.to_string_lossy().as_ref()
            && value["source_size"] == registration.source_a.size
            && value["source_sha256"] == registration.source_a.sha256
            && value.get("source_root")
                == Some(&serde_json::to_value(&registration.source_a.root)?)
            && value["expected_root"] == registration.original.name
            && value["semantic"] == serde_json::to_value(&registration.source_a.semantic)?
            && value["document"]["drivewsid"] == registration.original.id
            && value["document"]["etag"] == registration.original.etag.clone().unwrap_or_default(),
        "native original readback scope refused"
    );
    super::super::icloud_owned_fixture_verify(&fixture, &registration.preflight_sha256).await?;
    ensure!(
        serde_json::to_vec(&completed)? == serde_json::to_vec(&initial(&registration, &recovery)?)?
            && serde_json::to_vec(&frontier(&registration, &recovery, None)?)? == frozen,
        "native original journal changed"
    );
    drop(recovery);
    // Each factory retains this exact operation latch. The manager remains the
    // public admission/socket lifecycle; only its write provider is constrained.
    let state = registration.session_directory.join("state");
    let provider_state = state.clone();
    let request = registration.request();
    let directory = registration.session_directory.clone();
    let run = registration.run;
    let expected_account = account.id.clone();
    let cancel = CancellationToken::new();
    let deadline = registration.deadline_unix;
    let pause_seconds = registration.pause_seconds;
    let shutdown = cancel.clone();
    tokio::spawn(async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {_=tokio::time::sleep(Duration::from_secs(deadline.saturating_sub(now())))=>{},_=async {if let Some(signal)=&mut terminate {signal.recv().await;}else{std::future::pending::<()>().await}}=>{},_=tokio::signal::ctrl_c()=>{}}
        shutdown.cancel();
    });
    let (manager, worker) = Manager::start_with_providers(
        state.clone(),
        cancel.clone(),
        Arc::new(move |a| crate::accounts::provider_with_state(a, &provider_state)),
        Some(Arc::new(move |a, context| {
            ensure!(
                a.id == expected_account,
                "native write factory account changed"
            );
            let guard = Guard::new(
                ICloudWriteProvider::new(a, context)?,
                request.clone(),
                context.journal(),
                completed.clone(),
                directory.clone(),
                run,
                Mode::Lose,
            )?;
            let guard = if let Some(seconds) = pause_seconds {
                // Couple the original epoch deadline to a monotonic instant,
                // retaining fractional wall time and no renewed phase clock.
                let monotonic = tokio::time::Instant::now();
                let wall = SystemTime::now().duration_since(UNIX_EPOCH)?;
                let remaining = deadline
                    .checked_sub(45)
                    .and_then(|end| Duration::from_secs(end).checked_sub(wall))
                    .filter(|remaining| !remaining.is_zero())
                    .ok_or_else(|| anyhow::anyhow!("native pre-Trash active window expired"))?;
                guard.with_pre_trash_pause(monotonic + remaining, Duration::from_secs(seconds))?
            } else {
                guard
            };
            Ok(Arc::new(guard))
        })),
    );
    let result = crate::serve_managed(
        state.join("metadata.db"),
        registration.session_directory.join("control.sock"),
        cancel.clone(),
        Some(manager),
    )
    .await;
    cancel.cancel();
    worker.await?;
    result?;
    anyhow::bail!("native final receipt was not withheld; inspect retained attempt")
}
async fn recover(path: &Path, digest: &str) -> Result<()> {
    let (registration, account) = load(path, digest, true)?;
    attempt(&registration, digest, "recovery")?;
    let expected = registration
        .recovery
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("native recovery binding absent"))?;
    let marker_path = registration
        .session_directory
        .join("lost-final-confirmation.json");
    let marker_data = bytes(&marker_path, 64 * 1024)?;
    ensure!(
        hash(&marker_data) == expected.marker_sha256,
        "native loss marker changed"
    );
    let marker: LossMarker = serde_json::from_slice(&marker_data)?;
    ensure!(
        marker.version == 1
            && marker.run == registration.run
            && marker.operation == expected.operation
            && marker.request == registration.request()
            && !marker.acknowledgement_returned,
        "native loss marker scope changed"
    );
    marker.receipt.check(&marker.request)?;
    let read = RecoveryJournal::open(&journal_path(&registration), &account.id)?;
    let completed = initial(&registration, &read)?;
    let frozen_before = frontier(&registration, &read, None)?;
    let before = read.native_validation_upload(expected.operation)?;
    ensure!(
        before.state == UploadState::Uploading
            && before.attempt.is_some()
            && before.session_key == Some(expected.operation)
            && before.transferred_bytes == before.size
            && before.remote.is_none()
            && before.package_completion.is_none()
            && before.scope == marker.request.scope
            && before.intent == marker.request.intent
            && before.representation == marker.request.representation
            && before.sha256 == marker.request.sha256
            && before.size == marker.request.size
            && before.working_file.is_none()
            && before.base.is_none()
            && before
                .identity_handoff
                .as_ref()
                .is_some_and(|h| serde_json::to_value(h)
                    .ok()
                    .is_some_and(|v| v["backup"].is_null())),
        "native raw interrupted frontier changed"
    );
    drop(read);
    let cipher = registration
        .session_directory
        .join("state/accounts")
        .join(account.id.as_str())
        .join("upload-checkpoints")
        .join(expected.operation.to_string())
        .join("checkpoint.sealed");
    ensure!(
        file_hash(&cipher, 4 * 1024 * 1024)? == expected.checkpoint_cipher_sha256,
        "native encrypted continuation changed"
    );
    ensure!(
        file_hash(
            &journal_path(&registration)
                .join("objects")
                .join(expected.operation.to_string()),
            LIMIT
        )? == registration.source_b.sha256,
        "native sealed B bytes changed before recovery"
    );
    let state = registration.session_directory.join("state");
    let read_provider = crate::accounts::provider_with_state(&account, &state)?;
    let engine = Engine::new(account.clone(), read_provider, state.clone()).await?;
    let context = WriteContext::open(&engine, &state).await?;
    let reopened = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("native journal lock failed"))?
        .get(expected.operation)?;
    let mut normalized = before.clone();
    normalized.state = UploadState::VerifyRequired;
    normalized.attempt = None;
    ensure!(
        serde_json::to_vec(&reopened)? == serde_json::to_vec(&normalized)?,
        "native interrupted reopen changed more than state"
    );
    let checkpoints = context.checkpoints();
    let saved = checkpoints
        .load(&format!("upload/{}", expected.operation))
        .await?
        .ok_or_else(|| anyhow::anyhow!("native checkpoint unavailable"))?;
    ensure!(
        hash(saved.expose_secret().as_bytes()) == marker.checkpoint_sha256,
        "native checkpoint content changed"
    );
    let guard = Arc::new(Guard::new(
        ICloudWriteProvider::new(&account, &context)?,
        marker.request.clone(),
        context.journal(),
        completed,
        registration.session_directory.clone(),
        registration.run,
        Mode::Recover {
            operation: expected.operation,
            checkpoint_sha256: marker.checkpoint_sha256.clone(),
            receipt: Box::new(marker.receipt.clone()),
        },
    )?);
    let cancel = CancellationToken::new();
    let transfer = TransferWorker::new(
        context.journal(),
        guard.clone(),
        checkpoints.clone(),
        cancel.clone(),
    );
    let remaining = Duration::from_secs(registration.deadline_unix.saturating_sub(now()));
    let result = tokio::time::timeout(remaining, transfer.run_once())
        .await
        .map_err(|_| {
            anyhow::anyhow!("native recovery observation deadline; inspect retained state")
        })??
        .ok_or_else(|| anyhow::anyhow!("native recovery did not select operation"))?;
    ensure!(
        result.id == expected.operation
            && result.state == UploadState::Uploaded
            && guard.counts.refused_uploads.load(Ordering::SeqCst) == 0
            && guard.counts.refused_namespace.load(Ordering::SeqCst) == 0
            && guard.counts.inspections.load(Ordering::SeqCst)
                + guard.counts.reconciliations.load(Ordering::SeqCst)
                > 0,
        "native recovery did not finish through inspection only"
    );
    let row = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("native journal lock failed"))?
        .get(expected.operation)?;
    let (original, current, backup) = row
        .native_replacement_receipt()
        .ok_or_else(|| anyhow::anyhow!("native recovered typed receipt absent"))?;
    ensure!(
        original == &marker.receipt.original
            && current == &marker.receipt.current
            && backup == &marker.receipt.backup
            && row.package_completion.as_ref() == Some(&marker.receipt.current_semantic),
        "native recovered receipt differs"
    );
    let fs = CloudFs::new_experimental_writable(engine.clone(), context.journal()).await?;
    let control = fs.write_control()?;
    ensure!(
        control.publish_completed_package().await?,
        "native recovered metadata publication pending"
    );
    let published = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("native journal lock failed"))?
        .package_publication_status(expected.operation)?;
    ensure!(
        matches!(published,crate::journal::PackagePublicationStatus::Present(node) if node==marker.receipt.current),
        "native recovered publication changed"
    );
    ensure!(
        checkpoints
            .load(&format!("upload/{}", expected.operation))
            .await?
            .is_none(),
        "native completed checkpoint retained"
    );
    ensure!(
        file_hash(
            &registration.session_directory.join("state/accounts.json"),
            256 * 1024
        )? == registration.settings_sha256,
        "native recovery settings changed"
    );
    source(
        &registration.source_a,
        &registration.session_directory,
        "source-a.numbers",
    )?;
    source(
        &registration.source_b,
        &registration.session_directory,
        "source-b.numbers",
    )?;
    ensure!(
        file_hash(path, 64 * 1024)? == digest,
        "native recovery registration changed"
    );
    let counts = guard.counts.value();
    drop(control);
    drop(fs);
    drop(transfer);
    drop(guard);
    drop(context);
    let readonly = RecoveryJournal::open(&journal_path(&registration), &account.id)?;
    let after = frontier(&registration, &readonly, Some(&marker.receipt))?;
    ensure!(
        after.mutations.len() == frozen_before.mutations.len()
            && serde_json::to_vec(&after.mutations)?
                == serde_json::to_vec(&frozen_before.mutations)?
            && after
                .uploads
                .iter()
                .find(|row| row.id == registration.import)
                .map(serde_json::to_vec)
                .transpose()?
                == frozen_before
                    .uploads
                    .iter()
                    .find(|row| row.id == registration.import)
                    .map(serde_json::to_vec)
                    .transpose()?,
        "native unrelated history changed"
    );
    ensure!(
        file_hash(
            &journal_path(&registration)
                .join("objects")
                .join(expected.operation.to_string()),
            LIMIT
        )? == registration.source_b.sha256,
        "native sealed B bytes changed after recovery"
    );
    record(
        &registration
            .session_directory
            .join("recovery-completed.json"),
        &serde_json::json!({"version":1,"run":registration.run,"operation":expected.operation,"same_operation":true,"same_sealed_source":true,"typed_receipt":marker.receipt,"counts":counts,"metadata_publication_confirmed":true,"mounted":false,"independent_postflight_required":true,"cloud_mutation_replayed":false}),
    )?;
    engine.stop().await;
    Ok(())
}
pub async fn icloud_native_final_loss(path: &Path, digest: &str) -> Result<()> {
    loss(path, digest).await.map_err(|_| {
        anyhow::anyhow!("native final-loss validation refused; inspect private attempt")
    })
}
pub async fn icloud_native_final_recover(path: &Path, digest: &str) -> Result<()> {
    recover(path, digest).await.map_err(|_| {
        anyhow::anyhow!("native inspection-only recovery refused; inspect private attempt")
    })
}

// Bounded complete local frontier, held behind the read-only owner lease.
// These are existing local readers; none opens a provider or a writer.
type OperationColumn = (String, String, u64, String, Option<u64>, Option<bool>);

#[derive(Serialize)]
struct Frontier {
    uploads: Vec<UploadRecord>,
    mutations: Vec<crate::journal::MutationRecord>,
    objects: Vec<crate::journal::NamespaceObject>,
    pairs: Vec<(String, String)>,
    columns: Vec<OperationColumn>,
    auxiliary: (i64, i64, i64),
    publication_and_atomic: (i64, i64),
    incomplete: i64,
}
fn frontier(
    registration: &Registration,
    journal: &RecoveryJournal,
    complete: Option<&Receipt>,
) -> Result<Frontier> {
    let uploads = journal.list(0, 4)?;
    let mutations = journal.native_validation_mutations(0, 2)?;
    let (objects, working, publication_and_atomic) = journal.ordinary_validation_inventory()?;
    let auxiliary = journal.native_validation_import_auxiliary_inventory()?;
    let incomplete = journal.native_validation_incomplete_queue()?;
    let pairs = journal.ordinary_validation_namespace_operations()?;
    let columns = journal.ordinary_validation_operation_columns()?;
    ensure!(
        working.is_empty()
            && journal.native_validation_working_inventory()? == (0, 0)
            && auxiliary.1 == 0
            && publication_and_atomic.1 == 0,
        "native unrelated local work refused"
    );
    let folder = mutations
        .iter()
        .find(|row| row.id == registration.parent_creation)
        .ok_or_else(|| anyhow::anyhow!("native folder operation missing"))?;
    let parent = journal
        .ordinary_validation_namespace_for_operation(folder.id)?
        .ok_or_else(|| anyhow::anyhow!("native folder association missing"))?;
    let remote = parent
        .remote
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("native parent remote missing"))?;
    let mut expected_parent = registration.parent.clone();
    expected_parent.etag = remote.etag.clone();
    let mut visible_parent = remote.clone();
    visible_parent.id = parent.node.id.clone();
    ensure!(
        remote == &expected_parent
            && parent.node == visible_parent
            && parent.scope == registration.scope()
            && parent.follows_remote
            && parent.remote_owned
            && !parent.unlinked
            && parent.working_file.is_none()
            && parent.latest.is_none()
            && parent.native_archive.is_none(),
        "native clean folder owner changed"
    );
    let target = registration.recovery.as_ref().map(|value| value.operation);
    let count = if target.is_some() { 3 } else { 2 };
    ensure!(
        columns.len() == count && auxiliary.2 == count as i64,
        "native complete queue inventory changed"
    );
    let mut expected_columns = mutations
        .iter()
        .map(|row| {
            Ok((
                "mutation".into(),
                row.id.to_string(),
                row.sequence,
                serde_json::to_value(row.state)?
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("native mutation state invalid"))?
                    .to_owned(),
                Some(row.sequence),
                Some(row.state == MutationState::Applied),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    for row in &uploads {
        expected_columns.push((
            "upload".into(),
            row.id.to_string(),
            row.sequence,
            serde_json::to_value(row.state)?
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("native upload state invalid"))?
                .to_owned(),
            Some(row.sequence),
            Some(row.state == UploadState::Uploaded),
        ));
    }
    expected_columns.sort_by_key(|row| row.2);
    ensure!(
        columns == expected_columns,
        "native queue/column identities changed"
    );
    let mut expected_pairs = vec![(folder.id.to_string(), parent.id.to_string())];
    if let Some(operation) = target {
        let row = uploads
            .iter()
            .find(|row| row.id == operation)
            .ok_or_else(|| anyhow::anyhow!("native target missing"))?;
        let owner = journal
            .ordinary_validation_namespace_for_operation(operation)?
            .ok_or_else(|| anyhow::anyhow!("native source association missing"))?;
        ensure!(
            row.scope == registration.scope()
                && row.intent == registration.request().intent
                && row.representation == registration.request().representation
                && row.size == registration.source_b.size
                && row.sha256 == registration.source_b.sha256
                && row.base.is_none()
                && row.working_file.is_none(),
            "native target request changed"
        );
        let reserved = serde_json::to_value(
            row.identity_handoff
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("native recovery reservation missing"))?,
        )?;
        let recovery_id = Uuid::parse_str(
            reserved["recovery_object"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("native recovery owner missing"))?,
        )?;
        let recovery = objects
            .iter()
            .find(|node| node.id == recovery_id)
            .ok_or_else(|| anyhow::anyhow!("native reserved namespace missing"))?;
        let recovery_name = format!("recovery-by-cirrove-{operation}.numbers");
        ensure!(
            reserved["old_item"] == registration.original.id
                && reserved["recovery_name"] == recovery_name
                && reserved["trash_parent"] == TRASH
                && recovery.id != owner.id
                && recovery.scope == registration.scope()
                && recovery.names == owner.names
                && recovery.unlinked
                && !recovery.follows_remote
                && recovery.working_file.is_none()
                && recovery.latest.is_none()
                && recovery.native_archive.is_none(),
            "native reserved old identity changed"
        );
        let mut expected_recovery = registration.original.clone();
        expected_recovery.id = format!("local-recovery-{recovery_id}");
        expected_recovery.name = recovery_name;
        let (current, backup_owned, revision) = if let Some(receipt) = complete {
            let (original, current, backup) = row
                .native_replacement_receipt()
                .ok_or_else(|| anyhow::anyhow!("native typed recovered receipt absent"))?;
            ensure!(
                original == &receipt.original
                    && current == &receipt.current
                    && backup == &receipt.backup
                    && row.package_completion.as_ref() == Some(&receipt.current_semantic)
                    && row.state == UploadState::Uploaded
                    && row.attempt.is_none()
                    && row.session_key == Some(operation),
                "native recovered upload changed"
            );
            expected_recovery.size = backup.size;
            expected_recovery.etag = backup.etag.clone();
            expected_recovery.content_version = backup.content_version.clone();
            expected_recovery.modified_unix = backup.modified_unix;
            ensure!(
                recovery.remote.as_ref() == Some(backup)
                    && recovery.remote_sequence == row.sequence
                    && reserved["backup"] == serde_json::to_value(backup)?,
                "native confirmed Trash binding changed"
            );
            (current, true, 1)
        } else {
            ensure!(
                row.state == UploadState::Uploading
                    && row.attempt.is_some()
                    && row.session_key == Some(operation)
                    && row.transferred_bytes == row.size
                    && row.remote.is_none()
                    && row.package_completion.is_none()
                    && reserved["backup"].is_null()
                    && recovery.remote.as_ref() == Some(&registration.original),
                "native interrupted source frontier changed"
            );
            (&registration.original, false, 0)
        };
        let mut expected_visible = current.clone();
        expected_visible.id = owner.node.id.clone();
        expected_visible.parent_id = Some(parent.node.id.clone());
        ensure!(
            owner.scope == registration.scope()
                && owner.remote.as_ref() == Some(current)
                && owner.remote_owned
                && !owner.unlinked
                && !owner.follows_remote
                && owner.working_file.is_none()
                && owner.latest == Some(operation)
                && owner.native_archive.is_none()
                && owner.node == expected_visible
                && recovery.node == expected_recovery
                && recovery.remote_owned == backup_owned
                && recovery.revision == revision,
            "native source/recovery ownership changed"
        );
        ensure!(
            objects.len() == 3
                && auxiliary.0 == 3
                && incomplete == if complete.is_some() { 0 } else { 1 }
                && publication_and_atomic.0 == if complete.is_some() { 2 } else { 1 },
            "native exact recovery frontier changed"
        );
        ensure!(
            journal.native_validation_package_publication(operation)?
                == if let Some(receipt) = complete {
                    crate::journal::PackagePublicationStatus::Present(receipt.current.clone())
                } else {
                    crate::journal::PackagePublicationStatus::Pending
                },
            "native target publication changed"
        );
        expected_pairs.push((operation.to_string(), owner.id.to_string()));
    } else {
        ensure!(
            objects.len() == 1
                && auxiliary.0 == 1
                && incomplete == 0
                && publication_and_atomic.0 == 1,
            "native exact fresh frontier changed"
        );
    }
    expected_pairs.sort();
    ensure!(
        pairs == expected_pairs,
        "native full namespace associations changed"
    );
    Ok(Frontier {
        uploads,
        mutations,
        objects,
        pairs,
        columns,
        auxiliary,
        publication_and_atomic,
        incomplete,
    })
}

#[cfg(test)]
pub(crate) fn test_frontier(
    journal: &RecoveryJournal,
    account: &str,
    folder: Uuid,
    imported: &UploadRecord,
    target: Option<Uuid>,
    complete: Option<&Receipt>,
) -> Result<()> {
    let mutations = journal.native_validation_mutations(0, 2)?;
    let parent = match mutations
        .iter()
        .find(|m| m.id == folder)
        .and_then(|m| m.receipt.as_ref())
    {
        Some(MutationReceipt::Upsert(n)) => n.clone(),
        _ => anyhow::bail!("test folder missing"),
    };
    let (source_layout, expected_root, semantic) = match &imported.representation {
        UploadRepresentation::PackageArchive {
            expected_root,
            semantic,
        } => (
            PackageSourceLayout::Wrapped,
            Some(expected_root.clone()),
            semantic,
        ),
        UploadRepresentation::FlatNumbersArchive { semantic } => {
            (PackageSourceLayout::FlatNumbers, None, semantic)
        }
        _ => anyhow::bail!("test source missing"),
    };
    let b = target
        .map(|id| journal.native_validation_upload(id))
        .transpose()?;
    let (layout_b, root_b, semantic_b) = match b.as_ref().map(|b| &b.representation) {
        Some(UploadRepresentation::PackageReplacementArchive {
            expected_root,
            semantic,
            ..
        }) => (
            PackageSourceLayout::Wrapped,
            Some(expected_root.clone()),
            semantic.clone(),
        ),
        Some(UploadRepresentation::FlatNumbersReplacementArchive { semantic, .. }) => {
            (PackageSourceLayout::FlatNumbers, None, semantic.clone())
        }
        _ => (source_layout, expected_root.clone(), semantic.clone()),
    };
    let registration = Registration {
        version: 1,
        run: Uuid::new_v4(),
        account: Uuid::parse_str(account)?,
        label: String::new(),
        session_directory: PathBuf::new(),
        tmpdir: PathBuf::new(),
        settings_sha256: String::new(),
        authorization_sha256: String::new(),
        executable_sha256: String::new(),
        started_unix: 0,
        deadline_unix: 0,
        parent_creation: folder,
        import: imported.id,
        parent,
        original: imported
            .remote
            .clone()
            .ok_or_else(|| anyhow::anyhow!("test original missing"))?,
        source_a: Source {
            path: PathBuf::new(),
            size: imported.size,
            sha256: imported.sha256.clone(),
            root: expected_root.clone(),
            source_layout,
            semantic: semantic.clone(),
        },
        source_b: Source {
            path: PathBuf::new(),
            size: b.as_ref().map_or(imported.size, |b| b.size),
            sha256: b
                .as_ref()
                .map_or_else(|| imported.sha256.clone(), |b| b.sha256.clone()),
            root: root_b,
            source_layout: layout_b,
            semantic: semantic_b,
        },
        preflight_sha256: String::new(),
        pause_seconds: None,
        recovery: target.map(|operation| Recovery {
            operation,
            marker_sha256: String::new(),
            checkpoint_cipher_sha256: String::new(),
        }),
    };
    initial(&registration, journal)?;
    frontier(&registration, journal, complete)?;
    Ok(())
}

#[cfg(test)]
#[path = "flat_registration_tests.rs"]
mod flat_registration_tests;
