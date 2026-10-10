//! Real isolated application processes; supporting evidence, not installed UI acceptance.
use super::*;
use crate::journal::MutationState;
use cirrove_core::mutation::MutationReceipt;
use std::path::{Component, PathBuf};

const LUA: &str = r#"
local path, mode = arg[1], arg[2]
assert(mode == 'create' or mode == 'replace')
vim.opt.exrc = false
vim.opt.modeline = false
vim.opt.swapfile = false
vim.opt.undofile = false
vim.opt.backup = false
vim.opt.writebackup = true
vim.opt.backupcopy = mode == 'replace' and 'no' or 'yes'
vim.opt.backupdir = {'.'}
vim.opt.backupext = '.cirrove-nvim-backup'
vim.opt.fsync = true
vim.cmd.edit(vim.fn.fnameescape(path))
local uv = vim.uv or vim.loop
local old = mode == 'replace' and assert(uv.fs_open(path, 'r', 0)) or nil
local before = old and assert(uv.fs_fstat(old)) or nil
vim.api.nvim_buf_set_lines(0, 0, -1, false, {
  mode == 'create' and 'Cirrove account-router original' or 'Cirrove account-router replacement'
})
vim.cmd.write()
if old then
  local after = assert(uv.fs_stat(path))
  assert(after.ino ~= before.ino, 'backupcopy=no did not replace inode')
  assert(assert(uv.fs_fstat(old)).nlink == 0, 'old inode not unlinked')
  assert(uv.fs_read(old, before.size, 0) == 'Cirrove account-router original\n')
  assert(uv.fs_close(old))
end
vim.cmd.quit()
"#;

fn owned_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !relative.is_empty()
            && !path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "application path must stay inside the owned mount"
    );
    // Only fixed fixture paths; no provider name is executable input.
    ensure!(
        matches!(
            relative,
            NAME | "Renamed.txt" | "Moved" | "Moved/Account Router.txt"
        ),
        "application target is not registered"
    );
    Ok(root.join(path))
}

fn fixture_guard(f: &Fixture, run: Uuid) -> Result<()> {
    ensure!(
        !run.is_nil()
            && f.parent.name == format!("Cirrove Write Validation-{run}")
            && f.parent.parent_id.as_deref() == Some(ROOT_ID)
            && f.parent.id.starts_with("FOLDER::com.apple.CloudDocs::")
            && f.parent.id != ROOT_ID
            && f.parent.kind == NodeKind::Folder
            && !f.parent.package
            && f.parent.target.is_none()
            && f.scope.account == f.account.id
            && f.scope.provider == "icloud"
            && f.scope.collection == "drive",
        "invalid fresh application fixture"
    );
    Ok(())
}

fn executable_digest(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

const APPLICATION_STAGES: &[(&str, &str, &str)] = &[
    ("/usr/bin/nvim", "create", "editor-create"),
    ("/usr/bin/nvim", "replace", "editor-replace"),
    ("/usr/bin/gio", "mkdir", "gio-mkdir"),
    ("/usr/bin/gio", "rename", "gio-rename"),
    ("/usr/bin/gio", "move", "gio-move"),
    ("/usr/bin/gio", "create", "gio-create"),
    ("/usr/bin/gio", "remove", "gio-remove"),
];
fn application_stage(program: &str, action: &str) -> Result<&'static str> {
    APPLICATION_STAGES
        .iter()
        .find(|(executable, operation, _)| *executable == program && *operation == action)
        .map(|(_, _, stage)| *stage)
        .context("unregistered application stage")
}

