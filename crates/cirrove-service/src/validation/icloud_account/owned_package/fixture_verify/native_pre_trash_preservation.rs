//! Feature-only exact three-identity observation; no ownership-by-name relaxation.
use super::*;
use crate::journal::{UploadRecord, UploadState};
use cirrove_core::{
    Node, NodeKind, Scope,
    upload::{UploadIntent, UploadRepresentation},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Capture,
    Preservation,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: PathBuf,
    size: u64,
    sha256: String,
    #[serde(deserialize_with = "required_source_root")]
    root: Option<String>,
    semantic: PackageSemanticIdentity,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u8,
    run: Uuid,
    operation: Uuid,
    attempt: Uuid,
    checkpoint_sha256: String,
    phase: String,
    inner_commit_called: bool,
    original_id: String,
    staged_id: String,
    parent_id: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Identities {
    parent: DriveEntry,
    original: DriveEntry,
    stage: DriveEntry,
    occupant: DriveEntry,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u8,
    phase: Phase,
    run: Uuid,
    account: Uuid,
    collection: String,
    session_directory: PathBuf,
    competitor_run: Uuid,
    competitor_directory: PathBuf,
    settings_sha256: String,
    competitor_settings_sha256: String,
    operation: Uuid,
    competitor_import: Uuid,
    parent: Node,
    marker_sha256: String,
    source_a: Source,
    source_b: Source,
    started_unix: u64,
    deadline_unix: u64,
    // Captured typed entries are required only for post-conflict preservation.
    baseline: Option<Identities>,
}
fn bytes(path: &Path, max: u64) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    open_private(path, false, max)?
        .take(max + 1)
        .read_to_end(&mut v)?;
    ensure!(
        v.len() as u64 <= max,
        "native preservation input bound refused"
    );
    Ok(v)
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn original(r: &UploadRecord) -> Result<&Node> {
    match &r.representation {
        UploadRepresentation::FlatNumbersReplacementArchive { original, .. } => Ok(original),
        _ => bail!("native preservation replacement layout refused"),
    }
}
fn scope(r: &Registration) -> Scope {
    Scope {
        account: r.account.to_string(),
        provider: "icloud".into(),
        collection: r.collection.clone(),
    }
}
fn row_tuple(
    row: &UploadRecord,
    sql_id: &str,
    sequence: i64,
    state: &str,
    queue: &[(i64, String, i64)],
) -> Result<()> {
    let sequence = u64::try_from(sequence)?;
    let complete = match row.state {
        UploadState::Uploaded => 1,
        UploadState::Uploading | UploadState::Conflict => 0,
        _ => bail!("native preservation row state refused"),
    };
    ensure!(
        row.id.to_string() == sql_id
            && row.sequence == sequence
            && serde_json::to_value(row.state)? == state
            && queue.len() == 1
            && queue[0] == (i64::try_from(row.sequence)?, sql_id.to_owned(), complete),
        "native preservation exact row/queue tuple refused"
    );
    Ok(())
}
fn read_upload(root: &Path, account: Uuid, id: Uuid) -> Result<UploadRecord> {
    // Live paused writer may own owner.lock; this is only a query-only snapshot,
    // never an UploadJournal/RecoveryJournal open or a normalization/migration.
    let path = root
        .join("state/accounts")
        .join(account.to_string())
        .join("journal/uploads.db");
    let held = open_private(&path, false, 64 * 1024 * 1024)?;
    let a = held.metadata()?;
    let db = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.pragma_update(None, "query_only", true)?;
    db.execute_batch("BEGIN")?;
    ensure!(
        db.pragma_query_value::<u32, _>(None, "user_version", |x| x.get(0))? == 21,
        "native preservation journal schema refused"
    );
    let actual: String =
        db.query_row("SELECT account FROM identity WHERE singleton=1", [], |x| {
            x.get(0)
        })?;
    ensure!(
        actual == account.to_string(),
        "native preservation journal account refused"
    );
    let (sql_id, sequence, state, body): (String, i64, String, String) = db.query_row(
        "SELECT id,sequence,state,body FROM uploads WHERE id=?1",
        [id.to_string()],
        |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?)),
    )?;
    ensure!(
        body.len() <= 128 * 1024,
        "native preservation row bound refused"
    );
    let row: UploadRecord = serde_json::from_str(&body)?;
    ensure!(
        row.id == id && sql_id == id.to_string(),
        "native preservation row columns refused"
    );
    let queue = db
        .prepare("SELECT sequence,id,complete FROM write_queue WHERE id=?1 LIMIT 2")?
        .query_map([id.to_string()], |x| {
            Ok((
                x.get::<_, i64>(0)?,
                x.get::<_, String>(1)?,
                x.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    row_tuple(&row, &sql_id, sequence, &state, &queue)?;
    drop(db);
    let b = open_private(&path, false, 64 * 1024 * 1024)?.metadata()?;
    ensure!(
        a.dev() == b.dev() && a.ino() == b.ino(),
        "native preservation journal replaced"
    );
    Ok(row)
}
fn account(r: &Registration) -> Result<(Account, Vec<u8>, Vec<u8>)> {
    let one = bytes(&r.session_directory.join("state/accounts.json"), 256 * 1024)?;
    let two = bytes(
        &r.competitor_directory.join("state/accounts.json"),
        256 * 1024,
    )?;
    ensure!(
        hex::encode(Sha256::digest(&one)) == r.settings_sha256
            && hex::encode(Sha256::digest(&two)) == r.competitor_settings_sha256,
        "native preservation settings changed"
    );
    let mut selected: Option<Account> = None;
    for (raw, root) in [
        (&one, &r.session_directory),
        (&two, &r.competitor_directory),
    ] {
        let s: Settings = serde_json::from_slice(raw)?;
        ensure!(
            s.version == 2 && s.accounts.len() == 1,
            "native preservation account inventory refused"
        );
        let a = &s.accounts[0];
        ensure!(
            a.id == r.account.to_string()
                && matches!(a.registration, AppRegistration::ICloud)
                && a.enabled
                && a.access == cirrove_auth::AccessMode::ReadWrite
                && a.label == "iCloudNativeFinalValidation"
                && a.drive.id == "drive"
                && a.drive.drive_type == "icloud_drive"
                && a.root_id == cirrove_icloud::ROOT_ID
                && a.mount_path == root.join("mount"),
            "native preservation account scope refused"
        );
        if let Some(first) = &selected {
            let first: &Account = first;
            ensure!(
                a.credential_id == first.credential_id
                    && a.identity.username == first.identity.username,
                "native preservation session AAD differs"
            );
        } else {
            selected = Some(a.clone());
        }
    }
    Ok((
        selected.context("native preservation account absent")?,
        one,
        two,
    ))
}
fn check(r: &Registration, m: &Marker, row: &UploadRecord, other: &UploadRecord) -> Result<()> {
    ensure!(
        r.version == 1
            && !r.run.is_nil()
            && !r.account.is_nil()
            && r.collection == "drive"
            && r.run != r.competitor_run
            && !r.competitor_run.is_nil()
            && r.session_directory == format!("/var/tmp/cirrove-native-final-{}", r.run)
            && r.competitor_directory
                == format!("/var/tmp/cirrove-native-final-{}", r.competitor_run)
            && r.operation != r.competitor_import
            && r.started_unix <= now()?
            && now()? < r.deadline_unix.saturating_sub(45)
            && r.deadline_unix
                .checked_sub(r.started_unix)
                .is_some_and(|x| x > 0 && x <= 600),
        "native preservation scope/window refused"
    );
    let old = original(row)?;
    ensure!(
        m.version == 1
            && m.run == r.run
            && m.operation == r.operation
            && !m.attempt.is_nil()
            && hex_digest(&m.checkpoint_sha256)
            && m.phase == "handoff-move-old-armed"
            && !m.inner_commit_called
            && m.original_id == old.id
            && m.parent_id == r.parent.id
            && m.staged_id != old.id
            && m.staged_id.starts_with("FILE::com.apple.CloudDocs::")
            && m.staged_id.len() > "FILE::com.apple.CloudDocs::".len(),
        "native preservation pause authority refused"
    );
    ensure!(
        r.parent.kind == NodeKind::Folder
            && !r.parent.package
            && r.parent.target.is_none()
            && r.parent.parent_id.as_deref() == Some(cirrove_icloud::ROOT_ID)
            && r.parent.name == format!("Cirrove-Native-{}", r.run)
            && old.name == format!("Cirrove-Numbers-Parent-{}.numbers", r.run)
            && old.parent_id.as_ref() == Some(&r.parent.id)
            && old.kind == NodeKind::Folder
            && old.package
            && old.target.is_none()
            && old
                .etag
                .as_ref()
                .is_some_and(|x| !x.is_empty() && !x.contains('*')),
        "native preservation original authority refused"
    );
    ensure!(
        row.scope == scope(r)
            && row.id == r.operation
            && row.intent
                == (UploadIntent::Replace {
                    item: old.id.clone(),
                    expected_etag: old
                        .etag
                        .clone()
                        .context("native preservation original etag absent")?
                })
            && row.size == r.source_b.size
            && row.sha256 == r.source_b.sha256
            && row.remote.is_none()
            && row.package_completion.is_none(),
        "native preservation replacement frontier refused"
    );
    cirrove_core::upload::UploadRequest {
        scope: row.scope.clone(),
        intent: row.intent.clone(),
        representation: row.representation.clone(),
        size: row.size,
        sha256: row.sha256.clone(),
    }
    .validate()?;
    match &row.representation {
        UploadRepresentation::FlatNumbersReplacementArchive {
            semantic,
            original_semantic,
            ..
        } => ensure!(
            semantic == &r.source_b.semantic && original_semantic == &r.source_a.semantic,
            "native preservation replacement semantic refused"
        ),
        _ => bail!("native preservation layout refused"),
    }
    match r.phase {
        Phase::Capture => ensure!(
            r.baseline.is_none()
                && row.state == UploadState::Uploading
                && row.attempt == Some(m.attempt)
                && row.session_key == Some(r.operation),
            "native preservation pause row refused"
        ),
        Phase::Preservation => ensure!(
            r.baseline.is_some()
                && row.state == UploadState::Conflict
                && row.attempt.is_none()
                && row.failed_attempts > 0
                && !r.session_directory.join("control.sock").exists()
                && !r.competitor_directory.join("control.sock").exists(),
            "native preservation stopped conflict refused"
        ),
    }
    ensure!(
        other.id == r.competitor_import
            && other.scope == scope(r)
            && other.state == UploadState::Uploaded
            && other.attempt.is_none()
            && other.failed_attempts == 0
            && other.intent
                == (UploadIntent::Create {
                    parent: r.parent.id.clone(),
                    name: format!("recovery-by-cirrove-{}.numbers", r.operation)
                })
            && other.representation
                == (UploadRepresentation::FlatNumbersArchive {
                    semantic: r.source_b.semantic.clone()
                })
            && other.size == r.source_b.size
            && other.sha256 == r.source_b.sha256
            && other.package_completion.as_ref() == Some(&r.source_b.semantic)
            && other.base.is_none()
            && other.identity_handoff.is_none()
            && other.working_file.is_none(),
        "native preservation competitor receipt refused"
    );
    let remote = other
        .remote
        .as_ref()
        .context("native preservation competitor remote absent")?;
    ensure!(
        remote.id != old.id
            && remote.id != m.staged_id
            && remote.kind == NodeKind::Folder
            && remote.package
            && remote.target.is_none()
            && remote.etag.as_ref().is_some_and(|t| !t.is_empty()
                && t.len() <= 4096
                && !t.chars().any(|c| c.is_control() || c == '*'))
            && remote
                .content_version
                .as_ref()
                .is_none_or(|t| Some(t) == remote.etag.as_ref())
            && remote.parent_id.as_ref() == Some(&r.parent.id)
            && remote.name == format!("recovery-by-cirrove-{}.numbers", r.operation),
        "native preservation competitor identity refused"
    );
    for (s, name) in [
        (&r.source_a, "source-a.numbers"),
        (&r.source_b, "source-b.numbers"),
    ] {
        ensure!(
            s.path == r.session_directory.join(name)
                && s.root.is_none()
                && s.semantic.version == 2
                && hex_digest(&s.sha256)
                && s.size > 0
                && s.size <= LIMIT,
            "native preservation flat source refused"
        );
        s.semantic.validate()?;
    }
    Ok(())
}
fn exact(
    entries: &[DriveEntry],
    id: &str,
    name: &str,
    parent: &str,
    etag: Option<&str>,
) -> Result<DriveEntry> {
    let matches: Vec<_> = entries.iter().filter(|e| e.drivewsid == id).collect();
    ensure!(
        matches.len() == 1 && entries.iter().filter(|e| e.display_name() == name).count() == 1,
        "native preservation exact identity ambiguous"
    );
    let e = matches[0];
    ensure!(
        e.kind == "FILE"
            && e.zone == "com.apple.CloudDocs"
            && e.docwsid
                == id
                    .strip_prefix("FILE::com.apple.CloudDocs::")
                    .unwrap_or_default()
            && !e.docwsid.is_empty()
            && e.parent_id == parent
            && e.display_name() == name
            && e.extension == "numbers"
            && !e.etag.is_empty()
            && !e.etag.contains('*')
            && etag.is_none_or(|v| e.etag == v),
        "native preservation exact revision differs"
    );
    Ok(e.clone())
}
fn source_check(r: &Registration, s: &Source) -> Result<()> {
    let e = DriveEntry {
        drivewsid: "FILE::com.apple.CloudDocs::local-proof".into(),
        docwsid: "local-proof".into(),
        item_id: String::new(),
        zone: "com.apple.CloudDocs".into(),
        name: "local-proof".into(),
        extension: "numbers".into(),
        parent_id: r.parent.id.clone(),
        etag: "local-proof".into(),
        kind: "FILE".into(),
        size: s.size,
        items: vec![],
        number_of_items: None,
    };
    let f = fixture(r, s, &e, &e);
    let file = open_private(&s.path, false, LIMIT)?;
    let mut copy = file.try_clone()?;
    let receipt = PackageDownload {
        size: file.metadata()?.len(),
        sha256: sha256(&mut copy)?,
    };
    ensure!(
        file.metadata()?.mode() & 0o7777 == 0o400 && file.metadata()?.nlink() == 1,
        "native preservation source mode refused"
    );
    proof(&f, &file, &receipt, true)
}
fn fixture(r: &Registration, s: &Source, parent: &DriveEntry, doc: &DriveEntry) -> Fixture {
    Fixture {
        version: 1,
        run: r.run,
        account: r.account,
        session_directory: r.session_directory.clone(),
        settings_sha256: r.settings_sha256.clone(),
        parent: parent.clone(),
        document: doc.clone(),
        format: "numbers".into(),
        representation: FixtureRepresentation::Package,
        source: s.path.clone(),
        source_size: s.size,
        source_sha256: s.sha256.clone(),
        source_root: None,
        expected_root: doc.display_name(),
        semantic: Some(s.semantic.clone()),
    }
}
// The final local validation/publication tail may never yield to timeout_at.
// Check both sides of synchronous publication using the same original clock.
fn publish_result(
    active_end: tokio::time::Instant,
    publish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    ensure!(
        tokio::time::Instant::now() < active_end,
        "native preservation deadline refused"
    );
    publish()?;
    // This fence also catches time consumed by synchronous result fsync.
    ensure!(
        tokio::time::Instant::now() < active_end,
        "native preservation publication overran deadline"
    );
    Ok(())
}
async fn observe(path: &Path, digest: &str) -> Result<serde_json::Value> {
    ensure!(hex_digest(digest), "native preservation digest refused");
    let raw = bytes(path, 64 * 1024)?;
    ensure!(
        hex::encode(Sha256::digest(&raw)) == digest,
        "native preservation registration changed"
    );
    let r: Registration = serde_json::from_slice(&raw)?;
    ensure!(
        path == r.session_directory.join(match r.phase {
            Phase::Capture => "pre-trash-capture-registration.json",
            Phase::Preservation => "pre-trash-preservation-registration.json",
        }),
        "native preservation registration filename refused"
    );
    let held = open_private(&r.session_directory, true, 0)?;
    let competitor_held = open_private(&r.competitor_directory, true, 0)?;
    let marker_raw = bytes(&r.session_directory.join("pre-trash-paused.json"), 4096)?;
    ensure!(
        hex::encode(Sha256::digest(&marker_raw)) == r.marker_sha256,
        "native preservation marker changed"
    );
    let marker: Marker = serde_json::from_slice(&marker_raw)?;
    let row = read_upload(&r.session_directory, r.account, r.operation)?;
    let other = read_upload(&r.competitor_directory, r.account, r.competitor_import)?;
    check(&r, &marker, &row, &other)?;
    source_check(&r, &r.source_a)?;
    source_check(&r, &r.source_b)?;
    let (account, settings, competitor_settings) = account(&r)?;
    let mono = tokio::time::Instant::now();
    let wall = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let remaining = Duration::from_secs(r.deadline_unix.saturating_sub(45))
        .checked_sub(wall)
        .filter(|x| !x.is_zero())
        .context("native preservation deadline refused")?;
    let active_end = mono + remaining;
    tokio::time::timeout_at(active_end,async {
        let mut remote=session(&r.session_directory,&account).await?;
        let parents=remote.list_folder(cirrove_icloud::ROOT_ID).await?;
        let candidates:Vec<_>=parents.iter().filter(|e|e.drivewsid==r.parent.id).collect();
        ensure!(candidates.len()==1&&candidates[0].kind=="FOLDER"&&candidates[0].zone=="com.apple.CloudDocs"&&candidates[0].display_name()==r.parent.name&&parents.iter().filter(|e|e.display_name()==r.parent.name).count()==1,"native preservation parent changed");let mut parent=candidates[0].clone();parent.items.clear();parent.number_of_items=None;
        let entries=remote.list_folder(&r.parent.id).await?;let old=original(&row)?;let occupant=other.remote.as_ref().context("native preservation competitor remote absent")?;
        let identities=Identities{parent:parent.clone(),original:exact(&entries,&old.id,&old.name,&r.parent.id,old.etag.as_deref())?,stage:exact(&entries,&marker.staged_id,&format!("staged-by-cirrove-{}.numbers",r.operation),&r.parent.id,None)?,occupant:exact(&entries,&occupant.id,&occupant.name,&r.parent.id,occupant.etag.as_deref())?};
        if let Some(b)=&r.baseline {ensure!(serde_json::to_value(&identities)?==serde_json::to_value(b)?,"native preservation captured revision differs");}
        let mut receipts=Vec::new();
        if matches!(r.phase,Phase::Preservation) {
            let attempt=verification_directory(&r.session_directory)?;manifest_with_duration(&attempt,r.run,"native-pre-Trash-three-identity-read-only",600)?;
            for (label,e,s) in [("original",&identities.original,&r.source_a),("stage",&identities.stage,&r.source_b),("occupant",&identities.occupant,&r.source_b)] {
                let output=OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(attempt.join(format!("{label}.zip")))?;
                let mut sink=DiskSink(tokio::fs::File::from_std(output.try_clone()?));
                let receipt=remote.read_owned_fixture(&parent,e,FixtureRepresentation::Package,&mut sink,&CancellationToken::new()).await?;sink.0.flush().await?;sink.0.sync_all().await?;
                proof(&fixture(&r,s,&parent,e),&output,&receipt,false)?;
                receipts.push(serde_json::json!({"identity":label,"id":e.drivewsid,"etag":e.etag,"size":receipt.size,"sha256":receipt.sha256,"semantic":s.semantic}));
            }
        }
        let end=remote.list_folder(&r.parent.id).await?;
        for e in [&identities.original,&identities.stage,&identities.occupant] {ensure!(serde_json::to_value(exact(&end,&e.drivewsid,&e.display_name(),&r.parent.id,Some(&e.etag))?)?==serde_json::to_value(e)?,"native preservation final document differs");}
        let end_parents=remote.list_folder(cirrove_icloud::ROOT_ID).await?;let end_parent:Vec<_>=end_parents.iter().filter(|e|e.drivewsid==r.parent.id).collect();ensure!(end_parent.len()==1&&end_parents.iter().filter(|e|e.display_name()==r.parent.name).count()==1,"native preservation final parent ambiguous");let mut end_parent=end_parent[0].clone();end_parent.items.clear();end_parent.number_of_items=None;ensure!(serde_json::to_value(&end_parent)?==serde_json::to_value(&parent)?,"native preservation final parent differs");
        ensure!(bytes(path,64*1024)?==raw&&bytes(&r.session_directory.join("pre-trash-paused.json"),4096)?==marker_raw&&bytes(&r.session_directory.join("state/accounts.json"),256*1024)?==settings&&bytes(&r.competitor_directory.join("state/accounts.json"),256*1024)?==competitor_settings,"native preservation inputs changed");
        ensure!(serde_json::to_value(read_upload(&r.session_directory,r.account,r.operation)?)?==serde_json::to_value(&row)?&&serde_json::to_value(read_upload(&r.competitor_directory,r.account,r.competitor_import)?)?==serde_json::to_value(&other)?,"native preservation journal changed");source_check(&r,&r.source_a)?;source_check(&r,&r.source_b)?;same_directory(&r.session_directory,&held)?;same_directory(&r.competitor_directory,&competitor_held)?;
        let result=serde_json::json!({"version":1,"run":r.run,"account":r.account,"collection":r.collection,"operation":r.operation,"competitor_import":r.competitor_import,"registration_sha256":digest,"phase":r.phase,"identities":identities,"receipts":receipts,"provider_mutation":false,"semantic_preservation_verified":matches!(r.phase,Phase::Preservation),"http_request_count":"uninstrumented","release_written":false});
        publish_result(active_end, || record(&r.session_directory.join(match r.phase{Phase::Capture=>"pre-trash-identities-captured.json",Phase::Preservation=>"pre-trash-identities-preserved.json"}),&result))?;Ok::<_,anyhow::Error>(result)
    }).await.map_err(|_|anyhow::anyhow!("native preservation deadline refused"))?
}
pub async fn icloud_owned_native_pre_trash_preservation(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    observe(path, digest)
        .await
        .map_err(|_| anyhow::anyhow!("owned native pre-Trash preservation refused"))
}

#[cfg(test)]
#[path = "native_pre_trash_preservation/tests.rs"]
mod tests;
