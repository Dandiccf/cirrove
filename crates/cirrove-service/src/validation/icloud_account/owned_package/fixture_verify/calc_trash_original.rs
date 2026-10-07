//! Feature-only exact original Calc DATA Trash read. No journal open or mutation.
use super::super::public_verify::exact_entry;
use super::calc_metadata::EditorArm;
use super::*;
use crate::journal::UploadRecord;
use cirrove_core::upload::UploadIntent;
use cirrove_core::{Node, NodeKind};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    path: PathBuf,
    sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: PathBuf,
    size: u64,
    sha256: String,
    #[serde(deserialize_with = "super::required_source_root")]
    root: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    observer_run: Uuid,
    writer_run: Uuid,
    account: Uuid,
    collection: String,
    session_directory: PathBuf,
    root_dev: u64,
    root_ino: u64,
    settings_sha256: String,
    parent: Node,
    original_a: Node,
    backup_a: Node,
    target_operation: Uuid,
    source_a: Source,
    prior_a_fixture: Pin,
    prior_a_read_receipt: Pin,
    completed_target_upload: Pin,
    started_unix_seconds: u64,
    deadline_unix_seconds: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ARead {
    run: Uuid,
    account: Uuid,
    parent: String,
    item: String,
    etag: String,
    representation: FixtureRepresentation,
    format: String,
    size: u64,
    sha256: String,
    semantic: Option<PackageSemanticIdentity>,
    manifest_sha256: String,
    content_identity_verified: bool,
    gui_fidelity_verified: bool,
    cloud_mutated: bool,
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
// Existing nested Node/UploadRecord structs tolerate unknown fields. Compare all
// supplied keys with their typed serialization, recursively, before accepting.
fn no_unknown(value: &serde_json::Value, typed: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Object(fields) => {
            let known = typed
                .as_object()
                .context("Calc Trash typed object refused")?;
            for (key, child) in fields {
                if let Some(actual) = known.get(key) {
                    no_unknown(child, actual)?;
                } else if key == "package"
                    && known.contains_key("id")
                    && known.contains_key("kind")
                    && child == &serde_json::json!(false)
                {
                    // Node skips this known false field when serialized.
                } else if key == "representation"
                    && known.contains_key("intent")
                    && known.contains_key("state")
                    && child
                        == &serde_json::to_value(
                            cirrove_core::upload::UploadRepresentation::FileBytes,
                        )?
                {
                    // UploadRecord skips the known default ordinary representation.
                } else {
                    anyhow::bail!("Calc Trash unknown field refused");
                }
            }
        }
        serde_json::Value::Array(fields) => {
            let known = typed.as_array().context("Calc Trash typed array refused")?;
            ensure!(
                fields.len() == known.len(),
                "Calc Trash typed array changed"
            );
            for (child, actual) in fields.iter().zip(known) {
                no_unknown(child, actual)?;
            }
        }
        _ => ensure!(value == typed, "Calc Trash typed value changed"),
    }
    Ok(())
}
fn ordinary(n: &Node) -> bool {
    n.kind == NodeKind::File
        && !n.package
        && n.target.is_none()
        && n.id.starts_with("FILE::com.apple.CloudDocs::")
        && n.id.len() > "FILE::com.apple.CloudDocs::".len()
        && n.id.len() <= 4096
        && !n.id.chars().any(char::is_control)
        && n.etag.as_ref().is_some_and(|v| {
            !v.is_empty() && v.len() <= 4096 && !v.chars().any(|c| c.is_control() || c == '*')
        })
        && n.content_revision().is_some()
}
fn active_remaining_for(r: &Registration, clock: u64, arm: EditorArm) -> Result<u64> {
    ensure!(
        r.started_unix_seconds > 0
            && r.started_unix_seconds <= clock
            && r.deadline_unix_seconds
                .checked_sub(r.started_unix_seconds)
                .is_some_and(|n| n > (if arm == EditorArm::Writer { 45 } else { 20 })
                    && n <= (if arm == EditorArm::Writer { 600 } else { 90 })),
        "Calc Trash original window refused"
    );
    let active_end = r
        .deadline_unix_seconds
        .checked_sub(if arm == EditorArm::Writer { 45 } else { 20 })
        .context("Calc Trash window refused")?;
    ensure!(clock < active_end, "Calc Trash cleanup reserve reached");
    Ok(active_end - clock)
}
fn registered_for(raw: &[u8], digest: &str, clock: u64, arm: EditorArm) -> Result<Registration> {
    ensure!(
        raw.len() <= 32 * 1024 && hex_digest(digest) && hex::encode(Sha256::digest(raw)) == digest,
        "Calc Trash registration digest refused"
    );
    let value: serde_json::Value = serde_json::from_slice(raw)?;
    let r: Registration = serde_json::from_slice(raw)?;
    no_unknown(&value, &serde_json::to_value(&r)?)?;
    ensure!(
        r.version == 1
            && !r.observer_run.is_nil()
            && !r.writer_run.is_nil()
            && r.observer_run != r.writer_run
            && !r.account.is_nil()
            && !r.target_operation.is_nil()
            && r.collection == "drive"
            && r.session_directory == arm.trash_root(r.observer_run)
            && r.root_dev > 0
            && r.root_ino > 0
            && hex_digest(&r.settings_sha256),
        "Calc Trash route refused"
    );
    let parent = &r.parent;
    ensure!(
        parent.kind == NodeKind::Folder
            && !parent.package
            && parent.target.is_none()
            && parent.id.starts_with("FOLDER::com.apple.CloudDocs::")
            && parent.id.len() > "FOLDER::com.apple.CloudDocs::".len()
            && parent.id != cirrove_icloud::ROOT_ID
            && !parent.id.ends_with("::TRASH_ROOT")
            && parent.id.len() <= 4096
            && !parent.id.chars().any(char::is_control)
            && parent.parent_id.as_deref() == Some(cirrove_icloud::ROOT_ID)
            && parent.name == format!("Cirrove-Native-{}", r.writer_run),
        "Calc Trash parent refused"
    );
    ensure!(
        ordinary(&r.original_a)
            && ordinary(&r.backup_a)
            && r.original_a.parent_id.as_ref() == Some(&parent.id)
            && r.original_a.name == arm.document_name(r.writer_run)
            && r.backup_a.parent_id.as_deref() == Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT")
            && r.backup_a.id == r.original_a.id
            && r.backup_a.name == r.original_a.name
            && r.original_a.size == r.source_a.size
            && r.backup_a.size == r.source_a.size
            && r.source_a.size > 0
            && r.source_a.size <= LIMIT
            && hex_digest(&r.source_a.sha256)
            && r.source_a.root.is_none()
            && r.source_a.path == r.session_directory.join(arm.source_name("a")),
        "Calc Trash original DATA identity refused"
    );
    for (pin, name) in [
        (&r.prior_a_fixture, "prior-a-fixture.json"),
        (&r.prior_a_read_receipt, "prior-a-read-receipt.json"),
        (&r.completed_target_upload, "target-upload.json"),
    ] {
        ensure!(
            pin.path == r.session_directory.join(name) && hex_digest(&pin.sha256),
            "Calc Trash proof path refused"
        );
    }
    active_remaining_for(&r, clock, arm)?;
    Ok(r)
}
#[cfg(test)]
fn active_remaining(r: &Registration, clock: u64) -> Result<u64> {
    active_remaining_for(r, clock, EditorArm::Calc)
}
#[cfg(test)]
fn registered(raw: &[u8], digest: &str, clock: u64) -> Result<Registration> {
    registered_for(raw, digest, clock, EditorArm::Calc)
}
fn read_bytes(path: &Path, cap: u64, immutable: bool) -> Result<Vec<u8>> {
    let file = open_private(path, false, cap)?;
    if immutable {
        ensure!(
            file.metadata()?.permissions().mode() & 0o777 == 0o400,
            "Calc Trash immutable input refused"
        );
    }
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= cap, "Calc Trash input bound refused");
    Ok(bytes)
}
fn pinned(pin: &Pin, cap: u64) -> Result<Vec<u8>> {
    let raw = read_bytes(&pin.path, cap, true)?;
    ensure!(
        hex::encode(Sha256::digest(&raw)) == pin.sha256,
        "Calc Trash proof changed"
    );
    Ok(raw)
}
fn account(r: &Registration, raw: &[u8]) -> Result<Account> {
    ensure!(
        hex::encode(Sha256::digest(raw)) == r.settings_sha256,
        "Calc Trash settings changed"
    );
    let settings: Settings = serde_json::from_slice(raw)?;
    no_unknown(
        &serde_json::from_slice(raw)?,
        &serde_json::to_value(&settings)?,
    )?;
    ensure!(
        settings.version == 2 && settings.accounts.len() == 1,
        "Calc Trash settings inventory refused"
    );
    let a = settings.accounts[0].clone();
    ensure!(
        a.id == r.account.to_string()
            && matches!(a.registration, AppRegistration::ICloud)
            && a.access == AccessMode::ReadOnly
            && a.enabled
            && a.drive.id == r.collection
            && a.drive.drive_type == "icloud_drive"
            && a.root_id == cirrove_icloud::ROOT_ID
            && a.mount_path == r.session_directory.join("mount")
            && !Uuid::parse_str(&a.credential_id)?.is_nil(),
        "Calc Trash read-only account refused"
    );
    Ok(a)
}
fn account_for(r: &Registration, raw: &[u8], arm: EditorArm) -> Result<Account> {
    let a = account(r, raw)?;
    ensure!(
        arm == EditorArm::Calc || a.label == "iCloudWriterTrashValidation",
        "Writer Trash label refused"
    );
    Ok(a)
}
fn historical_for(
    r: &Registration,
    fixture_raw: &[u8],
    proof_raw: &[u8],
    upload_raw: &[u8],
    arm: EditorArm,
) -> Result<()> {
    let f = decode(fixture_raw, &r.prior_a_fixture.sha256)?;
    validate(&f)?;
    let fixture_value: serde_json::Value = serde_json::from_slice(fixture_raw)?;
    no_unknown(&fixture_value["parent"], &serde_json::to_value(&f.parent)?)?;
    no_unknown(
        &fixture_value["document"],
        &serde_json::to_value(&f.document)?,
    )?;
    ensure!(
        f.run == r.writer_run
            && f.account == r.account
            && f.session_directory == arm.editor_root(r.writer_run)
            && f.format == arm.extension()
            && f.representation == FixtureRepresentation::Data
            && f.semantic.is_none()
            && f.source_root.is_none()
            && f.source == f.session_directory.join(arm.source_name("a"))
            && f.source_size == r.source_a.size
            && f.source_sha256 == r.source_a.sha256
            && f.parent.drivewsid == r.parent.id
            && f.parent.display_name() == r.parent.name
            && f.expected_root == r.original_a.name,
        "Calc Trash fresh A fixture refused"
    );
    exact_entry(std::slice::from_ref(&f.document), &r.original_a)?;
    let p: ARead = serde_json::from_slice(proof_raw)?;
    ensure!(
        p.run == r.writer_run
            && p.account == r.account
            && p.parent == r.parent.id
            && p.item == r.original_a.id
            && Some(&p.etag) == r.original_a.etag.as_ref()
            && p.representation == FixtureRepresentation::Data
            && p.format == arm.extension()
            && p.size == r.source_a.size
            && p.sha256 == r.source_a.sha256
            && p.semantic.is_none()
            && p.manifest_sha256 == r.prior_a_fixture.sha256
            && p.content_identity_verified
            && !p.gui_fidelity_verified
            && !p.cloud_mutated,
        "Calc Trash independent A proof refused"
    );
    let value: serde_json::Value = serde_json::from_slice(upload_raw)?;
    let row: UploadRecord = serde_json::from_slice(upload_raw)?;
    no_unknown(&value, &serde_json::to_value(&row)?)?;
    let (current, backup) = row
        .ordinary_handoff_receipt()
        .context("Calc Trash completed target receipt absent")?;
    ensure!(
        row.id == r.target_operation
            && row.sequence > 0
            && row.scope
                == (Scope {
                    account: r.account.to_string(),
                    provider: "icloud".into(),
                    collection: r.collection.clone()
                })
            && row.transferred_bytes == row.size
            && row.failed_attempts == 0
            && hex_digest(&row.sha256)
            && row.package_completion.is_none()
            && matches!(&row.intent,UploadIntent::Replace{item,expected_etag}
            if item == &r.original_a.id && Some(expected_etag) == r.original_a.etag.as_ref())
            && ordinary(current)
            && current.id != r.original_a.id
            && current.parent_id == r.original_a.parent_id
            && current.name == r.original_a.name
            && backup == &r.backup_a,
        "Calc Trash target backup lineage refused"
    );
    Ok(())
}

