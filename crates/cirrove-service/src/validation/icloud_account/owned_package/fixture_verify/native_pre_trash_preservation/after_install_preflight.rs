//! Explicit after-final-install-preflight observer. No Manager, WriteContext,
//! worker, vault save, mutation method, or journal normalization is used here.
use super::*;
use cirrove_auth::CredentialVault;
use cirrove_core::upload::{Reconciliation, UploadError, UploadProvider, UploadRequest};
use cirrove_icloud::{ICloudFileReplace, ICloudSealedSignIn, SealedUploadCheckpointVault};
use secrecy::ExposeSecret;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallMarker {
    version: u8,
    run: Uuid,
    operation: Uuid,
    attempt: Uuid,
    checkpoint_sha256: String,
    phase: String,
    rename_called: bool,
    original_id: String,
    staged_id: String,
    parent_id: String,
    scope: Scope,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallIdentities {
    parent: DriveEntry,
    original_id: String,
    trash_etag: String,
    stage: DriveEntry,
    occupant: DriveEntry,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRegistration {
    authority: Registration,
    checkpoint_cipher_sha256: String,
    baseline: Option<InstallIdentities>,
}
#[derive(Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum InstallObservationPhase {
    #[default]
    Registration,
    Marker,
    JournalAdmission,
    SourceAndSettings,
    Checkpoint,
    AdapterBinding,
    Session,
    ParentListing,
    ChildListing,
    StageSelection,
    OccupantSelection,
    TrashRead,
    IdentityParity,
    StageTransfer,
    StageProof,
    OccupantTransfer,
    OccupantProof,
    Reconciliation,
    Postflight,
    LocalPreservation,
    Publication,
}
#[derive(serde::Serialize)]
struct InstallEntryProjection {
    selected_id_unique: bool,
    exact_name: bool,
    canonical_name: bool,
    exact_parent: bool,
    file_zone_and_document_id: bool,
    numbers_extension: bool,
    nonempty_revision: bool,
    expected_revision: bool,
}
fn entry_projection(
    entries: &[DriveEntry],
    id: &str,
    name: &str,
    canonical: &str,
    parent: &str,
    etag: Option<&str>,
) -> InstallEntryProjection {
    let selected: Vec<_> = entries.iter().filter(|e| e.drivewsid == id).collect();
    let e = selected.first().copied();
    InstallEntryProjection {
        selected_id_unique: selected.len() == 1,
        exact_name: e.is_some_and(|e| e.display_name() == name),
        canonical_name: e.is_some_and(|e| e.display_name() == canonical),
        exact_parent: e.is_some_and(|e| e.parent_id == parent),
        file_zone_and_document_id: e.is_some_and(|e| {
            e.kind == "FILE"
                && e.zone == "com.apple.CloudDocs"
                && !e.docwsid.is_empty()
                && Some(e.docwsid.as_str()) == id.strip_prefix("FILE::com.apple.CloudDocs::")
        }),
        numbers_extension: e.is_some_and(|e| e.extension == "numbers"),
        nonempty_revision: e.is_some_and(|e| !e.etag.is_empty() && !e.etag.contains('*')),
        expected_revision: e.is_some_and(|e| etag.is_none_or(|v| e.etag == v)),
    }
}
#[derive(Default, serde::Serialize)]
struct InstallObservationDiagnostic {
    phase: InstallObservationPhase,
    parent_id_unique: Option<bool>,
    parent_name_unique: Option<bool>,
    parent_shape_and_name: Option<bool>,
    parent_has_revision: Option<bool>,
    baseline_parent_revision_equal: Option<bool>,
    baseline_parent_equal_except_revision: Option<bool>,
    baseline_stage_equal_except_name_revision: Option<bool>,
    original_absent_from_active: Option<bool>,
    stage: Option<InstallEntryProjection>,
    occupant: Option<InstallEntryProjection>,
    baseline_parent_equal: Option<bool>,
    baseline_original_equal: Option<bool>,
    baseline_trash_revision_equal: Option<bool>,
    baseline_occupant_equal: Option<bool>,
    baseline_stage_equal: Option<bool>,
    stage_name_unchanged: Option<bool>,
    stage_name_is_canonical: Option<bool>,
    stage_display_is_canonical_plus_extension: Option<bool>,
    stage_basename_is_canonical: Option<bool>,
}
fn install_result_name(r: &Registration, path: &Path) -> Result<String> {
    let (registration, result) = match r.phase {
        Phase::Capture => (
            "install-preflight-capture-registration.json",
            "install-preflight-identities-captured.json",
        ),
        Phase::Preservation => (
            "install-preflight-preservation-registration.json",
            "install-preflight-identities-preserved.json",
        ),
    };
    ensure!(
        path.parent() == Some(r.session_directory.as_path()),
        "install registration parent refused"
    );
    if path == r.session_directory.join(registration) {
        return Ok(result.into());
    }
    ensure!(
        matches!(r.phase, Phase::Preservation),
        "capture suffix refused"
    );
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .context("install registration filename refused")?;
    let suffix = name
        .strip_prefix("install-preflight-preservation-registration-")
        .and_then(|v| v.strip_suffix(".json"))
        .context("install registration suffix refused")?;
    let id = Uuid::parse_str(suffix).context("install registration UUID refused")?;
    ensure!(
        !id.is_nil() && id.to_string() == suffix,
        "install registration UUID refused"
    );
    Ok(format!("install-preflight-identities-preserved-{id}.json"))
}

fn admission(
    x: &InstallRegistration,
    m: &InstallMarker,
    row: &UploadRecord,
    other: &UploadRecord,
) -> Result<()> {
    let r = &x.authority;
    let old = original(row)?;
    ensure!(
        r.version == 1
            && !r.run.is_nil()
            && !r.account.is_nil()
            && r.collection == "drive"
            && r.run != r.competitor_run
            && !r.competitor_run.is_nil()
            && r.operation != r.competitor_import
            && !r.operation.is_nil()
            && !r.competitor_import.is_nil()
            && r.session_directory == format!("/var/tmp/cirrove-native-final-{}", r.run)
            && r.competitor_directory
                == format!("/var/tmp/cirrove-native-final-{}", r.competitor_run)
            && r.started_unix <= now()?
            && now()? < r.deadline_unix.saturating_sub(45)
            && r.deadline_unix
                .checked_sub(r.started_unix)
                .is_some_and(|v| v > 0 && v <= 600)
            && r.baseline.is_none()
            && hex_digest(&x.checkpoint_cipher_sha256),
        "install observation scope refused"
    );
    ensure!(
        m.version == 1
            && m.run == r.run
            && m.operation == r.operation
            && !m.attempt.is_nil()
            && m.phase == "after-final-install-preflight"
            && !m.rename_called
            && m.scope == scope(r)
            && hex_digest(&m.checkpoint_sha256)
            && m.original_id == old.id
            && m.parent_id == r.parent.id
            && m.staged_id != old.id
            && m.staged_id.starts_with("FILE::com.apple.CloudDocs::"),
        "install marker refused"
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
                .is_some_and(|v| !v.is_empty() && !v.contains('*')),
        "install original authority refused"
    );
    ensure!(
        row.id == r.operation
            && row.scope == scope(r)
            && row.intent
                == UploadIntent::Replace {
                    item: old.id.clone(),
                    expected_etag: old.etag.clone().context("original revision absent")?
                }
            && row.size == r.source_b.size
            && row.sha256 == r.source_b.sha256
            && row.remote.is_none()
            && row.session_key == Some(r.operation)
            && row.package_completion.is_none()
            && row.working_file.is_none()
            && row.base.is_none()
            && row
                .identity_handoff
                .as_ref()
                .is_none_or(|v| serde_json::to_value(v)
                    .ok()
                    .is_some_and(|v| v["backup"].is_null())),
        "install replacement frontier refused"
    );
    request(row).validate()?;
    match &row.representation {
        UploadRepresentation::FlatNumbersReplacementArchive {
            semantic,
            original_semantic,
            ..
        } => ensure!(
            semantic == &r.source_b.semantic && original_semantic == &r.source_a.semantic,
            "install source semantics refused"
        ),
        _ => bail!("install source layout refused"),
    }
    match r.phase {
        Phase::Capture => ensure!(
            x.baseline.is_none()
                && row.state == UploadState::Uploading
                && row.attempt == Some(m.attempt)
                && row.session_key == Some(r.operation),
            "install paused frontier refused"
        ),
        Phase::Preservation => ensure!(
            x.baseline.is_some()
                && matches!(
                    row.state,
                    UploadState::Conflict | UploadState::VerifyRequired
                )
                && row.attempt.is_none()
                && row.failed_attempts > 0
                && !r.session_directory.join("control.sock").exists()
                && !r.competitor_directory.join("control.sock").exists(),
            "install retained uncertain frontier refused"
        ),
    }
    let c = other.remote.as_ref().context("competitor receipt absent")?;
    ensure!(
        other.id == r.competitor_import
            && other.scope == scope(r)
            && other.state == UploadState::Uploaded
            && other.attempt.is_none()
            && other.failed_attempts == 0
            && other.intent
                == UploadIntent::Create {
                    parent: r.parent.id.clone(),
                    name: old.name.clone()
                }
            && other.representation
                == UploadRepresentation::FlatNumbersArchive {
                    semantic: r.source_b.semantic.clone()
                }
            && other.size == r.source_b.size
            && other.sha256 == r.source_b.sha256
            && other.package_completion.as_ref() == Some(&r.source_b.semantic)
            && other.base.is_none()
            && other.identity_handoff.is_none()
            && other.working_file.is_none()
            && c.id != old.id
            && c.id != m.staged_id
            && c.kind == NodeKind::Folder
            && c.package
            && c.target.is_none()
            && c.parent_id.as_ref() == Some(&r.parent.id)
            && c.name == old.name
            && c.etag.as_ref().is_some_and(|v| !v.is_empty()
                && v.len() <= 4096
                && !v.contains('*')
                && !v.chars().any(char::is_control)),
        "install competitor authority refused"
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
            "install source scope refused"
        );
        s.semantic.validate()?;
    }
    Ok(())
}
fn request(row: &UploadRecord) -> UploadRequest {
    UploadRequest {
        scope: row.scope.clone(),
        intent: row.intent.clone(),
        representation: row.representation.clone(),
        size: row.size,
        sha256: row.sha256.clone(),
    }
}
// Duplicate canonical names are precisely the tested uncertainty. Select by ID,
// then verify exact name/parent/revision; never give a name ownership authority.
fn by_id(
    entries: &[DriveEntry],
    id: &str,
    name: &str,
    parent: &str,
    etag: Option<&str>,
) -> Result<DriveEntry> {
    let one: Vec<_> = entries
        .iter()
        .filter(|e| e.drivewsid == id)
        .cloned()
        .collect();
    exact(&one, id, name, parent, etag)
}
// Preservation follows the already-owned B identity, including a provider-assigned
// collision name. Capture still binds the original staging name and revision.
// Name/revision changes are accepted only by identity_parity plus full B content
// proof and an exact same-name/revision postflight; this never declares B installed.
fn stage_by_id(
    entries: &[DriveEntry],
    id: &str,
    staging_name: &str,
    parent: &str,
    etag: Option<&str>,
    phase: Phase,
) -> Result<DriveEntry> {
    let observed = entries.iter().find(|e| e.drivewsid == id);
    let name = if matches!(phase, Phase::Preservation) {
        observed.context("staged B identity absent")?.display_name()
    } else {
        staging_name.to_string()
    };
    let revision = if name == staging_name { etag } else { None };
    by_id(entries, id, &name, parent, revision)
}
fn equal_except_name_revision(before: &DriveEntry, after: &DriveEntry) -> Result<bool> {
    let mut normalized = after.clone();
    normalized.name = before.name.clone();
    normalized.etag = before.etag.clone();
    Ok(serde_json::to_value(normalized)? == serde_json::to_value(before)?)
}
fn identity_parity(
    before: &InstallIdentities,
    after: &InstallIdentities,
    _target: &str,
) -> Result<()> {
    ensure!(
        serde_json::to_value(&before.parent)? == serde_json::to_value(&after.parent)?
            && before.original_id == after.original_id
            && before.trash_etag == after.trash_etag
            && serde_json::to_value(&before.occupant)? == serde_json::to_value(&after.occupant)?,
        "install A/C binding changed"
    );
    if before.stage.display_name() == after.stage.display_name() {
        ensure!(
            serde_json::to_value(&before.stage)? == serde_json::to_value(&after.stage)?,
            "staged B revision changed"
        );
    } else {
        ensure!(
            !after.stage.name.is_empty()
                && after.stage.name.len() <= 255
                && !matches!(after.stage.name.as_str(), "." | "..")
                && !after.stage.name.contains(['/', '\\'])
                && !after.stage.name.chars().any(char::is_control)
                && !after.stage.etag.is_empty()
                && !after.stage.etag.contains('*'),
            "B install location refused"
        );
        ensure!(
            equal_except_name_revision(&before.stage, &after.stage)?,
            "B changed outside observed name/revision"
        );
    }
    Ok(())
}
// Apple can retain the declared install name inside the downloaded archive
// while its exact Drive identity has a provider-assigned collision name.
// Both candidates remain explicit owned names; never infer authority from a ZIP.
fn proof_stage_archive(
    f: &Fixture,
    file: &File,
    receipt: &PackageDownload,
    canonical: &str,
    phase: Phase,
) -> Result<()> {
    match proof(f, file, receipt, false) {
        Ok(()) => Ok(()),
        Err(error) if matches!(phase, Phase::Capture) || f.expected_root == canonical => Err(error),
        Err(_) => {
            let semantic = f
                .semantic
                .as_ref()
                .context("stage semantic authority absent")?;
            let actual = cirrove_icloud::package_archive_semantic_identity_versioned(
                file,
                receipt,
                canonical,
                semantic.version,
                &CancellationToken::new(),
            )?;
            ensure!(&actual == semantic, "stage semantic content differs");
            Ok(())
        }
    }
}
fn category(result: cirrove_core::upload::Result<Reconciliation>) -> Result<&'static str> {
    match result {
        Ok(Reconciliation::Conflict) => Ok("conflict"),
        Err(UploadError::Uncertain) => Ok("uncertain"),
        // With preserved canonical C, an acknowledged current B is not valid.
        // All other errors remain observer failures, never inferred conflict.
        _ => bail!("install same-operation inspection did not retain uncertainty"),
    }
}
async fn observe_install(
    path: &Path,
    digest: &str,
    diagnostic: &mut InstallObservationDiagnostic,
) -> Result<serde_json::Value> {
    ensure!(hex_digest(digest), "install registration digest refused");
    let raw = bytes(path, 64 * 1024)?;
    ensure!(
        hex::encode(Sha256::digest(&raw)) == digest,
        "install registration changed"
    );
    let x: InstallRegistration = serde_json::from_slice(&raw)?;
    let r = &x.authority;
    let result_name = install_result_name(r, path)?;
    diagnostic.phase = InstallObservationPhase::Marker;
    let held = open_private(&r.session_directory, true, 0)?;
    let other_held = open_private(&r.competitor_directory, true, 0)?;
    let marker_path = r.session_directory.join("install-preflight-paused.json");
    let marker_raw = bytes(&marker_path, 4096)?;
    ensure!(
        hex::encode(Sha256::digest(&marker_raw)) == r.marker_sha256,
        "install marker changed"
    );
    let m: InstallMarker = serde_json::from_slice(&marker_raw)?;
    diagnostic.phase = InstallObservationPhase::JournalAdmission;
    let row =
        read_upload_with_retained_uncertainty(&r.session_directory, r.account, r.operation, true)?;
    let other = read_upload(&r.competitor_directory, r.account, r.competitor_import)?;
    admission(&x, &m, &row, &other)?;
    diagnostic.phase = InstallObservationPhase::SourceAndSettings;
    source_check(r, &r.source_a)?;
    source_check(r, &r.source_b)?;
    let (account, settings, other_settings) = account(r)?;
    let state = r.session_directory.join("state");
    let cipher = state
        .join("accounts")
        .join(account.id.as_str())
        .join("upload-checkpoints")
        .join(r.operation.to_string())
        .join("checkpoint.sealed");
    diagnostic.phase = InstallObservationPhase::Checkpoint;
    let cipher_raw = bytes(&cipher, 4 * 1024 * 1024)?;
    ensure!(
        hex::encode(Sha256::digest(&cipher_raw)) == x.checkpoint_cipher_sha256,
        "install checkpoint cipher changed"
    );
    // load only: neither a key nor checkpoint is saved/removed. No journal owner
    // or write context is acquired by this read-only observer.
    let mono = tokio::time::Instant::now();
    let wall = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let remaining = Duration::from_secs(r.deadline_unix.saturating_sub(45))
        .checked_sub(wall)
        .filter(|v| !v.is_zero())
        .context("install observer deadline expired")?;
    let end = mono + remaining;
    let checkpoints = SealedUploadCheckpointVault::new(&state, &account.id)?;
    let saved = tokio::time::timeout_at(end, checkpoints.load(&format!("upload/{}", r.operation)))
        .await
        .map_err(|_| anyhow::anyhow!("install checkpoint load deadline"))??
        .context("install saved checkpoint absent")?;
    ensure!(
        hex::encode(Sha256::digest(saved.expose_secret().as_bytes())) == m.checkpoint_sha256,
        "install saved operation differs"
    );
    let attempt = verification_directory(&r.session_directory)?;
    manifest_with_duration(
        &attempt,
        r.run,
        "after-final-install-preflight-three-identities-read-only",
        600,
    )?;
    diagnostic.phase = InstallObservationPhase::AdapterBinding;
    let adapter = ICloudFileReplace::restore_native_package_from_sealed_checkpoint(
        request(&row),
        r.operation,
        ICloudSealedSignIn {
            apple_id: account.identity.username.clone(),
            credential_id: account.credential_id.clone(),
        },
        &state,
        &attempt,
        &saved,
    )?;
    let binding = adapter.native_install_preflight_binding(
        &r.operation.to_string(),
        &request(&row),
        &saved,
    )?;
    ensure!(
        binding["phase"] == "handoff-install-armed"
            && binding["original_id"] == m.original_id
            && binding["staged_id"] == m.staged_id
            && binding["parent_id"] == m.parent_id
            && binding["target_name"] == original(&row)?.name,
        "install decoded binding differs"
    );
    tokio::time::timeout_at(end,async {
        diagnostic.phase=InstallObservationPhase::Session;let cancel=CancellationToken::new();let mut remote=session(&r.session_directory,&account).await?;
        diagnostic.phase=InstallObservationPhase::ParentListing;let parents=remote.list_folder(cirrove_icloud::ROOT_ID).await?;
        let one:Vec<_>=parents.iter().filter(|e|e.drivewsid==r.parent.id).collect();
        diagnostic.parent_id_unique=Some(one.len()==1);
        diagnostic.parent_name_unique=Some(parents.iter().filter(|e|e.display_name()==r.parent.name).count()==1);
        diagnostic.parent_shape_and_name=Some(one.first().is_some_and(|e|e.kind=="FOLDER" && e.zone=="com.apple.CloudDocs" && e.display_name()==r.parent.name));
        ensure!(one.len()==1 && one[0].kind=="FOLDER" && one[0].zone=="com.apple.CloudDocs" && one[0].display_name()==r.parent.name
            && parents.iter().filter(|e|e.display_name()==r.parent.name).count()==1,"install parent differs");
        let mut parent=one[0].clone();parent.items.clear();parent.number_of_items=None;
        diagnostic.phase=InstallObservationPhase::ChildListing;let entries=remote.list_folder(&r.parent.id).await?;let old=original(&row)?;let c=other.remote.as_ref().context("competitor absent")?;
        // Project every non-transfer metadata comparison before the first child
        // selection refusal, so one diagnostic identifies all independent fences.
        let stage_name=format!("staged-by-cirrove-{}.numbers",r.operation);
        diagnostic.original_absent_from_active=Some(!entries.iter().any(|e|e.drivewsid==old.id));
        diagnostic.parent_has_revision=Some(!parent.etag.is_empty() && !parent.etag.contains('*'));
        diagnostic.stage=Some(entry_projection(&entries,&m.staged_id,&stage_name,&old.name,&r.parent.id,binding["staged_etag"].as_str()));
        diagnostic.occupant=Some(entry_projection(&entries,&c.id,&c.name,&old.name,&r.parent.id,c.etag.as_deref()));
        if let Some(before)=&x.baseline {
            diagnostic.baseline_parent_equal=Some(serde_json::to_value(&before.parent)?==serde_json::to_value(&parent)?);
            diagnostic.baseline_parent_revision_equal=Some(before.parent.etag==parent.etag);
            let mut normalized_parent=parent.clone();normalized_parent.etag=before.parent.etag.clone();
            diagnostic.baseline_parent_equal_except_revision=Some(serde_json::to_value(&before.parent)?==serde_json::to_value(&normalized_parent)?);
            diagnostic.baseline_original_equal=Some(before.original_id==old.id);
            let occupants:Vec<_>=entries.iter().filter(|e|e.drivewsid==c.id).collect();
            diagnostic.baseline_occupant_equal=Some(occupants.len()==1 && serde_json::to_value(&before.occupant)?==serde_json::to_value(occupants[0])?);
            let stages:Vec<_>=entries.iter().filter(|e|e.drivewsid==m.staged_id).collect();
            diagnostic.baseline_stage_equal=Some(stages.len()==1 && serde_json::to_value(&before.stage)?==serde_json::to_value(stages[0])?);
            diagnostic.baseline_stage_equal_except_name_revision=Some(stages.len()==1 && equal_except_name_revision(&before.stage,stages[0])?);
            diagnostic.stage_name_unchanged=Some(stages.len()==1 && before.stage.display_name()==stages[0].display_name());
            diagnostic.stage_name_is_canonical=Some(stages.len()==1 && stages[0].display_name()==old.name);
            diagnostic.stage_display_is_canonical_plus_extension=Some(stages.len()==1 && stages[0].display_name()==format!("{}.numbers",old.name));
            diagnostic.stage_basename_is_canonical=Some(stages.len()==1 && stages[0].name==old.name);
        }
        ensure!(!entries.iter().any(|e|e.drivewsid==old.id),"original is still active");
        diagnostic.phase=InstallObservationPhase::StageSelection;
        let stage=stage_by_id(&entries,&m.staged_id,&stage_name,&r.parent.id,binding["staged_etag"].as_str(),r.phase)?;
        diagnostic.phase=InstallObservationPhase::OccupantSelection;
        let occupant=by_id(&entries,&c.id,&c.name,&r.parent.id,c.etag.as_deref())?;
        let trash_file=tempfile::tempfile_in(&attempt)?;trash_file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        diagnostic.phase=InstallObservationPhase::TrashRead;
        let trash=remote.verify_owned_package_in_trash(cirrove_icloud::OwnedPackageTrashRequest{
            apple_account:account.identity.username.clone(),drive_id:old.id.clone(),document_id:old.id.strip_prefix("FILE::com.apple.CloudDocs::").context("original ID refused")?.into(),
            expected_root:old.name.clone(),semantic:r.source_a.semantic.clone()},trash_file,&cancel).await?;
        let identities=InstallIdentities{parent:parent.clone(),original_id:old.id.clone(),trash_etag:trash.trash_etag.clone(),stage:stage.clone(),occupant:occupant.clone()};
        diagnostic.phase=InstallObservationPhase::IdentityParity;
        if let Some(before)=&x.baseline {
            diagnostic.baseline_parent_equal=Some(serde_json::to_value(&before.parent)?==serde_json::to_value(&identities.parent)?);
            diagnostic.baseline_original_equal=Some(before.original_id==identities.original_id);
            diagnostic.baseline_trash_revision_equal=Some(before.trash_etag==identities.trash_etag);
            diagnostic.baseline_occupant_equal=Some(serde_json::to_value(&before.occupant)?==serde_json::to_value(&identities.occupant)?);
            diagnostic.baseline_stage_equal=Some(serde_json::to_value(&before.stage)?==serde_json::to_value(&identities.stage)?);
            diagnostic.stage_name_unchanged=Some(before.stage.display_name()==identities.stage.display_name());
            diagnostic.stage_name_is_canonical=Some(identities.stage.display_name()==old.name);
            identity_parity(before,&identities,&old.name)?;
        }
        let mut receipts=vec![serde_json::json!({"identity":"original_trash","id":old.id,"etag":trash.trash_etag,"size":trash.archive.size,"sha256":trash.archive.sha256,"semantic":trash.semantic})];
        for (label,e) in [("stage",&stage),("occupant",&occupant)] {
            let output=OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(attempt.join(format!("{label}.zip")))?;
            let mut sink=DiskSink(tokio::fs::File::from_std(output.try_clone()?));
            diagnostic.phase=if label=="stage" {InstallObservationPhase::StageTransfer} else {InstallObservationPhase::OccupantTransfer};
            let receipt=remote.read_owned_fixture(&parent,e,FixtureRepresentation::Package,&mut sink,&cancel).await?;
            sink.0.flush().await?;sink.0.sync_all().await?;
            diagnostic.phase=if label=="stage" {InstallObservationPhase::StageProof} else {InstallObservationPhase::OccupantProof};
            let expected=fixture(r,&r.source_b,&parent,e);
            if label=="stage" {proof_stage_archive(&expected,&output,&receipt,&old.name,r.phase)?;} else {proof(&expected,&output,&receipt,false)?;}
            receipts.push(serde_json::json!({"identity":label,"id":e.drivewsid,"etag":e.etag,"size":receipt.size,"sha256":receipt.sha256,"semantic":r.source_b.semantic}));
        }
        diagnostic.phase=InstallObservationPhase::Reconciliation;
        let inspection=if matches!(r.phase,Phase::Preservation) {
            Some(category(adapter.reconcile_upload_for_operation(&r.operation.to_string(),&request(&row),Some(&saved),&cancel).await)?)
        } else {None};
        diagnostic.phase=InstallObservationPhase::Postflight;
        let final_entries=remote.list_folder(&r.parent.id).await?;
        ensure!(!final_entries.iter().any(|e|e.drivewsid==old.id),"original active postflight");
        for e in [&stage,&occupant] {ensure!(serde_json::to_value(by_id(&final_entries,&e.drivewsid,&e.display_name(),&r.parent.id,Some(&e.etag))?)?==serde_json::to_value(e)?,"install item postflight differs");}
        // Trash API fences its full transfer. Recheck it after the other reads
        // by a second full proof on the same exact ID/revision (no raw bodies).
        let final_trash_file=tempfile::tempfile_in(&attempt)?;final_trash_file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let final_trash=remote.verify_owned_package_in_trash(cirrove_icloud::OwnedPackageTrashRequest{
            apple_account:account.identity.username.clone(),drive_id:old.id.clone(),document_id:old.id.strip_prefix("FILE::com.apple.CloudDocs::").context("original ID refused")?.into(),
            expected_root:old.name.clone(),semantic:r.source_a.semantic.clone()},final_trash_file,&cancel).await?;
        ensure!(final_trash.trash_etag==identities.trash_etag,"Trash revision postflight differs");
        let final_parents=remote.list_folder(cirrove_icloud::ROOT_ID).await?;let one:Vec<_>=final_parents.iter().filter(|e|e.drivewsid==r.parent.id).collect();
        ensure!(one.len()==1&&final_parents.iter().filter(|e|e.display_name()==r.parent.name).count()==1,"install parent postflight ambiguous");
        let mut final_parent=one[0].clone();final_parent.items.clear();final_parent.number_of_items=None;
        ensure!(serde_json::to_value(final_parent)?==serde_json::to_value(&parent)?,"install parent postflight differs");
        diagnostic.phase=InstallObservationPhase::LocalPreservation;
        ensure!(bytes(path,64*1024)?==raw && bytes(&marker_path,4096)?==marker_raw && bytes(&cipher,4*1024*1024)?==cipher_raw
            && bytes(&state.join("accounts.json"),256*1024)?==settings && bytes(&r.competitor_directory.join("state/accounts.json"),256*1024)?==other_settings,
            "install local authority changed");
        ensure!(serde_json::to_value(read_upload_with_retained_uncertainty(&r.session_directory,r.account,r.operation,true)?)?==serde_json::to_value(&row)?
            && serde_json::to_value(read_upload(&r.competitor_directory,r.account,r.competitor_import)?)?==serde_json::to_value(&other)?,"install journal changed");
        source_check(r,&r.source_a)?;source_check(r,&r.source_b)?;same_directory(&r.session_directory,&held)?;same_directory(&r.competitor_directory,&other_held)?;
        let result=serde_json::json!({"version":1,"run":r.run,"operation":r.operation,"competitor_import":r.competitor_import,"registration_sha256":digest,
            "phase":r.phase,"identities":identities,"receipts":receipts,"same_operation_inspection":inspection,"checkpoint_sha256":m.checkpoint_sha256,
            "provider_mutation":false,"mutation_replayed":false,"semantic_preservation_verified":true,"release_written":false});
        diagnostic.phase=InstallObservationPhase::Publication;
        publish_result(end,||record(&r.session_directory.join(&result_name),&result))?;Ok::<_,anyhow::Error>(result)
    }).await.map_err(|_|anyhow::anyhow!("install observation deadline expired"))?
}
pub async fn icloud_owned_native_after_install_preflight_preservation(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    let mut diagnostic = InstallObservationDiagnostic::default();
    match observe_install(path, digest, &mut diagnostic).await {
        Ok(result) => Ok(result),
        Err(_) => {
            // Typed static phases and booleans only: never serialize the source error,
            // provider names/IDs/revisions, credentials, URLs or raw bodies.
            println!("{}", serde_json::json!({"observer_diagnostic":diagnostic}));
            Err(anyhow::anyhow!(
                "owned after-install-preflight observation refused"
            ))
        }
    }
}
#[cfg(test)]
mod tests;
