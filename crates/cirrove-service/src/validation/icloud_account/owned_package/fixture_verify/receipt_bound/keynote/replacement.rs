//! Explicit wrapped Keynote and wrapped/flat Pages A/B receipt observer; no enqueue, reconcile or mutation.
use super::*;
use cirrove_core::upload::PackageSourceLayout;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// The public entry point selects one of these two implemented contracts. The
// registration wire has no inferred format or caller-selected format field.
#[derive(Clone, Copy)]
enum Format {
    Keynote,
    Pages,
}
impl Format {
    fn application(self) -> &'static str {
        match self {
            Self::Keynote => "Keynote",
            Self::Pages => "Pages",
        }
    }
    fn fixture_format(self) -> &'static str {
        match self {
            Self::Keynote => "keynote",
            Self::Pages => "pages",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Keynote => "key",
            Self::Pages => "pages",
        }
    }
    fn source_root(self) -> &'static str {
        match self {
            Self::Keynote => "Source.key",
            Self::Pages => "Source.pages",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Keynote => "iCloudKeynoteReplacementValidation",
            Self::Pages => "iCloudPagesReplacementValidation",
        }
    }
    fn failure(self) -> &'static str {
        match self {
            Self::Keynote => "owned Keynote replacement observation refused; no mutation submitted",
            Self::Pages => "owned Pages replacement observation refused; no mutation submitted",
        }
    }
}

