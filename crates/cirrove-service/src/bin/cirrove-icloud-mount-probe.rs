//! Foreground, isolated read-only iCloud mount for live protocol validation.
//! This feature-gated binary neither edits account settings nor persists grants.
use anyhow::{Context, Result, bail};
use cirrove_auth::{AccessMode, AppRegistration, Identity};
use cirrove_core::{CollectionInfo, Scope};
use cirrove_icloud::{ICloudDrive, ICloudReadSession, ROOT_ID, SignInStep};
use cirrove_service::{accounts::Account, engine::Engine, filesystem::CloudFs};
use secrecy::SecretString;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let apple_id = args
        .next()
        .context("usage: cirrove-icloud-mount-probe APPLE_ID")?;
    if args.next().is_some() {
        bail!("usage: cirrove-icloud-mount-probe APPLE_ID");
    }
    let run = PreviewRun::prepare()?;
    let password =
        SecretString::new(rpassword::prompt_password("Apple account password: ")?.into());
    let mut session = ICloudReadSession::new()?;
    match session.sign_in(&apple_id, &password).await? {
        SignInStep::Ready => {}
        SignInStep::NeedsTrustedDeviceCode => {
            session.request_trusted_device_code().await?;
            let code =
                SecretString::new(rpassword::prompt_password("Trusted-device code: ")?.into());
            session.verify_trusted_device_code(&code).await?;
        }
    }
    drop(password);
    let account = run.account(&apple_id);
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: account.drive.id.clone(),
    };
    let provider = Arc::new(ICloudDrive::on_demand_from_live_session(scope, session)?);
    let engine = Engine::new(account, provider, run.state.clone()).await?;
    if let Err(error) = engine.start().await {
        engine.stop().await;
        return Err(error);
    }
    let fs = match CloudFs::new(engine.clone()) {
        Ok(fs) => fs,
        Err(error) => {
            engine.stop().await;
            return Err(error.into());
        }
    };
    let mount_path = run.mount.clone();
    let mounted = tokio::task::spawn_blocking(move || fs.mount(&mount_path)).await?;
    let mounted = match mounted {
        Ok(mounted) => mounted,
        Err(error) => {
            engine.stop().await;
            return Err(error.into());
        }
    };
    println!(
        "Read-only iCloud preview mounted at {}. Open this folder in Files; press Ctrl+C here to unmount.",
        run.mount.display()
    );
    let signal = tokio::signal::ctrl_c().await;
    engine.stop().await;
    tokio::task::spawn_blocking(move || mounted.umount_and_join()).await??;
    run.complete()?;
    signal?;
    Ok(())
}

struct PreviewRun {
    root: PathBuf,
    state: PathBuf,
    mount: PathBuf,
    id: String,
}

impl PreviewRun {
    fn prepare() -> Result<Self> {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .context("cannot locate the Cirrove worktree")?;
        let temp = std::env::var_os("TMPDIR")
            .context("set a private, disk-backed TMPDIR before running the preview")?;
        let temp = PathBuf::from(temp).canonicalize()?;
        let temp_meta = temp.metadata()?;
        if !temp.is_dir()
            || temp_meta.permissions().mode() & 0o077 != 0
            || temp_meta.dev() != workspace.metadata()?.dev()
        {
            bail!("TMPDIR must be a private directory on the worktree's disk filesystem");
        }
        let local = workspace.join(".local-state");
        if local.exists() && local.symlink_metadata()?.file_type().is_symlink() {
            bail!("the worktree's local state must not be a symlink");
        }
        private_dir(&local)?;
        let base = local.join("icloud-mount-preview");
        private_dir(&base)?;
        let id = uuid::Uuid::new_v4().to_string();
        let root = base.join(&id);
        private_dir(&root)?;
        let state = root.join("state");
        let mount = root.join("mount");
        private_dir(&state)?;
        private_dir(&mount)?;
        let run = Self {
            root,
            state,
            mount,
            id,
        };
        run.manifest(&temp)?;
        Ok(run)
    }

    fn account(&self, apple_id: &str) -> Account {
        Account {
            id: self.id.clone(),
            label: "iCloudPreview".into(),
            registration: AppRegistration::ICloud,
            identity: Identity {
                tenant_id: String::new(),
                subject: String::new(),
                username: apple_id.into(),
                graph_user_id: String::new(),
                display_name: "iCloud preview".into(),
            },
            credential_id: uuid::Uuid::new_v4().to_string(),
            access: AccessMode::ReadOnly,
            drive: CollectionInfo {
                id: "drive".into(),
                name: "iCloud Drive".into(),
                drive_type: "icloud_drive".into(),
                web_url: "https://www.icloud.com/iclouddrive".into(),
            },
            root_id: ROOT_ID.into(),
            mount_path: self.mount.clone(),
            enabled: true,
            poll_seconds: 60,
            cache_bytes: 256 * 1024 * 1024,
        }
    }

    fn manifest(&self, temp: &Path) -> Result<()> {
        let mut binary = File::open(std::env::current_exe()?)?;
        let mut digest = Sha256::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = binary.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            digest.update(&chunk[..n]);
        }
        let manifest = json!({
            "command": "cirrove-icloud-mount-probe APPLE_ID (interactive secret prompts)",
            "binary_sha256": hex::encode(digest.finalize()),
            "private_tmpdir": temp,
            "expected_duration_minutes": 60,
            "process_id": std::process::id(),
            "mount_path": self.mount,
            "state_path": self.state,
            "state": "running"
        });
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.root.join("manifest.json"))?;
        serde_json::to_writer_pretty(&mut file, &manifest)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    }

    fn complete(&self) -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.root.join("unmounted"))?;
        file.write_all(b"The foreground read-only mount exited.\n")?;
        file.sync_all()?;
        Ok(())
    }
}

fn private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.symlink_metadata()?.file_type().is_symlink() {
        bail!("preview state directory must not be a symlink");
    }
    let metadata = path.metadata()?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("preview state directory must already be private");
    }
    Ok(())
}
