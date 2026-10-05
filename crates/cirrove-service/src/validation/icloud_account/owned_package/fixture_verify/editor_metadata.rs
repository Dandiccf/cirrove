//! Feature-only saved-session metadata capture for one fresh browser document.
//! Representation/content is verified later by the unchanged generic reader.
use super::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: PathBuf,
    size: u64,
    sha256: String,
    #[serde(deserialize_with = "super::required_source_root")]
    root: Option<String>,
    semantic: Option<PackageSemanticIdentity>,
}
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    A,
    B,
}
impl Phase {
    fn stem(self) -> &'static str {
        match self {
            Self::A => "editor-a",
            Self::B => "editor-b",
        }
    }
    fn source_name(self) -> &'static str {
        match self {
            Self::A => "source-a.numbers",
            Self::B => "source-b.numbers",
        }
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    phase: Phase,
    prior_a_sha256: Option<String>,
    run: Uuid,
    account: Uuid,
    session_directory: PathBuf,
    root_dev: u64,
    root_ino: u64,
    settings_sha256: String,
    representation: FixtureRepresentation,
    source: Source,
    expected_parent_id: Option<String>,
    expected_document_id: Option<String>,
    expected_document_etag: Option<String>,
    started_unix_seconds: u64,
    deadline_unix_seconds: u64,
}
fn clock() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
}
fn etag(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(|c| c.is_control() || c == '*')
}
fn read_private(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_private(path, false, maximum)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= maximum,
        "editor private input bound refused"
    );
    Ok(bytes)
}
fn registered(bytes: &[u8], digest: &str, now: u64) -> Result<Registration> {
    ensure!(
        bytes.len() <= 32 * 1024
            && hex_digest(digest)
            && hex::encode(Sha256::digest(bytes)) == digest,
        "editor registration changed"
    );
    let r: Registration = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("editor registration schema refused"))?;
    let expected_directory = format!("/var/tmp/cirrove-numbers-browser-editor-{}", r.run);
    ensure!(
        r.version == 1
            && !r.run.is_nil()
            && !r.account.is_nil()
            && r.session_directory == Path::new(&expected_directory)
            && r.root_dev > 0
            && r.root_ino > 0
            && hex_digest(&r.settings_sha256)
            && r.source.path == r.session_directory.join(r.phase.source_name())
            && r.source.size > 0
            && r.source.size <= LIMIT
            && hex_digest(&r.source.sha256)
            && r.source
                .root
                .as_ref()
                .is_none_or(|root| root.ends_with(".numbers")
                    && root.len() <= 255
                    && !root.contains(['/', '\\'])
                    && !root.chars().any(char::is_control))
            && r.expected_parent_id
                .as_ref()
                .is_none_or(|v| id(v, "FOLDER::com.apple.CloudDocs::")
                    && v != cirrove_icloud::ROOT_ID
                    && !v.ends_with("::TRASH_ROOT"))
            && r.expected_document_id
                .as_ref()
                .is_none_or(|v| id(v, "FILE::com.apple.CloudDocs::"))
            && r.expected_document_etag.as_ref().is_none_or(|v| etag(v)),
        "editor registration scope refused"
    );
    ensure!(
        matches!((r.phase, r.prior_a_sha256.as_deref()), (Phase::A, None))
            || (r.phase == Phase::B && r.prior_a_sha256.as_deref().is_some_and(hex_digest)),
        "editor prior-phase binding refused"
    );
    match (&r.representation, &r.source.semantic) {
        (FixtureRepresentation::Data, None) => {}
        (FixtureRepresentation::Package, Some(v)) => {
            v.validate()?;
            ensure!(v.version == 2, "editor source requires semantic v2");
        }
        _ => anyhow::bail!("editor registered source representation refused"),
    }
    ensure!(
        r.started_unix_seconds > 0
            && r.started_unix_seconds <= now
            && now < r.deadline_unix_seconds
            && r.deadline_unix_seconds
                .checked_sub(r.started_unix_seconds)
                .is_some_and(|v| v > 0 && v <= 1800),
        "editor finite window refused"
    );
    Ok(r)
}
fn account(r: &Registration, bytes: &[u8]) -> Result<Account> {
    ensure!(
        hex::encode(Sha256::digest(bytes)) == r.settings_sha256,
        "editor settings changed"
    );
    let settings: Settings = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("editor settings schema refused"))?;
    ensure!(
        settings.version == 2 && settings.accounts.len() == 1,
        "editor account inventory refused"
    );
    let a = settings.accounts[0].clone();
    ensure!(
        a.id == r.account.to_string()
            && matches!(a.registration, AppRegistration::ICloud)
            && a.label == "iCloudNumbersBrowserValidation"
            && a.enabled
            && a.access == AccessMode::ReadOnly
            && a.drive.id == "drive"
            && a.drive.drive_type == "icloud_drive"
            && a.root_id == cirrove_icloud::ROOT_ID
            && a.mount_path == r.session_directory.join("mount"),
        "editor read-only account refused"
    );
    Ok(a)
}
fn source_verified(r: &Registration) -> Result<()> {
    let mut file = open_private(&r.source.path, false, LIMIT)?;
    ensure!(
        file.metadata()?.permissions().mode() & 0o777 == 0o400,
        "editor source mode refused"
    );
    let receipt = PackageDownload {
        size: file.metadata()?.len(),
        sha256: sha256(&mut file)?,
    };
    ensure!(
        receipt.size == r.source.size && receipt.sha256 == r.source.sha256,
        "editor source changed"
    );
    if let Some(expected) = &r.source.semantic {
        let actual = match r.source.root.as_deref() {
            Some(root) => cirrove_icloud::package_archive_semantic_identity_versioned(
                &file,
                &receipt,
                root,
                2,
                &CancellationToken::new(),
            )?,
            None => cirrove_icloud::package_flat_archive_semantic_identity_v2(
                &file,
                &receipt,
                &CancellationToken::new(),
            )?,
        };
        ensure!(actual == *expected, "editor source semantics refused");
    }
    Ok(())
}
fn select_parent(entries: &[DriveEntry], r: &Registration) -> Result<DriveEntry> {
    let name = format!("Cirrove-Native-{}", r.run);
    let mut candidates = entries.iter().filter(|v| v.display_name() == name);
    let mut p = candidates.next().context("editor parent absent")?.clone();
    ensure!(
        candidates.next().is_none()
            && p.kind == "FOLDER"
            && p.zone == "com.apple.CloudDocs"
            && id(&p.drivewsid, "FOLDER::com.apple.CloudDocs::")
            && p.drivewsid != cirrove_icloud::ROOT_ID
            && !p.drivewsid.ends_with("::TRASH_ROOT")
            && etag(&p.etag)
            && (p.parent_id.is_empty() || p.parent_id == cirrove_icloud::ROOT_ID)
            && entries
                .iter()
                .filter(|v| v.drivewsid == p.drivewsid)
                .count()
                == 1
            && r.expected_parent_id
                .as_ref()
                .is_none_or(|v| v == &p.drivewsid),
        "editor parent scope refused"
    );
    p.items.clear();
    p.number_of_items = None;
    Ok(p)
}
fn select_document(
    entries: &[DriveEntry],
    parent: &DriveEntry,
    r: &Registration,
) -> Result<DriveEntry> {
    let name = format!("Cirrove-Numbers-Editor-{}.numbers", r.run);
    let mut candidates = entries.iter().filter(|v| v.display_name() == name);
    let mut d = candidates.next().context("editor document absent")?.clone();
    ensure!(
        candidates.next().is_none()
            && d.kind == "FILE"
            && d.zone == "com.apple.CloudDocs"
            && !d.docwsid.is_empty()
            && id(&d.drivewsid, "FILE::com.apple.CloudDocs::")
            && d.drivewsid == format!("FILE::com.apple.CloudDocs::{}", d.docwsid)
            && d.parent_id == parent.drivewsid
            && d.extension == "numbers"
            && etag(&d.etag)
            && d.size <= LIMIT
            && entries
                .iter()
                .filter(|v| v.drivewsid == d.drivewsid)
                .count()
                == 1
            && r.expected_document_id
                .as_ref()
                .is_none_or(|v| v == &d.drivewsid)
            && r.expected_document_etag
                .as_ref()
                .is_none_or(|v| v == &d.etag),
        "editor document scope refused"
    );
    // Narrow child metadata, preserving every other actual optional field.
    d.items.clear();
    Ok(d)
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: u32,
    phase: Phase,
    run: Uuid,
    account: Uuid,
    registration_sha256: String,
    parent: DriveEntry,
    document: DriveEntry,
    fixture_path: PathBuf,
    fixture_sha256: String,
    source: Source,
    expected_representation: FixtureRepresentation,
    prior_a_sha256: Option<String>,
    prior_document_etag: Option<String>,
    metadata_acquired: bool,
    representation_verified: bool,
    current_content_verified: bool,
    gui_fidelity_verified: bool,
    journal_receipt_verified: bool,
    cloud_mutated: bool,
    automatic_retry: bool,
}
fn prior_binding(r: &Registration, a: &Registration, m: &Metadata, digest: &str) -> Result<()> {
    ensure!(
        r.phase == Phase::B
            && a.phase == Phase::A
            && m.version == 1
            && m.phase == Phase::A
            && m.run == r.run
            && m.account == r.account
            && a.run == r.run
            && a.account == r.account
            && a.session_directory == r.session_directory
            && a.root_dev == r.root_dev
            && a.root_ino == r.root_ino
            && a.settings_sha256 == r.settings_sha256
            && a.started_unix_seconds == r.started_unix_seconds
            && a.deadline_unix_seconds == r.deadline_unix_seconds
            && m.registration_sha256 == digest
            && m.fixture_path == r.session_directory.join("editor-a-fixture.json")
            && hex_digest(&m.fixture_sha256)
            && m.source.path == a.source.path
            && m.source.size == a.source.size
            && m.source.sha256 == a.source.sha256
            && m.source.root == a.source.root
            && m.source.semantic == a.source.semantic
            && m.expected_representation == a.representation
            && m.prior_a_sha256.is_none()
            && m.prior_document_etag.is_none()
            && m.metadata_acquired
            && !m.representation_verified
            && !m.current_content_verified
            && !m.gui_fidelity_verified
            && !m.journal_receipt_verified
            && !m.cloud_mutated
            && !m.automatic_retry,
        "editor prior-A scope refused"
    );
    ensure!(
        select_parent(std::slice::from_ref(&m.parent), a)? == m.parent
            && select_document(std::slice::from_ref(&m.document), &m.parent, a)? == m.document,
        "editor prior-A metadata refused"
    );
    Ok(())
}
fn prior_a(r: &Registration) -> Result<Option<Metadata>> {
    let Some(expected) = &r.prior_a_sha256 else {
        return Ok(None);
    };
    let bytes = read_private(
        &r.session_directory.join("editor-a-metadata.json"),
        32 * 1024,
    )?;
    ensure!(
        hex::encode(Sha256::digest(&bytes)) == *expected,
        "editor prior-A capture changed"
    );
    let m: Metadata = serde_json::from_slice(&bytes)?;
    let registration_path = r.session_directory.join("editor-a-registration.json");
    let a = registered(
        &read_private(&registration_path, 32 * 1024)?,
        &m.registration_sha256,
        clock()?,
    )?;
    prior_binding(r, &a, &m, &m.registration_sha256)?;
    source_verified(&a)?;
    ensure!(
        hex::encode(Sha256::digest(read_private(&m.fixture_path, 32 * 1024)?)) == m.fixture_sha256,
        "editor prior-A fixture changed"
    );
    Ok(Some(m))
}
fn continuity(prior: Option<&Metadata>, parent: &DriveEntry, document: &DriveEntry) -> Result<()> {
    if let Some(a) = prior {
        ensure!(
            parent.drivewsid == a.parent.drivewsid
                && document.drivewsid == a.document.drivewsid
                && document.docwsid == a.document.docwsid
                && document.parent_id == a.document.parent_id
                && document.name == a.document.name
                && document.extension == a.document.extension
                && document.etag != a.document.etag,
            "editor changed-revision continuity refused"
        );
    }
    Ok(())
}
fn unchanged(path: &Path, digest: &str, r: &Registration, root: &File, state: &File) -> Result<()> {
    let _ = registered(&read_private(path, 32 * 1024)?, digest, clock()?)?;
    let _ = account(
        r,
        &read_private(&r.session_directory.join("state/accounts.json"), 256 * 1024)?,
    )?;
    source_verified(r)?;
    let _ = prior_a(r)?;
    same_directory(&r.session_directory, root)?;
    same_directory(&r.session_directory.join("state"), state)?;
    Ok(())
}
async fn observe(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let r = registered(&read_private(path, 32 * 1024)?, digest, clock()?)?;
    ensure!(
        path == r
            .session_directory
            .join(format!("{}-registration.json", r.phase.stem())),
        "editor registration path refused"
    );
    let root = open_private(&r.session_directory, true, 0)?;
    let state = open_private(&r.session_directory.join("state"), true, 0)?;
    let meta = root.metadata()?;
    ensure!(
        meta.dev() == r.root_dev && meta.ino() == r.root_ino,
        "editor root pin changed"
    );
    let a = account(
        &r,
        &read_private(&r.session_directory.join("state/accounts.json"), 256 * 1024)?,
    )?;
    let _accounts = open_private(&r.session_directory.join("state/accounts"), true, 0)?;
    let _account = open_private(
        &r.session_directory
            .join("state/accounts")
            .join(r.account.to_string()),
        true,
        0,
    )?;
    source_verified(&r)?;
    let prior = prior_a(&r)?;
    unchanged(path, digest, &r, &root, &state)?;
    record(
        &r.session_directory
            .join(format!("{}.attempt.json", r.phase.stem())),
        &serde_json::json!({
        "run":r.run,"phase":r.phase,"account":r.account,"registration_sha256":digest,"provider_mutation":false,"automatic_retry":false}),
    )?;
    root.sync_all()?;
    let remaining = r
        .deadline_unix_seconds
        .checked_sub(clock()?)
        .context("editor window expired")?;
    ensure!(remaining > 0, "editor window expired");
    tokio::time::timeout(Duration::from_secs(remaining), async {
        // Saved sealed session only; no login, mutation router or recovery API.
        let mut remote = session(&r.session_directory, &a).await?;
        let parent = select_parent(&remote.list_root().await?, &r)?;
        let document = select_document(&remote.list_folder(&parent.drivewsid).await?, &parent, &r)?;
        continuity(prior.as_ref(), &parent, &document)?;
        ensure!(select_document(&remote.list_folder(&parent.drivewsid).await?, &parent, &r)? == document,
            "editor document metadata changed");
        ensure!(select_parent(&remote.list_root().await?, &r)? == parent, "editor parent metadata changed");
        unchanged(path, digest, &r, &root, &state)?;
        let fixture_value = serde_json::json!({"version":1,"run":r.run,"account":r.account,
            "session_directory":r.session_directory,"settings_sha256":r.settings_sha256,
            "parent":parent,"document":document,"format":"numbers","representation":r.representation,
            "source":r.source.path,"source_size":r.source.size,"source_sha256":r.source.sha256,
            "source_root":r.source.root,"expected_root":format!("Cirrove-Numbers-Editor-{}.numbers",r.run),
            "semantic":r.source.semantic});
        let fixture: Fixture = serde_json::from_value(fixture_value.clone())?;
        validate(&fixture)?;
        let fixture_path = r.session_directory.join(format!("{}-fixture.json",r.phase.stem()));
        record(&fixture_path, &fixture_value)?;
        let fixture_digest = hex::encode(Sha256::digest(read_private(&fixture_path, 32 * 1024)?));
        let result = serde_json::json!({"version":1,"phase":r.phase,"run":r.run,"account":r.account,"registration_sha256":digest,
            "parent":parent,"document":document,"fixture_path":fixture_path,"fixture_sha256":fixture_digest,
            "source":r.source,"expected_representation":r.representation,
            "prior_a_sha256":r.prior_a_sha256,"prior_document_etag":prior.as_ref().map(|a| &a.document.etag),"metadata_acquired":true,
            "representation_verified":false,"current_content_verified":false,"gui_fidelity_verified":false,
            "journal_receipt_verified":false,"cloud_mutated":false,"automatic_retry":false});
        unchanged(path, digest, &r, &root, &state)?;
        record(&r.session_directory.join(format!("{}-metadata.json",r.phase.stem())), &result)?;
        root.sync_all()?;
        Ok(result)
    }).await.map_err(|_| anyhow::anyhow!("editor metadata deadline exceeded"))?
}
/// Metadata only: the separate generic fixture reader proves actual content.
pub async fn icloud_owned_editor_metadata(path: &Path, digest: &str) -> Result<serde_json::Value> {
    observe(path, digest)
        .await
        .map_err(|_| anyhow::anyhow!("owned iCloud editor metadata refused"))
}

#[cfg(test)]
mod tests;
