//! Feature-only receipt-selected ordinary XLSX metadata capture. No mutation or journal claim.
use super::super::public_verify::exact_entry;
use super::*;
use cirrove_core::{Node, NodeKind};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    A,
    B,
}
impl Phase {
    fn letter(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: PathBuf,
    size: u64,
    sha256: String,
    #[serde(deserialize_with = "super::required_source_root")]
    root: Option<String>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    phase: Phase,
    run: Uuid,
    account: Uuid,
    session_directory: PathBuf,
    root_dev: u64,
    root_ino: u64,
    settings_sha256: String,
    parent: Node,
    document: Node,
    source: Source,
    started_unix_seconds: u64,
    deadline_unix_seconds: u64,
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn revision(node: &Node) -> bool {
    node.etag.as_ref().is_some_and(|v| {
        !v.is_empty() && v.len() <= 4096 && !v.chars().any(|c| c.is_control() || c == '*')
    })
}
fn identity(id: &str, prefix: &str) -> bool {
    id.starts_with(prefix)
        && id.len() > prefix.len()
        && id.len() <= 4096
        && !id.chars().any(char::is_control)
}
fn registered(bytes: &[u8], digest: &str, clock: u64) -> Result<Registration> {
    ensure!(
        bytes.len() <= 32 * 1024
            && hex_digest(digest)
            && hex::encode(Sha256::digest(bytes)) == digest,
        "Calc registration digest refused"
    );
    let r: Registration =
        serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!("Calc schema refused"))?;
    let expected = format!("/var/tmp/cirrove-calc-editor-{}", r.run);
    ensure!(
        r.version == 1
            && !r.run.is_nil()
            && !r.account.is_nil()
            && r.session_directory == Path::new(&expected)
            && r.root_dev > 0
            && r.root_ino > 0
            && hex_digest(&r.settings_sha256),
        "Calc owned registration refused"
    );
    ensure!(
        identity(&r.parent.id, "FOLDER::com.apple.CloudDocs::")
            && r.parent.id != cirrove_icloud::ROOT_ID
            && !r.parent.id.ends_with("::TRASH_ROOT")
            && r.parent.parent_id.as_deref() == Some(cirrove_icloud::ROOT_ID)
            && r.parent.name == format!("Cirrove-Native-{}", r.run)
            && r.parent.kind == NodeKind::Folder
            && !r.parent.package
            && r.parent.target.is_none(),
        "Calc parent receipt refused"
    );
    ensure!(
        identity(&r.document.id, "FILE::com.apple.CloudDocs::")
            && r.document.parent_id.as_ref() == Some(&r.parent.id)
            && r.document.name == format!("Cirrove-Calc-{}.xlsx", r.run)
            && r.document.kind == NodeKind::File
            && !r.document.package
            && r.document.target.is_none()
            && revision(&r.document)
            && r.document.size == r.source.size
            && r.source.size > 0
            && r.source.size <= LIMIT
            && r.source.path
                == r.session_directory
                    .join(format!("source-{}.xlsx", r.phase.letter()))
            && hex_digest(&r.source.sha256)
            && r.source.root.is_none(),
        "Calc DATA source or receipt refused"
    );
    ensure!(
        r.started_unix_seconds > 0
            && r.started_unix_seconds <= clock
            && clock < r.deadline_unix_seconds
            && r.deadline_unix_seconds
                .checked_sub(r.started_unix_seconds)
                .is_some_and(|n| n > 0 && n <= 1800),
        "Calc finite window refused"
    );
    Ok(r)
}
fn bytes(path: &Path, cap: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    open_private(path, false, cap)?
        .take(cap + 1)
        .read_to_end(&mut out)?;
    ensure!(out.len() as u64 <= cap, "Calc private input bound refused");
    Ok(out)
}
fn source(r: &Registration) -> Result<()> {
    let mut file = open_private(&r.source.path, false, LIMIT)?;
    let before = file.metadata()?;
    ensure!(
        before.len() == r.source.size && before.permissions().mode() & 0o777 == 0o400,
        "Calc source mode/size refused"
    );
    let (digest, count) = {
        let mut reader = (&mut file).take(r.source.size + 1);
        let mut hash = Sha256::new();
        let mut count = 0u64;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            count += n as u64;
            ensure!(count <= r.source.size, "Calc source grew");
            hash.update(&buf[..n]);
        }
        (hex::encode(hash.finalize()), count)
    };
    let after = file.metadata()?;
    ensure!(
        count == r.source.size
            && digest == r.source.sha256
            && before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "Calc source changed"
    );
    Ok(())
}
fn account(r: &Registration, raw: &[u8]) -> Result<Account> {
    ensure!(
        hex::encode(Sha256::digest(raw)) == r.settings_sha256,
        "Calc settings digest refused"
    );
    let settings: Settings = serde_json::from_slice(raw)?;
    ensure!(
        settings.version == 2 && settings.accounts.len() == 1,
        "Calc singleton settings refused"
    );
    let a = settings.accounts[0].clone();
    ensure!(
        a.id == r.account.to_string()
            && matches!(a.registration, AppRegistration::ICloud)
            && a.enabled
            && a.drive.id == "drive"
            && a.drive.drive_type == "icloud_drive"
            && a.root_id == cirrove_icloud::ROOT_ID
            && a.mount_path == r.session_directory.join("mount")
            && !Uuid::parse_str(&a.credential_id)?.is_nil(),
        "Calc account route refused"
    );
    Ok(a)
}
fn parent(entries: &[DriveEntry], r: &Registration) -> Result<DriveEntry> {
    let mut candidates = entries.iter().filter(|v| v.drivewsid == r.parent.id);
    let mut p = candidates.next().context("Calc parent absent")?.clone();
    ensure!(
        candidates.next().is_none()
            && entries
                .iter()
                .filter(|v| v.display_name() == r.parent.name)
                .count()
                == 1
            && p.display_name() == r.parent.name
            && p.kind == "FOLDER"
            && p.zone == "com.apple.CloudDocs"
            && !p.etag.is_empty()
            && p.etag.len() <= 4096
            && !p.etag.chars().any(|c| c.is_control() || c == '*')
            && (p.parent_id.is_empty() || p.parent_id == cirrove_icloud::ROOT_ID),
        "Calc fresh parent identity refused"
    );
    // Parent revision is fresh; historical CreateFolder ETag may precede its children.
    p.items.clear();
    p.number_of_items = None;
    Ok(p)
}
fn unchanged(path: &Path, digest: &str, r: &Registration, root: &File, state: &File) -> Result<()> {
    let _ = registered(&bytes(path, 32 * 1024)?, digest, now()?)?;
    let _ = account(
        r,
        &bytes(&r.session_directory.join("state/accounts.json"), 256 * 1024)?,
    )?;
    source(r)?;
    same_directory(&r.session_directory, root)?;
    same_directory(&r.session_directory.join("state"), state)
}
async fn observe(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let r = registered(&bytes(path, 32 * 1024)?, digest, now()?)?;
    let letter = r.phase.letter();
    ensure!(
        path == r
            .session_directory
            .join(format!("calc-{letter}-registration.json")),
        "Calc registration path refused"
    );
    let root = open_private(&r.session_directory, true, 0)?;
    let m = root.metadata()?;
    ensure!(
        (m.dev(), m.ino()) == (r.root_dev, r.root_ino),
        "Calc root identity refused"
    );
    let state = open_private(&r.session_directory.join("state"), true, 0)?;
    let a = account(
        &r,
        &bytes(&r.session_directory.join("state/accounts.json"), 256 * 1024)?,
    )?;
    let _accounts = open_private(&r.session_directory.join("state/accounts"), true, 0)?;
    let _account = open_private(
        &r.session_directory
            .join("state/accounts")
            .join(r.account.to_string()),
        true,
        0,
    )?;
    unchanged(path, digest, &r, &root, &state)?;
    record(
        &r.session_directory
            .join(format!("calc-{letter}.attempt.json")),
        &serde_json::json!({
        "run":r.run,"phase":r.phase,"account":r.account,"registration_sha256":digest,
        "provider_mutation":false,"automatic_retry":false}),
    )?;
    root.sync_all()?;
    let remaining = r
        .deadline_unix_seconds
        .checked_sub(now()?)
        .context("Calc window expired")?;
    ensure!(remaining > 0, "Calc window expired");
    tokio::time::timeout(Duration::from_secs(remaining),async {
        let mut remote=session(&r.session_directory,&a).await?;
        let p=parent(&remote.list_root().await?,&r)?;
        let d=exact_entry(&remote.list_folder(&p.drivewsid).await?,&r.document)?;
        ensure!(d.extension=="xlsx" && d.items.is_empty(),"Calc metadata extension/envelope refused");
        ensure!(exact_entry(&remote.list_folder(&p.drivewsid).await?,&r.document)?==d
            && parent(&remote.list_root().await?,&r)?==p,"Calc metadata revision changed");
        let value=serde_json::json!({"version":1,"run":r.run,"account":r.account,"session_directory":r.session_directory,
            "settings_sha256":r.settings_sha256,"parent":p,"document":d,"format":"xlsx","representation":"data",
            "source":r.source.path,"source_size":r.source.size,"source_sha256":r.source.sha256,
            "source_root":null,"expected_root":r.document.name,"semantic":null});
        let fixture: Fixture=serde_json::from_value(value.clone())?;validate(&fixture)?;
        unchanged(path,digest,&r,&root,&state)?;
        let fixture_path=r.session_directory.join(format!("calc-{letter}-fixture.json"));record(&fixture_path,&value)?;
        let result=serde_json::json!({"version":1,"phase":r.phase,"run":r.run,"account":r.account,
            "parent":p,"document":d,"registration_sha256":digest,"source":r.source,
            "fixture_path":fixture_path,"fixture_sha256":hex::encode(Sha256::digest(bytes(&fixture_path,32*1024)?)),
            "expected_representation":"data","metadata_acquired":true,"representation_verified":false,
            "current_content_verified":false,"gui_fidelity_verified":false,"journal_receipt_verified":false,
            "cloud_mutated":false,"automatic_retry":false});
        unchanged(path,digest,&r,&root,&state)?;
        record(&r.session_directory.join(format!("calc-{letter}-metadata.json")),&result)?;root.sync_all()?;Ok(result)
    }).await.map_err(|_| anyhow::anyhow!("Calc metadata window expired"))?
}
/// Independent read-only metadata; generic DATA readback proves representation/content later.
pub async fn icloud_owned_calc_metadata(path: &Path, digest: &str) -> Result<serde_json::Value> {
    observe(path, digest)
        .await
        .map_err(|_| anyhow::anyhow!("owned Calc metadata refused; retained evidence; no retry"))
}
#[cfg(test)]
mod tests;