async fn command(
    f: &Fixture,
    stage: &str,
    program: &str,
    args: &[std::ffi::OsString],
    input: Option<&[u8]>,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let stage = application_stage(program, stage)?;
    let digest = executable_digest(Path::new(program))?;
    let sandbox = f.run_dir.join("application-state");
    private_dir(&sandbox)?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(&sandbox)
        .env_clear()
        .env("PATH", "/usr/bin")
        .env("LANG", "C.UTF-8")
        .env("GIO_USE_VFS", "local")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // HOME is never overridden. These processes receive no user startup variables.
    for (key, directory) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("TMPDIR", "tmp"),
        ("SQLITE_TMPDIR", "tmp"),
    ] {
        let path = sandbox.join(directory);
        private_dir(&path)?;
        command.env(key, path);
    }
    record(
        &f.run_dir.join(format!("application-{stage}.json")),
        &serde_json::json!({
            "program": program, "executable_sha256":digest,
            "argv":args.iter().map(|a|a.to_string_lossy()).collect::<Vec<_>>(), "timeout_seconds":120, "input_bytes":input.map_or(0, |bytes|bytes.len()),
            "content_logged":false,
        }),
    )?;
    command.stdin(if input.is_some() {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::null()
    });
    let started = std::time::Instant::now();
    let mut child = command.spawn()?;
    let status = tokio::time::timeout(Duration::from_secs(120), async {
        if let Some(bytes) = input {
            let mut stdin = child
                .stdin
                .take()
                .context("application stdin unavailable")?;
            stdin.write_all(bytes).await?;
            stdin.shutdown().await?;
        }
        Ok::<_, anyhow::Error>(child.wait().await?)
    })
    .await
    .context("application deadline; retained fixture requires review")??;
    record(
        &f.run_dir.join(format!("application-{stage}-result.json")),
        &serde_json::json!({
            "success":status.success(), "exit_code":status.code(), "elapsed_ms":started.elapsed().as_millis(),
        }),
    )?;
    ensure!(
        status.success(),
        "application failed at {stage}; fixture retained, no replay"
    );
    Ok(())
}
async fn editor(f: &Fixture, mode: &str) -> Result<()> {
    let script = f.run_dir.join("editor.lua");
    let file = owned_path(&f.run_dir.join("mount"), NAME)?;
    let args = [
        "--headless",
        "-u",
        "NONE",
        "-i",
        "NONE",
        "-n",
        "--cmd",
        "set noexrc nomodeline noloadplugins",
        "-l",
    ]
    .map(std::ffi::OsString::from);
    let args: Vec<_> = args
        .into_iter()
        .chain([script.into_os_string(), file.into_os_string(), mode.into()])
        .collect();
    command(f, mode, "/usr/bin/nvim", &args, None).await
}
async fn gio(
    f: &Fixture,
    stage: &str,
    args: Vec<std::ffi::OsString>,
    input: Option<&[u8]>,
) -> Result<()> {
    command(f, stage, "/usr/bin/gio", &args, input).await
}
async fn settled(session: &WritableSession) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(900), async {
        loop {
            let uploads = session.uploads(0, 64).await?;
            let mutations = session.mutations(0, 64).await?;
            ensure!(
                uploads.len() < 64 && mutations.len() < 64,
                "unexpected application operation count"
            );
            ensure!(
                !uploads
                    .iter()
                    .any(|r| matches!(r.state, UploadState::Failed | UploadState::Conflict))
                    && !mutations.iter().any(|r| matches!(
                        r.state,
                        MutationState::Failed
                            | MutationState::Conflict
                            | MutationState::NeedsReview
                    )),
                "application operation requires review; no replay"
            );
            if !uploads.is_empty()
                && uploads
                    .iter()
                    .all(|r| matches!(r.state, UploadState::Uploaded | UploadState::Resolved))
                && mutations
                    .iter()
                    .all(|r| matches!(r.state, MutationState::Applied | MutationState::Resolved))
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("application settlement deadline; retained fixture")?
}
async fn file(
    f: &Fixture,
    session: &WritableSession,
    parent: &Node,
    name: &str,
    bytes: &[u8],
) -> Result<Node> {
    let read = ICloudDrive::on_demand_from_session_snapshot(
        f.scope.clone(),
        &f.account.identity.username,
        &f.snapshot,
    )?;
    let page = read
        .children(&f.scope, &parent.id, None, &CancellationToken::new())
        .await?;
    ensure!(page.next.is_none(), "unexpected fixture pagination");
    let matches: Vec<_> = page
        .nodes
        .into_iter()
        .filter(|node| node.name == name)
        .collect();
    ensure!(
        matches.len() == 1,
        "application result missing or ambiguous"
    );
    let node = matches
        .into_iter()
        .next()
        .context("application node missing")?;
    let uploaded = session
        .uploads(0, 64)
        .await?
        .into_iter()
        .any(|row| row.remote.is_some_and(|n| n.id == node.id));
    let relocated = session
        .mutations(0, 64)
        .await?
        .into_iter()
        .any(|row| matches!(row.receipt, Some(MutationReceipt::Upsert(n)) if n.id == node.id));
    ensure!(
        (uploaded || relocated) && !node.package && node.target.is_none(),
        "unowned application result"
    );
    ensure!(
        name == NAME,
        "digest verifier uses registered final file name"
    );
    verify(&f.snapshot, &f.account, parent, &node, bytes).await?;
    Ok(node)
}

/// Run twice with distinct fresh UUIDs; each arm exercises both applications.
pub async fn icloud_account_mounted_applications(run: Uuid) -> Result<()> {
    let started = std::time::Instant::now();
    ensure!(!run.is_nil(), "a fresh nonnil run UUID is required");
    ensure!(
        Path::new("/usr/bin/nvim").is_file() && Path::new("/usr/bin/gio").is_file(),
        "required application is missing"
    );
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local-state");
    private_dir(&base)?;
    let filesystem = rustix::fs::fstatfs(&std::fs::File::open(&base)?)?;
    ensure!(
        filesystem.f_type != libc::TMPFS_MAGIC && filesystem.f_type != libc::FUSE_SUPER_MAGIC,
        "application evidence must live on a disk-backed non-FUSE filesystem"
    );
    let binary = std::env::current_exe()?;
    record(
        &base.join(format!("icloud-application-launch-{run}.json")),
        &serde_json::json!({
            "run":run,"pid":std::process::id(),"binary":binary,"binary_sha256":executable_digest(&binary)?,
            "command":[binary.to_string_lossy().to_string(),"--account-mounted-applications",run.to_string()],
            "started_unix":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
            "question":"Can one freshly owned subtree complete actual Neovim and Gio operations with independent cloud verification and remount?",
            "prediction":"Both complete application arms pass; local backupcopy=no inode replacement is observed; remote backup identity is reported separately.",
            "maximum_stage_seconds":900,"maximum_application_seconds":120,"fixture_filesystem_type":filesystem.f_type,
            "ordinary_daemon_untouched":true,"installed_settings_workflow":false,
        }),
    )?;
    let f = prepare(run, "mounted-applications").await?;
    fixture_guard(&f, run)?;
    let mut remote =
        ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
    ensure!(
        remote.list_folder(&f.parent.id).await?.is_empty(),
        "fresh application folder is not empty"
    );
    let script = f.run_dir.join("editor.lua");
    let mut out = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(script)?;
    out.write_all(LUA.as_bytes())?;
    out.sync_all()?;
    drop(out);
    let session = mount(&f).await?;
    let result: Result<(Node, String)> = async {
        editor(&f, "create").await?;
        settled(&session).await?;
        let original = file(&f, &session, &f.parent, NAME, FIRST).await?;
        record(&f.run_dir.join("editor-original.json"), &original)?;
        editor(&f, "replace").await?;
        settled(&session).await?;
        let current = file(&f, &session, &f.parent, NAME, SECOND).await?;
        ensure!(current.id != original.id && remote.exact_item_in_trash(&original.id).await?, "editor predecessor not recoverable");
        record(&f.run_dir.join("editor-replaced.json"), &current)?;
        let editor_uploads = session.uploads(0, 64).await?;
        let editor_mutations = session.mutations(0, 64).await?;
        let backup_name = format!("{NAME}.cirrove-nvim-backup");
        let backup_receipts: Vec<_> = editor_mutations.iter().filter_map(|row| {
            match &row.receipt {
                Some(MutationReceipt::Upsert(node)) if node.name == backup_name => Some(node.id.clone()),
                _ => None,
            }
        }).collect();
        record(&f.run_dir.join("editor-lineage.json"), &serde_json::json!({
            "uploads": editor_uploads, "mutations": editor_mutations,
            "original_id":original.id, "replacement_id":current.id,
            "backup_remote_ids":backup_receipts,
            "local_inode_replacement_asserted":true,
            "remote_backup_identity_proved":!backup_receipts.is_empty(),
            "atomic_cloud_publication_claimed":false,
        }))?;
        let mount = f.run_dir.join("mount");
        let root_file = owned_path(&mount, NAME)?.into_os_string();
        let folder_path = owned_path(&mount, "Moved")?.into_os_string();
        gio(&f, "mkdir", vec!["mkdir".into(), folder_path.clone()], None).await?;
        settled(&session).await?;
        let folder = session.mutations(0,64).await?.into_iter().filter_map(|r|match r.receipt { Some(MutationReceipt::Upsert(n)) if n.kind == NodeKind::Folder && n.name == "Moved" && n.parent_id.as_ref() == Some(&f.parent.id) => Some(n), _ => None }).next_back().context("Gio folder receipt missing")?;
        gio(&f, "rename", vec!["rename".into(), root_file.clone(), "Renamed.txt".into()], None).await?;
        settled(&session).await?;
        gio(&f, "move", vec!["move".into(), "--no-copy-fallback".into(), "--no-target-directory".into(), owned_path(&mount,"Renamed.txt")?.into_os_string(), owned_path(&mount,"Moved/Account Router.txt")?.into_os_string()], None).await?;
        settled(&session).await?;
        let moved = file(&f, &session, &folder, NAME, SECOND).await?;
        ensure!(moved.id == current.id, "Gio relocation changed the owned remote identity");
        record(&f.run_dir.join("gio-moved.json"), &moved)?;
        gio(&f, "create", vec!["save".into(), "--create".into(), "--private".into(), root_file.clone()], Some(FIRST)).await?;
        settled(&session).await?;
        let disposable = file(&f, &session, &f.parent, NAME, FIRST).await?;
        // Gio local remove issues unlink; Cirrove maps it to provider recoverable Trash.
        // This is deliberately not a claim that the Freedesktop `gio trash` API works.
        gio(&f, "remove", vec!["remove".into(), root_file], None).await?;
        settled(&session).await?;
        ensure!(remote.exact_item_in_trash(&disposable.id).await?, "Gio deletion not in recoverable cloud Trash");
        let listing = remote.list_folder(&f.parent.id).await?;
        ensure!(listing.len()==1 && listing[0].drivewsid==folder.id, "unexpected temporary files remain");
        record(&f.run_dir.join("application-journal.json"), &serde_json::json!({"uploads":session.uploads(0,64).await?,"mutations":session.mutations(0,64).await?}))?;
        record(&f.run_dir.join("gio-deleted.json"), &disposable)?;
        Ok((folder, current.id))
    }.await;
    let shutdown = session.shutdown().await;
    let (folder, expected_id) = result?;
    shutdown?;
    let reopened = mount(&f).await?;
    let result: Result<()> = async {
        let bytes = tokio::time::timeout(
            Duration::from_secs(120),
            tokio::fs::read(owned_path(
                &f.run_dir.join("mount"),
                "Moved/Account Router.txt",
            )?),
        )
        .await??;
        ensure!(bytes == SECOND, "application bytes changed after remount");
        let restored = file(&f, &reopened, &folder, NAME, SECOND).await?;
        ensure!(
            restored.id == expected_id,
            "Gio remote identity changed after remount"
        );
        Ok(())
    }
    .await;
    let shutdown = reopened.shutdown().await;
    result?;
    shutdown?;
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,"elapsed_ms":started.elapsed().as_millis(),"real_nvim":true,"backupcopy_no_inode_replaced":true,"gio_backend_create_rename_move_remove":true,"provider_recoverable_trash":true,"gio_remote_identity_preserved":true,"independent_digests":true,"remounted_read":true,"installed_settings_workflow":false,"gui_clickthrough":false,"gio_trash_tested":false,"atomic_cloud_publication_claimed":false,"editor_lineage_requires_review":true}),
    )?;
    println!(
        "Owned application arm passed: Neovim, Gio backend, independent digests and remount. GUI/installed workflow remain separate gates."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn application_manifests_have_unique_registered_stage_names() {
        let mut paths = std::collections::HashSet::new();
        for (program, operation, _) in APPLICATION_STAGES {
            let stage = application_stage(program, operation).expect("registered stage");
            assert!(
                paths.insert(format!("application-{stage}.json")),
                "duplicate launch manifest"
            );
            assert!(
                paths.insert(format!("application-{stage}-result.json")),
                "duplicate result manifest"
            );
        }
        assert_eq!(paths.len(), 14);
        assert_eq!(
            application_stage("/usr/bin/nvim", "create").expect("editor"),
            "editor-create"
        );
        assert_eq!(
            application_stage("/usr/bin/gio", "create").expect("gio"),
            "gio-create"
        );
        assert!(application_stage("/usr/bin/gio", "../escape").is_err());
        assert!(application_stage("/untrusted/program", "create").is_err());
    }

    #[test]
    fn application_fixture_refuses_foreign_root_package_and_run_identity() {
        let run = Uuid::new_v4();
        let mut settings: Settings = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../cirrove-desktop/fixtures/accounts.json"
        )))
        .expect("synthetic fixture");
        let account = settings.accounts.remove(0);
        let mut f = Fixture {
            state: "/synthetic/state".into(),
            run_dir: "/synthetic/run".into(),
            scope: Scope {
                account: account.id.clone(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            account,
            snapshot: SecretString::from("synthetic-not-a-session"),
            parent: Node {
                id: "FOLDER::com.apple.CloudDocs::owned".into(),
                parent_id: Some(ROOT_ID.into()),
                name: format!("Cirrove Write Validation-{run}"),
                kind: NodeKind::Folder,
                size: 0,
                modified_unix: 0,
                etag: None,
                content_version: None,
                target: None,
                package: false,
            },
        };
        assert!(fixture_guard(&f, run).is_ok());
        assert!(fixture_guard(&f, Uuid::new_v4()).is_err());
        assert!(fixture_guard(&f, Uuid::nil()).is_err());
        f.parent.package = true;
        assert!(fixture_guard(&f, run).is_err());
        f.parent.package = false;
        f.parent.id = ROOT_ID.into();
        assert!(fixture_guard(&f, run).is_err());
        f.parent.id = "FOLDER::com.apple.CloudDocs::owned".into();
        f.scope.account = Uuid::new_v4().to_string();
        assert!(fixture_guard(&f, run).is_err());
    }

    #[test]
    fn application_paths_reject_escape_and_unregistered_targets() {
        for relative in [
            "",
            "/etc/passwd",
            "../outside",
            "Moved/../../outside",
            ".Trash",
            "other.txt",
        ] {
            assert!(owned_path(Path::new("/fixture/mount"), relative).is_err());
        }
        for relative in [NAME, "Moved", "Renamed.txt", "Moved/Account Router.txt"] {
            assert!(
                owned_path(Path::new("/fixture/mount"), relative)
                    .expect("registered fixture path")
                    .starts_with("/fixture/mount")
            );
        }
    }
}
