//! Fresh, read-only typed metadata plus complete PACKAGE readback for the standalone arm.
use super::registration::{Source, bytes, hash, now, pin, private};
use crate::{
    accounts::{Account, Settings},
    journal::RecoveryJournal,
};
use anyhow::{Result, ensure};
use cirrove_auth::{AccessMode, AppRegistration, CredentialVault};
use cirrove_core::{Node, NodeKind};
use cirrove_icloud::{DriveEntry, ICloudReadSession, ROOT_ID, SealedSessionVault};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;
const CAP: u64 = 32 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    run: Uuid,
    account: Uuid,
    label: String,
    session_directory: PathBuf,
    settings_sha256: String,
    source: Source,
    started_unix: u64,
    deadline_unix: u64,
}
fn registered(data: &[u8], digest: &str, clock: u64) -> Result<Registration> {
    ensure!(
        data.len() as u64 <= CAP && pin(digest) && hash(data) == digest,
        "metadata registration pin refused"
    );
    let r: Registration = serde_json::from_slice(data)?;
    ensure!(
        r.version == 1
            && !r.run.is_nil()
            && !r.account.is_nil()
            && r.label == "iCloudNativeTrashLossValidation"
            && r.session_directory == format!("/var/tmp/cirrove-native-trash-{}", r.run)
            && pin(&r.settings_sha256)
            && r.started_unix > 0
            && r.started_unix <= clock
            && clock < r.deadline_unix
            && r.deadline_unix
                .checked_sub(r.started_unix)
                .is_some_and(|n| n > 0 && n <= 600),
        "metadata scope/window refused"
    );
    ensure!(
        r.source.path == r.session_directory.join("source.numbers")
            && r.source.source_layout == cirrove_core::upload::PackageSourceLayout::FlatNumbers
            && r.source.root.is_none()
            && pin(&r.source.sha256)
            && r.source.size > 0
            && r.source.size <= 64 * 1024 * 1024
            && r.source.semantic.version == 2
            && r.source.semantic.validate().is_ok(),
        "metadata flat source refused"
    );
    Ok(r)
}
fn account(r: &Registration, data: &[u8]) -> Result<Account> {
    ensure!(hash(data) == r.settings_sha256, "metadata settings changed");
    let s: Settings = serde_json::from_slice(data)?;
    ensure!(
        s.version == 2 && s.accounts.len() == 1,
        "metadata singleton account refused"
    );
    let a = s.accounts[0].clone();
    ensure!(
        a.id == r.account.to_string()
            && a.label == r.label
            && matches!(a.registration, AppRegistration::ICloud)
            && a.enabled
            && a.access == AccessMode::ReadWrite
            && a.drive.id == "drive"
            && a.drive.drive_type == "icloud_drive"
            && a.root_id == ROOT_ID
            && a.mount_path == r.session_directory.join("mount")
            && !Uuid::parse_str(&a.credential_id)?.is_nil(),
        "metadata account route refused"
    );
    Ok(a)
}
fn selected(entries: &[DriveEntry], r: &Registration, parent: Option<&str>) -> Result<DriveEntry> {
    let name = match parent {
        None => format!("Cirrove-Native-Trash-{}", r.run),
        Some(_) => format!("Cirrove-Numbers-Trash-{}.numbers", r.run),
    };
    let mut found = entries.iter().filter(|v| v.display_name() == name);
    let mut v = found
        .next()
        .ok_or_else(|| anyhow::anyhow!("owned metadata absent"))?
        .clone();
    let folder = parent.is_none();
    ensure!(
        found.next().is_none()
            && entries
                .iter()
                .filter(|e| e.drivewsid == v.drivewsid)
                .count()
                == 1
            && v.drivewsid.starts_with(if folder {
                "FOLDER::com.apple.CloudDocs::"
            } else {
                "FILE::com.apple.CloudDocs::"
            })
            && v.drivewsid.len()
                > if folder {
                    "FOLDER::com.apple.CloudDocs::".len()
                } else {
                    "FILE::com.apple.CloudDocs::".len()
                }
            && v.drivewsid.len() <= 4096
            && !v.drivewsid.chars().any(char::is_control)
            && v.drivewsid != ROOT_ID
            && !v.drivewsid.ends_with("::TRASH_ROOT")
            && v.zone == "com.apple.CloudDocs"
            && v.kind == if folder { "FOLDER" } else { "FILE" }
            && !v.etag.is_empty()
            && v.etag.len() <= 4096
            && !v.etag.chars().any(|c| c.is_control() || c == '*')
            && match parent {
                None => v.parent_id.is_empty() || v.parent_id == ROOT_ID,
                Some(id) => v.parent_id == id,
            }
            && (folder
                || (v.extension == "numbers"
                    && v.items.is_empty()
                    && !v.docwsid.is_empty()
                    && v.drivewsid == format!("FILE::com.apple.CloudDocs::{}", v.docwsid)
                    && v.size > 0
                    && v.size <= 64 * 1024 * 1024)),
        "owned metadata identity refused"
    );
    if folder {
        v.items.clear();
        v.number_of_items = None;
    }
    Ok(v)
}
fn node(entry: &DriveEntry, package: bool) -> Node {
    // This is the same mapping used by the provider's validated package receipt.
    Node {
        id: entry.drivewsid.clone(),
        parent_id: Some(if entry.parent_id.is_empty() {
            ROOT_ID.into()
        } else {
            entry.parent_id.clone()
        }),
        name: entry.display_name(),
        kind: NodeKind::Folder,
        size: entry.size,
        modified_unix: 0,
        etag: Some(entry.etag.clone()),
        content_version: None,
        target: None,
        package,
    }
}
fn record(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    File::open(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("metadata output parent missing"))?,
    )?
    .sync_all()?;
    Ok(())
}
fn same(path: &Path, held: &File) -> Result<()> {
    let a = private(path, true, 0)?.metadata()?;
    let b = held.metadata()?;
    ensure!(
        (a.dev(), a.ino()) == (b.dev(), b.ino()),
        "metadata directory changed"
    );
    Ok(())
}
fn fence(path: &Path, digest: &str, r: &Registration, root: &File, state: &File) -> Result<()> {
    registered(&bytes(path, CAP)?, digest, now())?;
    account(
        r,
        &bytes(&r.session_directory.join("state/accounts.json"), 256 * 1024)?,
    )?;
    r.source.check(&r.session_directory)?;
    same(&r.session_directory, root)?;
    same(&r.session_directory.join("state"), state)?;
    ensure!(
        !r.session_directory.join("control.sock").try_exists()?,
        "metadata writer socket present"
    );
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
    let mount = r.session_directory.join("mount");
    ensure!(
        !mounts.lines().any(|line| line
            .split_whitespace()
            .nth(4)
            .is_some_and(|p| Path::new(p) == mount)),
        "metadata mount present"
    );
    // Take and release the existing read-only owner lease before any network await.
    let journal = RecoveryJournal::open(
        &r.session_directory
            .join("state/accounts")
            .join(r.account.to_string())
            .join("journal"),
        &r.account.to_string(),
    )?;
    drop(journal);
    Ok(())
}
async fn observe(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let r = registered(&bytes(path, CAP)?, digest, now())?;
    ensure!(
        path == r.session_directory.join("metadata-registration.json"),
        "metadata registration path refused"
    );
    let root = private(&r.session_directory, true, 0)?;
    let state = private(&r.session_directory.join("state"), true, 0)?;
    let a = account(
        &r,
        &bytes(&r.session_directory.join("state/accounts.json"), 256 * 1024)?,
    )?;
    fence(path, digest, &r, &root, &state)?;
    record(
        &r.session_directory.join("metadata-attempt.json"),
        &serde_json::json!({"version":1,"run":r.run,"account":r.account,"registration_sha256":digest,"cloud_mutated":false,"automatic_retry":false}),
    )?;
    let remaining = r
        .deadline_unix
        .checked_sub(now())
        .ok_or_else(|| anyhow::anyhow!("metadata deadline expired"))?;
    ensure!(remaining > 0, "metadata deadline expired");
    tokio::time::timeout(Duration::from_secs(remaining), async {
        let vault = SealedSessionVault::new(&r.session_directory.join("state"), &a.id)?;
        let snapshot = vault.load(&a.credential_id).await?.ok_or_else(|| anyhow::anyhow!("saved metadata session unavailable"))?;
        let mut remote = ICloudReadSession::from_session_snapshot(&snapshot, &a.identity.username)?;
        let p = selected(&remote.list_root().await?, &r, None)?;
        let d = selected(&remote.list_folder(&p.drivewsid).await?, &r, Some(&p.drivewsid))?;
        fence(path, digest, &r, &root, &state)?;
        let fixture = serde_json::json!({"version":1,"run":r.run,"account":r.account,"session_directory":r.session_directory,
            "settings_sha256":r.settings_sha256,"parent":p,"document":d,"format":"numbers","representation":"package",
            "source":r.source.path,"source_size":r.source.size,"source_sha256":r.source.sha256,"source_root":null,
            "expected_root":d.display_name(),"semantic":r.source.semantic});
        let fixture_path = r.session_directory.join("preflight-fixture.json"); record(&fixture_path, &fixture)?;
        let fixture_sha256 = hash(&bytes(&fixture_path, CAP)?);
        let content_proof = super::super::owned_package::icloud_owned_native_trash_fixture_verify(&fixture_path, &fixture_sha256).await?;
        ensure!(selected(&remote.list_folder(&p.drivewsid).await?, &r, Some(&p.drivewsid))? == d
            && selected(&remote.list_root().await?, &r, None)? == p, "metadata revision changed");
        fence(path, digest, &r, &root, &state)?;
        let result = serde_json::json!({"version":1,"run":r.run,"account":r.account,"session_directory":r.session_directory,
            "registration_sha256":digest,"settings_sha256":r.settings_sha256,"source":r.source,
            "parent":node(&p,false),"original":node(&d,true),"parent_entry":p,"document_entry":d,
            "fixture_path":fixture_path,"fixture_sha256":fixture_sha256,"content_proof":content_proof,
            "metadata_acquired":true,"current_content_verified":true,"cloud_mutated":false,"automatic_retry":false});
        record(&r.session_directory.join("metadata-acquired.json"), &result)?; Ok(result)
    }).await.map_err(|_| anyhow::anyhow!("metadata deadline expired"))?
}
/// Saved-session read-only acquisition; all underlying error details are discarded.
pub async fn icloud_owned_native_trash_metadata(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    observe(path, digest).await.map_err(|_| {
        anyhow::anyhow!("owned native Trash metadata refused; retained evidence; no retry")
    })
}
#[cfg(test)]
mod tests;