#[cfg(test)]
fn historical(
    r: &Registration,
    fixture_raw: &[u8],
    proof_raw: &[u8],
    upload_raw: &[u8],
) -> Result<()> {
    historical_for(r, fixture_raw, proof_raw, upload_raw, EditorArm::Calc)
}
type SourceStamp = (u64, u64, u64, u32, u32, i64, i64, i64, i64);
fn source_stamp(m: &std::fs::Metadata) -> SourceStamp {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.uid(),
        m.mode(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn source(r: &Registration, held: &mut File) -> Result<()> {
    let before = held.metadata()?;
    ensure!(
        before.len() == r.source_a.size && before.permissions().mode() & 0o777 == 0o400,
        "Calc Trash source mode/size refused"
    );
    use std::io::{Seek, SeekFrom};
    held.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = held.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        ensure!(count <= r.source_a.size, "Calc Trash source grew");
        hash.update(&buffer[..n]);
    }
    let after = held.metadata()?;
    let current = open_private(&r.source_a.path, false, LIMIT)?.metadata()?;
    ensure!(
        count == r.source_a.size
            && hex::encode(hash.finalize()) == r.source_a.sha256
            && source_stamp(&before) == source_stamp(&after)
            && source_stamp(&before) == source_stamp(&current),
        "Calc Trash source changed"
    );
    Ok(())
}
fn immutable_record(path: &Path, value: &serde_json::Value) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(path)?;
    f.write_all(&serde_json::to_vec_pretty(value)?)?;
    f.sync_all()?;
    Ok(())
}
fn end_fences(
    path: &Path,
    digest: &str,
    r: &Registration,
    root: &File,
    state: &File,
    held: &mut File,
    arm: EditorArm,
) -> Result<()> {
    registered_for(&read_bytes(path, 32 * 1024, true)?, digest, now()?, arm)?;
    account_for(
        r,
        &read_bytes(
            &r.session_directory.join("state/accounts.json"),
            256 * 1024,
            false,
        )?,
        arm,
    )?;
    historical_for(
        r,
        &pinned(&r.prior_a_fixture, 32 * 1024)?,
        &pinned(&r.prior_a_read_receipt, 32 * 1024)?,
        &pinned(&r.completed_target_upload, 128 * 1024)?,
        arm,
    )?;
    source(r, held)?;
    same_directory(&r.session_directory, root)?;
    same_directory(&r.session_directory.join("state"), state)
}
async fn observe(path: &Path, digest: &str, arm: EditorArm) -> Result<serde_json::Value> {
    let r = registered_for(&read_bytes(path, 32 * 1024, true)?, digest, now()?, arm)?;
    ensure!(
        path == r.session_directory.join("trash-original-registration.json"),
        "Calc Trash registration path refused"
    );
    let root = open_private(&r.session_directory, true, 0)?;
    let m = root.metadata()?;
    ensure!(
        (m.dev(), m.ino()) == (r.root_dev, r.root_ino),
        "Calc Trash root changed"
    );
    let state = open_private(&r.session_directory.join("state"), true, 0)?;
    let a = account_for(
        &r,
        &read_bytes(
            &r.session_directory.join("state/accounts.json"),
            256 * 1024,
            false,
        )?,
        arm,
    )?;
    let mut held = open_private(&r.source_a.path, false, LIMIT)?;
    end_fences(path, digest, &r, &root, &state, &mut held, arm)?;
    let initial_source_stamp = source_stamp(&held.metadata()?);
    immutable_record(
        &r.session_directory.join("trash-original.attempt.json"),
        &serde_json::json!({"observer_run":r.observer_run,"writer_run":r.writer_run,"account":r.account,
            "registration_sha256":digest,"cloud_mutated":false,"automatic_retry":false}),
    )?;
    root.sync_all()?;
    let remaining = active_remaining_for(&r, now()?, arm)?;
    let observed = tokio::time::timeout(Duration::from_secs(remaining), async {
        active_remaining_for(&r, now()?, arm)?;
        let mut remote = session(&r.session_directory, &a).await?;
        active_remaining_for(&r, now()?, arm)?;
        remote
            .verify_owned_data_in_trash(&r.original_a, &r.backup_a, &r.source_a.sha256)
            .await
    })
    .await
    .map_err(|_| anyhow::anyhow!("Calc Trash original window expired"))??;
    ensure!(observed == r.backup_a, "Calc Trash observed backup changed");
    ensure!(
        source_stamp(&held.metadata()?) == initial_source_stamp,
        "Calc Trash source changed during observation"
    );
    end_fences(path, digest, &r, &root, &state, &mut held, arm)?;
    let result = serde_json::json!({"version":1,"observer_run":r.observer_run,"writer_run":r.writer_run,
        "account":r.account,"collection":r.collection,"node_zone":"com.apple.CloudDocs",
        "original_item":r.original_a.id,"backup":observed,"source_a_size":r.source_a.size,
        "source_a_sha256":r.source_a.sha256,"registration_sha256":digest,
        "prior_a_fixture_sha256":r.prior_a_fixture.sha256,"prior_a_read_receipt_sha256":r.prior_a_read_receipt.sha256,
        "target_upload_sha256":r.completed_target_upload.sha256,"trash_presence_and_raw_identity_verified":true,
        "restore_destination_verified":false,"whole_trash_inventory_verified":false,
        "journal_receipt_verified_by_observer":false,"gui_fidelity_verified":false,"cloud_mutated":false,"automatic_retry":false});
    immutable_record(
        &r.session_directory.join("trash-original-verified.json"),
        &result,
    )?;
    root.sync_all()?;
    active_remaining_for(&r, now()?, arm)?;
    ensure!(
        source_stamp(&held.metadata()?) == initial_source_stamp,
        "Calc Trash source changed during result publication"
    );
    Ok(result)
}
/// The Root controller proves stopped journal provenance separately. This command
/// verifies copied typed receipt bindings and exact provider Trash presence/raw A only.
pub async fn icloud_owned_calc_trash_original(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    observe(path, digest, EditorArm::Calc).await.map_err(|_| {
        anyhow::anyhow!("owned Calc Trash original refused; retained evidence; no retry")
    })
}
/// Explicit ordinary DOCX original-A Trash observer; no replay or mutation.
pub async fn icloud_owned_writer_trash_original(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    observe(path, digest, EditorArm::Writer).await.map_err(|_| {
        anyhow::anyhow!("owned Writer Trash original refused; retained evidence; no retry")
    })
}
#[cfg(test)]
mod tests;