// This source wire belongs only to replacement observers. Historical import and
// Numbers registrations keep their existing required string root unchanged.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: PathBuf,
    size: u64,
    sha256: String,
    #[serde(deserialize_with = "required_source_root")]
    root: Option<String>,
    semantic: PackageSemanticIdentity,
}
fn wrapped_layout(layout: &PackageSourceLayout) -> bool {
    *layout == PackageSourceLayout::Wrapped
}
#[cfg(test)]
fn source_verified(source: &Source) -> Result<()> {
    source_verified_for(source, PackageSourceLayout::Wrapped)
}
fn source_verified_for(source: &Source, layout: PackageSourceLayout) -> Result<()> {
    let mut file = open_private(&source.path, false, LIMIT)?;
    if layout == PackageSourceLayout::FlatPages {
        ensure!(
            file.metadata()?.permissions().mode() & 0o777 == 0o400,
            "owned flat Pages source must remain read-only"
        );
    }
    let receipt = PackageDownload {
        size: file.metadata()?.len(),
        sha256: sha256(&mut file)?,
    };
    ensure!(
        receipt.size == source.size && receipt.sha256 == source.sha256,
        "owned source digest changed"
    );
    let proof = match (layout, source.root.as_deref()) {
        (PackageSourceLayout::Wrapped, Some(root)) => {
            cirrove_icloud::package_archive_semantic_identity_versioned(
                &file,
                &receipt,
                root,
                2,
                &CancellationToken::new(),
            )?
        }
        (PackageSourceLayout::FlatPages, None) => {
            cirrove_icloud::package_flat_archive_semantic_identity_v2(
                &file,
                &receipt,
                &CancellationToken::new(),
            )?
        }
        _ => bail!("owned replacement source layout refused"),
    };
    ensure!(
        proof == source.semantic,
        "owned source semantic content changed"
    );
    Ok(())
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Preflight,
    Postflight,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    version: u8,
    phase: Phase,
    run: Uuid,
    account: Uuid,
    label: String,
    session_directory: PathBuf,
    settings_sha256: String,
    parent_creation: Uuid,
    import: Uuid,
    replacement: Option<Uuid>,
    original: Node,
    #[serde(default, skip_serializing_if = "wrapped_layout")]
    source_layout: PackageSourceLayout,
    source_a: Source,
    source_b: Source,
    started_unix: u64,
    deadline_unix: u64,
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn revision(n: &Node) -> bool {
    n.id.starts_with("FILE::com.apple.CloudDocs::")
        && !n.id.ends_with("::")
        && n.kind == NodeKind::Folder
        && n.package
        && n.target.is_none()
        && n.content_version.is_none()
        && n.size > 0
        && n.etag.as_ref().is_some_and(|e| {
            !e.is_empty() && e.len() <= 1024 && !e.contains('*') && !e.chars().any(char::is_control)
        })
}
impl Plan {
    fn name(&self, format: Format) -> String {
        format!(
            "Cirrove-{}-Replacement-{}.{}",
            format.application(),
            self.run,
            format.extension()
        )
    }
    fn scope(&self) -> Scope {
        Scope {
            account: self.account.to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        }
    }
    fn parent_name(&self) -> String {
        format!("Cirrove-Native-{}", self.run)
    }
    fn settings(&self) -> Result<Account> {
        settings_for(
            self.account,
            &self.label,
            &self.session_directory,
            &self.settings_sha256,
        )
    }
    fn representation(&self, source: &Source, replacing: bool) -> Result<UploadRepresentation> {
        match (self.source_layout, source.root.as_deref(), replacing) {
            (PackageSourceLayout::Wrapped, Some(root), false) => {
                Ok(UploadRepresentation::PackageArchive {
                    expected_root: root.into(),
                    semantic: source.semantic.clone(),
                })
            }
            (PackageSourceLayout::Wrapped, Some(root), true) => {
                Ok(UploadRepresentation::PackageReplacementArchive {
                    expected_root: root.into(),
                    semantic: source.semantic.clone(),
                    original: Box::new(self.original.clone()),
                    original_semantic: self.source_a.semantic.clone(),
                })
            }
            (PackageSourceLayout::FlatPages, None, false) => {
                Ok(UploadRepresentation::FlatPagesArchive {
                    semantic: source.semantic.clone(),
                })
            }
            (PackageSourceLayout::FlatPages, None, true) => {
                Ok(UploadRepresentation::FlatPagesReplacementArchive {
                    semantic: source.semantic.clone(),
                    original: Box::new(self.original.clone()),
                    original_semantic: self.source_a.semantic.clone(),
                })
            }
            _ => bail!("owned replacement source layout refused"),
        }
    }
    fn validate_at(&self, time: u64, format: Format) -> Result<()> {
        ensure!(
            self.version == 1
                && !self.run.is_nil()
                && !self.account.is_nil()
                && self.label == format.label()
                && self.session_directory
                    == format!(
                        "/var/tmp/cirrove-{}-replacement-{}",
                        format.fixture_format(),
                        self.run
                    )
                && hex_digest(&self.settings_sha256)
                && !self.parent_creation.is_nil()
                && !self.import.is_nil()
                && self.parent_creation != self.import
                && self.deadline_unix > self.started_unix
                && self.deadline_unix - self.started_unix <= 600
                && self.started_unix <= time
                && time < self.deadline_unix
                && match (self.phase, self.replacement) {
                    (Phase::Preflight, None) => true,
                    (Phase::Postflight, Some(id)) =>
                        !id.is_nil() && id != self.import && id != self.parent_creation,
                    _ => false,
                },
            "owned Keynote replacement registration refused"
        );
        for (source, filename) in [
            (&self.source_a, format!("source-a.{}", format.extension())),
            (&self.source_b, format!("source-b.{}", format.extension())),
        ] {
            source.semantic.validate()?;
            ensure!(
                source.path == self.session_directory.join(filename)
                    && match (format, self.source_layout, source.root.as_deref()) {
                        (_, PackageSourceLayout::Wrapped, Some(root)) =>
                            root == format.source_root(),
                        (Format::Pages, PackageSourceLayout::FlatPages, None) => true,
                        _ => false,
                    }
                    && source.size > 0
                    && source.size <= LIMIT
                    && hex_digest(&source.sha256)
                    && source.semantic.version == 2,
                "owned Keynote replacement source refused"
            );
        }
        ensure!(
            self.source_a.sha256 != self.source_b.sha256
                && self.source_a.semantic != self.source_b.semantic
                && revision(&self.original)
                && self.original.name == self.name(format)
                && self.original.size == self.source_a.semantic.expanded_bytes,
            "owned Keynote original/source pair refused"
        );
        Ok(())
    }
    fn stem(&self) -> &'static str {
        if self.phase == Phase::Preflight {
            "replacement-preflight"
        } else {
            "replacement-postflight"
        }
    }
}
#[derive(serde::Serialize)]
struct Snapshot {
    uploads: Vec<UploadRecord>,
    mutations: Vec<MutationRecord>,
    objects: Vec<NamespaceObject>,
    queue: Vec<(i64, String, i64)>,
    associations: Vec<(String, String)>,
    publications: Vec<(String, i64, Option<String>)>,
}
fn tuple(
    id: Uuid,
    sequence: u64,
    state: serde_json::Value,
    sql: &(String, i64, String),
    queue: &[(i64, String, i64)],
) -> Result<()> {
    ensure!(
        id.to_string() == sql.0
            && i64::try_from(sequence)? == sql.1
            && state == sql.2
            && queue.iter().filter(|q| q.1 == sql.0).count() == 1
            && queue.iter().any(|q| q == &(sql.1, sql.0.clone(), 1)),
        "owned Keynote journal row/queue tuple refused"
    );
    Ok(())
}
fn snapshot(path: &Path, account: Uuid) -> Result<Snapshot> {
    let held = open_private(path, false, 64 * 1024 * 1024)?;
    let original = held.metadata()?;
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.pragma_update(None, "query_only", true)?;
    db.execute_batch("BEGIN")?;
    ensure!(
        db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))? == 21
            && db.query_row("SELECT account FROM identity WHERE singleton=1", [], |r| {
                r.get::<_, String>(0)
            })? == account.to_string(),
        "owned Keynote journal schema/account refused"
    );
    let queue = db
        .prepare("SELECT sequence,id,complete FROM write_queue ORDER BY sequence LIMIT 4")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<Vec<(i64, String, i64)>, _>>()?;
    let mut uploads = Vec::new();
    let raw = db.prepare("SELECT id,sequence,state,CASE WHEN length(CAST(body AS BLOB))<=131072 THEN body END FROM uploads ORDER BY sequence LIMIT 3")?
        .query_map([],|r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?
        .collect::<std::result::Result<Vec<_>,_>>()?;
    for (id, sequence, state, body) in raw {
        let row: UploadRecord = serde_json::from_str(&body)?;
        tuple(
            row.id,
            row.sequence,
            serde_json::to_value(row.state)?,
            &(id, sequence, state),
            &queue,
        )?;
        uploads.push(row);
    }
    let mut mutations = Vec::new();
    let raw = db.prepare("SELECT id,sequence,state,CASE WHEN length(CAST(body AS BLOB))<=131072 THEN body END FROM mutations ORDER BY sequence LIMIT 2")?
        .query_map([],|r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?
        .collect::<std::result::Result<Vec<_>,_>>()?;
    for (id, sequence, state, body) in raw {
        let row: MutationRecord = serde_json::from_str(&body)?;
        tuple(
            row.id,
            row.sequence,
            serde_json::to_value(row.state)?,
            &(id, sequence, state),
            &queue,
        )?;
        mutations.push(row);
    }
    let mut objects = Vec::new();
    let raw = db.prepare("SELECT id,CASE WHEN length(CAST(body AS BLOB))<=131072 THEN body END FROM namespace_objects ORDER BY id LIMIT 4")?
        .query_map([],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    for (id, body) in raw {
        let row: NamespaceObject = serde_json::from_str(&body)?;
        ensure!(
            row.id.to_string() == id,
            "owned Keynote namespace ID refused"
        );
        objects.push(row);
    }
    let associations = db
        .prepare("SELECT operation,object FROM namespace_operations ORDER BY operation LIMIT 3")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<Vec<(String, String)>, _>>()?;
    let publications = db.prepare("SELECT operation,done,observed FROM package_metadata_publication ORDER BY operation LIMIT 3")?
        .query_map([],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<std::result::Result<Vec<(String,i64,Option<String>)>,_>>()?;
    let empty: i64 = db.query_row("SELECT (SELECT count(*) FROM working_files)+(SELECT count(*) FROM native_working_operations)",[],|r| r.get(0))?;
    ensure!(empty == 0, "owned Keynote working frontier refused");
    drop(db);
    let end = open_private(path, false, 64 * 1024 * 1024)?.metadata()?;
    ensure!(
        original.dev() == end.dev() && original.ino() == end.ino(),
        "owned Keynote journal replaced"
    );
    Ok(Snapshot {
        uploads,
        mutations,
        objects,
        queue,
        associations,
        publications,
    })
}
fn admitted(plan: &Plan, s: &Snapshot, format: Format) -> Result<(Node, Node, Option<Node>)> {
    let post = plan.phase == Phase::Postflight;
    let count = if post { 2 } else { 1 };
    ensure!(
        s.uploads.len() == count
            && s.queue.len() == count + 1
            && s.publications.len() == count
            && s.objects.len() == if post { 3 } else { 1 }
            && s.associations.len() == if post { 2 } else { 1 },
        "owned Keynote exact frontier refused"
    );
    let parent_object = s
        .objects
        .iter()
        .find(|o| {
            o.remote
                .as_ref()
                .is_some_and(|n| n.kind == NodeKind::Folder && !n.package)
        })
        .context("owned Keynote parent namespace absent")?;
    let parent = parent_binding_for(
        &plan.scope(),
        plan.parent_creation,
        &plan.parent_name(),
        &s.mutations,
        parent_object,
    )?;
    ensure!(
        s.associations.contains(&(
            plan.parent_creation.to_string(),
            parent_object.id.to_string()
        )) && plan.original.parent_id.as_ref() == Some(&parent.id),
        "owned Keynote parent association refused"
    );
    let original = s
        .uploads
        .iter()
        .find(|r| r.id == plan.import)
        .context("owned Keynote import absent")?;
    for row in &s.uploads {
        ensure!(
            row.scope == plan.scope()
                && row.state == UploadState::Uploaded
                && row.attempt.is_none()
                && row.base.is_none()
                && row.working_file.is_none()
                && (row.id == plan.import || Some(row.id) == plan.replacement),
            "owned Keynote upload authority refused"
        );
    }
    ensure!(
        original.intent
            == UploadIntent::Create {
                parent: parent.id.clone(),
                name: plan.name(format)
            }
            && original.representation == plan.representation(&plan.source_a, false)?
            && original.size == plan.source_a.size
            && original.sha256 == plan.source_a.sha256
            && original.package_completion.as_ref() == Some(&plan.source_a.semantic)
            && original.identity_handoff.is_none()
            && original.remote.as_ref() == Some(&plan.original),
        "owned Keynote imported original refused"
    );
    let (current, backup) = if post {
        let (current, backup) = replacement_bound(
            plan.replacement
                .context("owned replacement identity missing")?,
            &plan.original,
            plan.representation(&plan.source_b, true)?,
            plan.source_b.size,
            &plan.source_b.sha256,
            &plan.source_b.semantic,
            &s.uploads,
        )?;
        ensure!(
            revision(current)
                && revision(backup)
                && current.size == plan.source_b.semantic.expanded_bytes,
            "owned Keynote current/Trash revision refused"
        );
        let owner = s
            .objects
            .iter()
            .find(|o| o.remote.as_ref() == Some(current))
            .context("owned Keynote current owner absent")?;
        let recovery = s
            .objects
            .iter()
            .find(|o| o.remote.as_ref() == Some(backup))
            .context("owned Keynote backup owner absent")?;
        ensure!(
            owner.id != recovery.id
                && owner.scope == plan.scope()
                && recovery.scope == owner.scope
                && owner.remote_owned
                && owner.latest.is_none_or(|id| Some(id) == plan.replacement)
                && owner.working_file.is_none()
                && owner.native_archive.is_none()
                && recovery.remote_owned
                && recovery.latest.is_none()
                && recovery.working_file.is_none()
                && recovery.native_archive.is_none()
                && s.associations.contains(&(
                    plan.replacement
                        .context("owned Keynote replacement absent")?
                        .to_string(),
                    owner.id.to_string()
                )),
            "owned Keynote handoff owners refused"
        );
        (current.clone(), Some(backup.clone()))
    } else {
        (plan.original.clone(), None)
    };
    for row in &s.uploads {
        let published = s
            .publications
            .iter()
            .find(|p| p.0 == row.id.to_string())
            .context("owned Keynote publication absent")?;
        ensure!(
            published.1 == 1
                && published
                    .2
                    .as_ref()
                    .is_some_and(
                        |p| serde_json::from_str::<Node>(p).ok().as_ref() == row.remote.as_ref()
                    ),
            "owned Keynote publication unconfirmed"
        );
    }
    Ok((parent, current, backup))
}
fn before_dispatch(active_end: tokio::time::Instant) -> Result<()> {
    ensure!(
        tokio::time::Instant::now() < active_end,
        "owned Keynote dispatch deadline expired"
    );
    Ok(())
}
fn publish_result(
    active_end: tokio::time::Instant,
    publish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    ensure!(
        tokio::time::Instant::now() < active_end,
        "owned Keynote deadline expired"
    );
    publish()?;
    ensure!(
        tokio::time::Instant::now() < active_end,
        "owned Keynote publication overran deadline"
    );
    Ok(())
}
async fn observe(
    path: &Path,
    digest: &str,
    active_end: tokio::time::Instant,
    format: Format,
) -> Result<serde_json::Value> {
    let bytes = read_private(path, 32 * 1024)?;
    let plan: Plan = decode_keynote(&bytes, digest)?;
    plan.validate_at(now()?, format)?;
    ensure!(
        path == plan
            .session_directory
            .join(format!("{}-registration.json", plan.stem())),
        "owned Keynote replacement manifest location refused"
    );
    let guard = open_private(&plan.session_directory, true, 0)?;
    let account = plan.settings()?;
    source_verified_for(&plan.source_a, plan.source_layout)?;
    source_verified_for(&plan.source_b, plan.source_layout)?;
    let journal_path = plan
        .session_directory
        .join("state/accounts")
        .join(&account.id)
        .join("journal");
    let journal_guard = open_private(&journal_path, true, 0)?;
    // Exclusive account-owner lease prevents a writer from opening this stopped
    // journal. It is not a shared content mutex or SQLite transaction; all query
    // snapshots/statements below close before session/provider awaits.
    let _lease = RecoveryJournal::open(&journal_path, &account.id)?;
    let state = snapshot(&journal_path.join("uploads.db"), plan.account)?;
    let (parent, current, backup) = admitted(&plan, &state, format)?;
    let frozen = serde_json::to_vec(&state)?;
    before_dispatch(active_end)?;
    let mut remote = session(&plan.session_directory, &account).await?;
    before_dispatch(active_end)?;
    let parents = remote.list_folder(cirrove_icloud::ROOT_ID).await?;
    let mut matching = parents.iter().filter(|e| e.drivewsid == parent.id);
    let mut parent_entry = matching
        .next()
        .context("owned Keynote parent absent")?
        .clone();
    ensure!(
        matching.next().is_none()
            && parent_entry.kind == "FOLDER"
            && parent_entry.zone == "com.apple.CloudDocs"
            && parent_entry.name == plan.parent_name(),
        "owned Keynote parent changed"
    );
    parent_entry.items.clear();
    parent_entry.number_of_items = None;
    before_dispatch(active_end)?;
    let document = exact_entry(&remote.list_folder(&parent.id).await?, &current)?;
    let source = if backup.is_some() {
        &plan.source_b
    } else {
        &plan.source_a
    };
    let fixture = serde_json::json!({"version":1,"run":plan.run,"account":plan.account,"session_directory":plan.session_directory,
        "settings_sha256":plan.settings_sha256,"parent":parent_entry,"document":document,"format":format.fixture_format(),"representation":"package",
        "source":source.path,"source_size":source.size,"source_sha256":source.sha256,"source_root":source.root,"expected_root":plan.name(format),"semantic":source.semantic});
    let fixture_path = plan
        .session_directory
        .join(format!("{}-fixture.json", plan.stem()));
    record(&fixture_path, &fixture)?;
    let fixture_digest = hex::encode(Sha256::digest(read_private(&fixture_path, 32 * 1024)?));
    before_dispatch(active_end)?;
    let current_proof = if plan.source_layout == PackageSourceLayout::FlatPages {
        icloud_owned_flat_pages_fixture_verify_before(&fixture_path, &fixture_digest, active_end)
            .await?
    } else {
        icloud_owned_fixture_verify_before(&fixture_path, &fixture_digest, active_end).await?
    };
    let mut trash_proof = None;
    if let Some(backup) = &backup {
        let attempt = verification_directory(&plan.session_directory)?;
        manifest(
            &attempt,
            plan.run,
            &format!("{}-replacement-Trash-read-only", format.fixture_format()),
        )?;
        let staging = tempfile::tempfile_in(&attempt)?;
        staging.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        before_dispatch(active_end)?;
        let verified = remote
            .verify_owned_package_in_trash(
                cirrove_icloud::OwnedPackageTrashRequest {
                    apple_account: account.identity.username.clone(),
                    drive_id: backup.id.clone(),
                    document_id: backup
                        .id
                        .strip_prefix("FILE::com.apple.CloudDocs::")
                        .context("owned Keynote Trash ID refused")?
                        .into(),
                    expected_root: plan.name(format),
                    semantic: plan.source_a.semantic.clone(),
                },
                staging,
                &CancellationToken::new(),
            )
            .await?;
        trash_binding(backup, &plan.source_a.semantic, &verified)?;
        trash_proof = Some(
            serde_json::json!({"item":backup.id,"etag":verified.trash_etag,"semantic":verified.semantic,"size":verified.archive.size,"sha256":verified.archive.sha256,"retained_archive":false}),
        );
    }
    before_dispatch(active_end)?;
    ensure!(
        exact_entry(&remote.list_folder(&parent.id).await?, &current)? == document,
        "owned Keynote current changed during proof"
    );
    before_dispatch(active_end)?;
    let end = remote.list_folder(cirrove_icloud::ROOT_ID).await?;
    let mut matching = end.iter().filter(|e| e.drivewsid == parent.id);
    let mut end_parent = matching
        .next()
        .context("owned Keynote parent disappeared")?
        .clone();
    end_parent.items.clear();
    end_parent.number_of_items = None;
    ensure!(
        matching.next().is_none() && end_parent == parent_entry,
        "owned Keynote parent changed during proof"
    );
    plan.validate_at(now()?, format)?;
    plan.settings()?;
    source_verified_for(&plan.source_a, plan.source_layout)?;
    source_verified_for(&plan.source_b, plan.source_layout)?;
    ensure!(
        read_private(path, 32 * 1024)? == bytes
            && serde_json::to_vec(&snapshot(&journal_path.join("uploads.db"), plan.account)?)?
                == frozen,
        "owned Keynote local authority changed during proof"
    );
    same_directory(&plan.session_directory, &guard)?;
    same_directory(&journal_path, &journal_guard)?;
    let mut result = serde_json::json!({"run":plan.run,"account":plan.account,"phase":plan.phase,"parent_creation":plan.parent_creation,"import":plan.import,
        "replacement":plan.replacement,"registration_sha256":digest,"fixture_manifest_sha256":fixture_digest,"original":plan.original,"current_node":current,"backup_node":backup,
        "current":current_proof,"original_trash":trash_proof,"cloud_mutated":false,"gui_fidelity_verified":false});
    if plan.source_layout == PackageSourceLayout::FlatPages {
        result["source_layout"] = serde_json::json!("flat_pages");
    }
    publish_result(active_end, || {
        record(
            &plan
                .session_directory
                .join(format!("{}-verified.json", plan.stem())),
            &result,
        )
    })?;
    Ok(result)
}
async fn verify_with_format(
    path: &Path,
    digest: &str,
    format: Format,
) -> Result<serde_json::Value> {
    let result = async {
        let plan: Plan = decode_keynote(&read_private(path, 32 * 1024)?, digest)?;
        plan.validate_at(now()?, format)?;
        let monotonic = tokio::time::Instant::now();
        let wall = SystemTime::now().duration_since(UNIX_EPOCH)?;
        let remaining = Duration::from_secs(plan.deadline_unix)
            .checked_sub(wall)
            .context("owned Keynote deadline expired")?;
        let active_end = monotonic + remaining;
        tokio::time::timeout_at(active_end, observe(path, digest, active_end, format))
            .await
            .map_err(|_| anyhow::anyhow!("owned Keynote deadline expired"))?
    }
    .await;
    result.map_err(|_| anyhow::anyhow!(format.failure()))
}
// Existing public Keynote entry point retains its exact contract and output.
pub async fn icloud_owned_keynote_replacement_receipt_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    verify_with_format(path, digest, Format::Keynote).await
}
pub(in super::super) async fn verify_pages(path: &Path, digest: &str) -> Result<serde_json::Value> {
    verify_with_format(path, digest, Format::Pages).await
}
#[cfg(test)]
mod pages_tests;
#[cfg(test)]
mod tests;
