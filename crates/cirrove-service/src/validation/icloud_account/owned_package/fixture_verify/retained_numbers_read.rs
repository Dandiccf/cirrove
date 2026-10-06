//! Feature-only retained Numbers direct read; never a browser export or write.
use super::*;
use anyhow::Context as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum RevisionMode {
    #[default]
    ChangedOnly,
    CurrentSnapshot,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    observation_run: Uuid,
    subject_run: Uuid,
    account: Uuid,
    credential_id: Uuid,
    session_directory: PathBuf,
    root_dev: u64,
    root_ino: u64,
    settings_sha256: String,
    same_account_context_sha256: String,
    original_parent: DriveEntry,
    original_document: DriveEntry,
    expected_current_etag: Option<String>,
    #[serde(default)]
    revision_mode: RevisionMode,
    started_unix_seconds: u64,
    deadline_unix_seconds: u64,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    version: u32,
    observation_run: Uuid,
    subject_run: Uuid,
    account: Uuid,
    credential_id: Uuid,
    source_closure_sha256: String,
    sealed_session_size: u64,
    sealed_session_sha256: String,
    copied_ciphertext_only: bool,
    raw_key_or_plaintext_session_exported: bool,
    cloud_dispatched: bool,
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn bytes(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let mut value = Vec::new();
    open_private(path, false, maximum)?
        .take(maximum + 1)
        .read_to_end(&mut value)?;
    ensure!(
        value.len() as u64 <= maximum,
        "retained read input bound refused"
    );
    Ok(value)
}
fn revision(v: &str) -> bool {
    !v.is_empty() && v.len() <= 4096 && !v.chars().any(|c| c.is_control() || c == '*')
}
fn mounted(root: &Path) -> Result<bool> {
    let target = root.join("mount");
    Ok(std::fs::read_to_string("/proc/self/mountinfo")?
        .lines()
        .any(|line| {
            line.split_whitespace()
                .nth(4)
                .is_some_and(|p| Path::new(p) == target || Path::new(p).starts_with(root))
        }))
}
fn validate(r: &Registration) -> Result<()> {
    let root = format!(
        "/var/tmp/cirrove-numbers-browser-editor-{}",
        r.observation_run
    );
    let p = &r.original_parent;
    let d = &r.original_document;
    ensure!(
        r.version == 1
            && !r.observation_run.is_nil()
            && !r.subject_run.is_nil()
            && r.observation_run != r.subject_run
            && !r.account.is_nil()
            && !r.credential_id.is_nil()
            && r.session_directory.as_path() == Path::new(&root)
            && r.root_dev > 0
            && r.root_ino > 0
            && hex_digest(&r.settings_sha256)
            && hex_digest(&r.same_account_context_sha256)
            && p.name == format!("Cirrove-Native-{}", r.subject_run)
            && p.kind == "FOLDER"
            && p.zone == "com.apple.CloudDocs"
            && p.drivewsid.starts_with("FOLDER::com.apple.CloudDocs::")
            && p.drivewsid != cirrove_icloud::ROOT_ID
            && !p.drivewsid.ends_with("::TRASH_ROOT")
            && (p.parent_id.is_empty() || p.parent_id == cirrove_icloud::ROOT_ID)
            && revision(&p.etag)
            && p.items.is_empty()
            && p.number_of_items.is_none()
            && d.parent_id == p.drivewsid
            && d.kind == "FILE"
            && d.zone == "com.apple.CloudDocs"
            && d.drivewsid == format!("FILE::com.apple.CloudDocs::{}", d.docwsid)
            && !d.docwsid.is_empty()
            && d.display_name() == format!("Cirrove-Numbers-Editor-{}.numbers", r.subject_run)
            && d.extension == "numbers"
            && d.size <= LIMIT
            && d.items.is_empty()
            && revision(&d.etag)
            && r.expected_current_etag.as_deref().is_none_or(revision),
        "retained read registration refused"
    );
    let clock = now()?;
    ensure!(
        r.started_unix_seconds > 0
            && r.started_unix_seconds <= clock
            && clock < r.deadline_unix_seconds
            && r.deadline_unix_seconds
                .checked_sub(r.started_unix_seconds)
                .is_some_and(|v| v <= 1800),
        "retained read deadline refused"
    );
    Ok(())
}
fn exact_one(entries: Vec<DriveEntry>, id: &str) -> Result<DriveEntry> {
    let mut found = entries.into_iter().filter(|v| v.drivewsid == id);
    let value = found.next().context("retained item absent")?;
    ensure!(found.next().is_none(), "retained identity ambiguous");
    Ok(value)
}
fn current_document_binding(
    r: &Registration,
    parent: &DriveEntry,
    document: &DriveEntry,
) -> Result<()> {
    // exact_one already binds both selected IDs in the actual observation path;
    // retain those bindings here as well when checking a supplied typed entry.
    ensure!(
        parent.drivewsid == r.original_parent.drivewsid
            && document.drivewsid == r.original_document.drivewsid
            && document.kind == "FILE"
            && document.zone == r.original_document.zone
            && document.docwsid == r.original_document.docwsid
            && document.parent_id == parent.drivewsid
            && document.name == r.original_document.name
            && document.extension == "numbers"
            && revision(&document.etag)
            && (r.revision_mode == RevisionMode::CurrentSnapshot
                || document.etag != r.original_document.etag)
            && r.expected_current_etag
                .as_ref()
                .is_none_or(|v| v == &document.etag)
            && document.size <= LIMIT,
        "retained current identity or revision refused"
    );
    Ok(())
}
fn sealed_pin(root: &Path, r: &Registration, c: &Context) -> Result<()> {
    let path = root
        .join("state/accounts")
        .join(r.account.to_string())
        .join("icloud-session.sealed");
    let raw = bytes(&path, 128 * 1024 + 64)?;
    ensure!(
        raw.len() as u64 == c.sealed_session_size
            && hex::encode(Sha256::digest(&raw)) == c.sealed_session_sha256,
        "retained opaque session changed"
    );
    Ok(())
}
async fn observe(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let registered = bytes(path, 32 * 1024)?;
    ensure!(
        hex_digest(digest) && hex::encode(Sha256::digest(&registered)) == digest,
        "retained read registration changed"
    );
    let r: Registration = serde_json::from_slice(&registered)?;
    validate(&r)?;
    let root = &r.session_directory;
    ensure!(
        path == root.join("retained-read-registration.json")
            && !mounted(root)?
            && !root.join("control.sock").try_exists()?,
        "retained read isolation refused"
    );
    let held = open_private(root, true, 0)?;
    let m = held.metadata()?;
    ensure!(
        (m.dev(), m.ino()) == (r.root_dev, r.root_ino),
        "retained root changed"
    );
    let context_path = root.join("same-account-context.json");
    let context_raw = bytes(&context_path, 16 * 1024)?;
    ensure!(
        hex::encode(Sha256::digest(&context_raw)) == r.same_account_context_sha256,
        "retained account context changed"
    );
    let c: Context = serde_json::from_slice(&context_raw)?;
    ensure!(
        c.version == 1
            && c.observation_run == r.observation_run
            && c.subject_run == r.subject_run
            && c.account == r.account
            && c.credential_id == r.credential_id
            && hex_digest(&c.source_closure_sha256)
            && hex_digest(&c.sealed_session_sha256)
            && c.sealed_session_size > 0
            && c.sealed_session_size <= 128 * 1024 + 64
            && c.copied_ciphertext_only
            && !c.raw_key_or_plaintext_session_exported
            && !c.cloud_dispatched,
        "retained same-account provenance refused"
    );
    let settings_path = root.join("state/accounts.json");
    let settings_raw = bytes(&settings_path, 256 * 1024)?;
    ensure!(
        hex::encode(Sha256::digest(&settings_raw)) == r.settings_sha256,
        "retained settings changed"
    );
    let settings: Settings = serde_json::from_slice(&settings_raw)?;
    ensure!(
        settings.version == 2 && settings.accounts.len() == 1,
        "retained account inventory refused"
    );
    let a = &settings.accounts[0];
    ensure!(
        a.id == r.account.to_string()
            && a.credential_id == r.credential_id.to_string()
            && matches!(a.registration, AppRegistration::ICloud)
            && a.enabled
            && a.access == AccessMode::ReadOnly
            && a.drive.id == "drive"
            && a.drive.drive_type == "icloud_drive"
            && a.root_id == cirrove_icloud::ROOT_ID
            && a.mount_path == root.join("mount"),
        "retained account route refused"
    );
    for (dir, expected) in [
        (
            root.join("state"),
            vec!["accounts".to_owned(), "accounts.json".to_owned()],
        ),
        (root.join("state/accounts"), vec![r.account.to_string()]),
    ] {
        let _held_dir = open_private(&dir, true, 0)?;
        let mut found = std::fs::read_dir(&dir)?
            .take(4)
            .map(|v| v.map(|v| v.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<_>>>()?;
        found.sort();
        ensure!(found == expected, "retained state inventory refused");
    }
    let account_dir = root.join("state/accounts").join(r.account.to_string());
    let _account = open_private(&account_dir, true, 0)?;
    ensure!(
        std::fs::read_dir(&account_dir)?
            .map(|v| v.map(|v| v.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?
            == [std::ffi::OsString::from("icloud-session.sealed")],
        "retained state is not fresh"
    );
    sealed_pin(root, &r, &c)?;
    let mut attempt_value = serde_json::json!({"observation_run":r.observation_run,
        "subject_run":r.subject_run,"registration_sha256":digest,"provider_mutation":false,"automatic_retry":false});
    if r.revision_mode == RevisionMode::CurrentSnapshot {
        attempt_value["revision_mode"] = serde_json::json!("current_snapshot");
    }
    record(&root.join("retained-read.attempt.json"), &attempt_value)?;
    held.sync_all()?;
    let remaining = r
        .deadline_unix_seconds
        .checked_sub(now()?)
        .context("retained window expired")?;
    ensure!(remaining > 0, "retained window expired");
    tokio::time::timeout(Duration::from_secs(remaining), async {
        let mut remote = session(root, a).await?;
        let mut parent = exact_one(remote.list_root().await?, &r.original_parent.drivewsid)?;
        ensure!(parent.kind == "FOLDER" && parent.zone == r.original_parent.zone
            && parent.name == r.original_parent.name && parent.display_name() == r.original_parent.display_name()
            && (parent.parent_id.is_empty() || parent.parent_id == cirrove_icloud::ROOT_ID), "retained parent moved");
        parent.items.clear(); parent.number_of_items = None;
        let document = exact_one(remote.list_folder(&parent.drivewsid).await?, &r.original_document.drivewsid)?;
        current_document_binding(&r, &parent, &document)?;
        let output = root.join("source-b.numbers");
        let file = OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&output)?;
        let mut sink = DiskSink(tokio::fs::File::from_std(file.try_clone()?));
        let receipt = remote.read_owned_fixture(&parent, &document, FixtureRepresentation::Package,
            &mut sink, &CancellationToken::new()).await?;
        sink.0.flush().await?; sink.0.sync_all().await?; drop(sink);
        let source_root = document.display_name();
        let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(&file, &receipt,
            &source_root, 2, &CancellationToken::new())?;
        file.set_permissions(std::fs::Permissions::from_mode(0o400))?; file.sync_all()?;
        ensure!(bytes(path, 32 * 1024)? == registered && bytes(&context_path, 16 * 1024)? == context_raw
            && bytes(&settings_path, 256 * 1024)? == settings_raw && !mounted(root)?
            && !root.join("control.sock").try_exists()?, "retained inputs changed");
        sealed_pin(root, &r, &c)?; same_directory(root, &held)?; validate(&r)?;
        let fixture = serde_json::json!({"version":1,"run":r.subject_run,"account":r.account,
            "session_directory":root,"settings_sha256":r.settings_sha256,"parent":parent,"document":document,
            "format":"numbers","representation":"package","source":output,"source_size":receipt.size,
            "source_sha256":receipt.sha256,"source_root":source_root,"expected_root":source_root,"semantic":semantic});
        let typed: Fixture = serde_json::from_value(fixture.clone())?;
        super::validate(&typed)?;
        let fixture_path = root.join("retained-current-fixture.json"); record(&fixture_path, &fixture)?;
        let mut result = serde_json::json!({"version":1,"observation_run":r.observation_run,"subject_run":r.subject_run,
            "account":r.account,"collection":"drive","parent":parent,"document":document,"registration_sha256":digest,
            "source":{"path":output,"size":receipt.size,"sha256":receipt.sha256,"root":source_root,"semantic":semantic},
            "fixture_path":fixture_path,"fixture_sha256":hex::encode(Sha256::digest(bytes(&fixture_path,32*1024)?)),
            "reference_origin":"current-provider-read","actual_representation":"package","current_read_fenced":true,
            "browser_export_verified":false,"independently_decoded_cells":false,"cloud_mutated":false,"automatic_retry":false});
        if r.revision_mode == RevisionMode::CurrentSnapshot {
            result["revision_mode"] = serde_json::json!("current_snapshot");
        }
        record(&root.join("retained-current-read.json"), &result)?; held.sync_all()?; Ok(result)
    }).await.map_err(|_| anyhow::anyhow!("retained read deadline"))?
}
/// Distinct feature-only observation; no operation enqueue or mutation router.
pub async fn icloud_owned_retained_numbers_read(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    observe(path, digest).await.map_err(|_| {
        anyhow::anyhow!("owned retained Numbers read refused; preserve partial artifacts; no retry")
    })
}

#[cfg(test)]
mod admission_tests;
