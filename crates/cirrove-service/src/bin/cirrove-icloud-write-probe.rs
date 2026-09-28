//! Explicit, one-shot live mutation experiment against Cirrove-owned fixtures.
//! This does not expose iCloud as a writable filesystem.
#[path = "cirrove-icloud-write-probe/icloud_handoff_journal.rs"]
mod icloud_handoff_journal;

use anyhow::{Context, Result, bail};
use cirrove_auth::{AppRegistration, CredentialVault, DesktopVault};
use cirrove_core::mutation::{
    MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation, MutationRequest,
};
use cirrove_core::upload::UploadRequest;
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use cirrove_icloud::{
    EmptyFolderMoveOutcome, HandoffOutcome, ICloudOwnedFixtureFolderCreate,
    ICloudOwnedFixtureFolderMove, ICloudOwnedFixtureFolderRemove, ICloudOwnedFixtureHandoff,
    ICloudOwnedFixtureMove, ICloudOwnedFixtureRemove, ICloudOwnedFixtureUpload,
    ICloudOwnedMovePause, ICloudReadSession, MoveCollisionOutcome, MoveProbeOutcome,
    OccupiedNameOutcome, PopulatedFolderMoveOutcome, RenameProbeOutcome, SameIdUpdateOutcome,
    SealedSessionVault, TrashProbeOutcome, TrashRestoreOutcome,
};
use cirrove_service::accounts::Settings;
use cirrove_service::journal::{MutationState, UploadIntent, UploadJournal, UploadState};
use cirrove_service::mutations::MutationWorker;
use cirrove_service::transfers::TransferWorker;
use icloud_handoff_journal::{Journal, StageJournal};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct OccupiedMoveFixture {
    account_id: String,
    source_folder_id: String,
    source_folder_name: String,
    destination_folder_id: String,
    destination_folder_name: String,
    moving_file_id: String,
    moving_file_etag: String,
    moving_sha256: String,
    moving_size: u64,
    occupant_file_id: String,
    occupant_file_etag: String,
    occupant_sha256: String,
    occupant_size: u64,
}

#[derive(Serialize, Deserialize)]
struct OwnedMoveFolders {
    source_id: String,
    source_name: String,
    destination_id: String,
    destination_name: String,
}

#[derive(Serialize, Deserialize)]
struct EmptyFolderMoveFixture {
    account_id: String,
    source_id: String,
    source_name: String,
    destination_id: String,
    destination_name: String,
    nested_id: String,
    nested_name: String,
    nested_etag: String,
}

#[derive(Serialize, Deserialize)]
struct PopulatedFolderMoveFixture {
    account_id: String,
    source_id: String,
    source_name: String,
    destination_id: String,
    destination_name: String,
    nested_id: String,
    nested_name: String,
    nested_etag: String,
    file_id: String,
    file_document_id: String,
    file_etag: String,
    file_size: u64,
    file_sha256: String,
}

fn populated_folder_move_fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-populated-folder-move-fixtures")
}

fn save_populated_folder_move_fixture(fixture: &PopulatedFolderMoveFixture) -> Result<Uuid> {
    let directory = populated_folder_move_fixture_directory();
    fs::create_dir_all(&directory)?;
    let id = Uuid::new_v4();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(format!("{id}.json")))?;
    file.write_all(&serde_json::to_vec_pretty(fixture)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::File::open(directory)?.sync_all()?;
    Ok(id)
}

fn empty_folder_move_fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-empty-folder-move-fixtures")
}

fn save_empty_folder_move_fixture(fixture: &EmptyFolderMoveFixture) -> Result<Uuid> {
    let directory = empty_folder_move_fixture_directory();
    fs::create_dir_all(&directory)?;
    let id = Uuid::new_v4();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(format!("{id}.json")))?;
    file.write_all(&serde_json::to_vec_pretty(fixture)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::File::open(directory)?.sync_all()?;
    Ok(id)
}

fn occupied_move_fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local-state/icloud-occupied-move-fixtures")
}

fn save_occupied_move_fixture(fixture: &OccupiedMoveFixture) -> Result<Uuid> {
    let directory = occupied_move_fixture_directory();
    fs::create_dir_all(&directory)?;
    let id = Uuid::new_v4();
    let path = directory.join(format!("{id}.json"));
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&serde_json::to_vec_pretty(fixture)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::File::open(directory)?.sync_all()?;
    Ok(id)
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = match arguments.as_slice() {
        [] => 0,
        [flag] if flag == "--same-id" => 1,
        [flag] if flag == "--stale-etag" => 2,
        [flag] if flag == "--rename-conflict" => 3,
        [flag] if flag == "--metadata-rename" => 4,
        [flag] if flag == "--http-if-match" => 5,
        [flag] if flag == "--occupied-name" => 6,
        [flag] if flag == "--inspect-occupied" => 7,
        [flag] if flag == "--staged-handoff" => 8,
        [flag] if flag == "--durable-stop-after-recovery" => 9,
        [flag] if flag == "--durable-resume" => 10,
        [flag] if flag == "--durable-drop-old-receipt" => 11,
        [flag] if flag == "--durable-resume-lost-old" => 12,
        [flag] if flag == "--durable-drop-new-receipt" => 13,
        [flag] if flag == "--durable-resume-lost-new" => 14,
        [flag] if flag == "--durable-drop-registration-receipt" => 15,
        [flag] if flag == "--durable-resume-registration" => 16,
        [flag] if flag == "--durable-handoff-registered" => 17,
        [flag] if flag == "--stale-etag-trash" => 18,
        [flag] if flag == "--stale-then-fresh-trash" => 19,
        [flag] if flag == "--inspect-trash" => 20,
        [flag] if flag == "--trash-restore-cycle" => 21,
        [flag] if flag == "--worker-create" => 22,
        [flag] if flag == "--worker-discard-registration-receipt" => 23,
        [flag] if flag == "--worker-resume-registration" => 24,
        [flag] if flag == "--owned-file-trash-adapter" => 25,
        [flag] if flag == "--worker-owned-trash" => 26,
        [flag] if flag == "--worker-discard-trash-receipt" => 27,
        [flag] if flag == "--worker-resume-trash" => 28,
        [flag] if flag == "--worker-owned-handoff" => 29,
        [flag] if flag == "--worker-discard-old-handoff-receipt" => 30,
        [flag] if flag == "--worker-resume-handoff" => 31,
        [flag] if flag == "--worker-discard-new-handoff-receipt" => 32,
        [flag] if flag == "--worker-reconcile-new-handoff" => 33,
        [flag] if flag == "--owned-trash-download" => 34,
        [flag] if flag == "--conditional-trash-handoff" => 35,
        [flag] if flag == "--worker-conditional-trash-handoff" => 36,
        [flag] if flag == "--worker-discard-conditional-trash-receipt" => 37,
        [flag] if flag == "--worker-resume-conditional-trash" => 38,
        [flag] if flag == "--inspect-conditional-trash-journal" => 39,
        [flag] if flag == "--worker-resume-inspected-conditional-trash" => 40,
        [flag] if flag == "--inspect-conditional-trash-receipt" => 41,
        [flag] if flag == "--publish-conditional-trash-receipt" => 42,
        [flag] if flag == "--worker-discard-conditional-rename-receipt" => 43,
        [flag] if flag == "--worker-reconcile-conditional-rename" => 44,
        [flag] if flag == "--worker-intervening-edit-before-trash" => 45,
        [flag] if flag == "--worker-timeout-after-conditional-trash" => 46,
        [flag] if flag == "--worker-resume-timed-out-conditional-trash" => 47,
        [flag] if flag == "--worker-reserved-create-lost-receipt" => 48,
        [flag] if flag == "--worker-reconcile-reserved-create" => 49,
        [flag] if flag == "--worker-ordinary-name-create" => 50,
        [flag] if flag == "--worker-ordinary-name-collision" => 51,
        [flag] if flag == "--worker-bounded-binary-create" => 52,
        [flag] if flag == "--worker-streamed-binary-create" => 53,
        [flag] if flag == "--worker-streamed-lost-registration" => 54,
        [flag] if flag == "--worker-reconcile-streamed-registration" => 55,
        [flag] if flag == "--worker-streamed-lost-content" => 56,
        [flag] if flag == "--worker-retry-streamed-content" => 57,
        [flag] if flag == "--worker-create-folder" => 58,
        [flag] if flag == "--worker-discard-folder-receipt" => 59,
        [flag] if flag == "--worker-reconcile-folder" => 60,
        [flag] if flag == "--worker-discard-empty-folder-trash" => 61,
        [flag] if flag == "--worker-reconcile-empty-folder-trash" => 62,
        [flag] if flag == "--stale-etag-move" => 63,
        [flag] if flag == "--fresh-etag-move" => 64,
        [flag] if flag == "--metadata-stale-move" => 65,
        [flag] if flag == "--occupied-move-name" => 66,
        [flag] if flag == "--inspect-occupied-move" => 67,
        [flag, _folder_id] if flag == "--inspect-owned-folder" => 68,
        [flag, _run_id] if flag == "--reconcile-occupied-move" => 69,
        [flag] if flag == "--worker-owned-move" => 70,
        [flag] if flag == "--worker-discard-move-response" => 71,
        [flag, _run_id] if flag == "--worker-reconcile-move" => 72,
        [flag] if flag == "--worker-move-collision-race" => 73,
        [flag] if flag == "--empty-folder-move" => 75,
        [flag, _run_id] if flag == "--inspect-empty-folder-move" => 76,
        [flag] if flag == "--worker-discard-folder-move-response" => 77,
        [flag, _run_id] if flag == "--worker-reconcile-folder-move" => 78,
        [flag] if flag == "--populated-folder-move" => 79,
        [flag, _run_id] if flag == "--inspect-populated-folder-move" => 80,
        [flag] if flag == "--worker-discard-populated-folder-move-response" => 81,
        [flag, _run_id] if flag == "--worker-reconcile-populated-folder-move" => 82,
        _ => bail!(
            "usage: cirrove-icloud-write-probe [--same-id | --stale-etag | --rename-conflict | --metadata-rename | --stale-etag-move | --fresh-etag-move | --metadata-stale-move | --occupied-move-name | --inspect-occupied-move | --inspect-owned-folder ID | --reconcile-occupied-move UUID | --worker-owned-move | --worker-discard-move-response | --worker-reconcile-move UUID | --worker-move-collision-race | --empty-folder-move | --inspect-empty-folder-move UUID | --worker-discard-folder-move-response | --worker-reconcile-folder-move UUID | --populated-folder-move | --inspect-populated-folder-move UUID | --http-if-match | --occupied-name | --staged-handoff | --durable-stop-after-recovery | --durable-resume | --durable-drop-old-receipt | --durable-resume-lost-old | --durable-drop-new-receipt | --durable-resume-lost-new | --durable-drop-registration-receipt | --durable-resume-registration | --durable-handoff-registered | --stale-etag-trash | --stale-then-fresh-trash | --inspect-trash | --trash-restore-cycle | --worker-create | --worker-discard-registration-receipt | --worker-resume-registration | --owned-file-trash-adapter | --worker-owned-trash | --worker-discard-trash-receipt | --worker-resume-trash | --worker-owned-handoff | --worker-discard-old-handoff-receipt | --worker-resume-handoff | --worker-discard-new-handoff-receipt | --worker-reconcile-new-handoff | --owned-trash-download | --conditional-trash-handoff | --worker-conditional-trash-handoff | --worker-discard-conditional-trash-receipt | --worker-resume-conditional-trash | --inspect-conditional-trash-journal | --worker-resume-inspected-conditional-trash | --inspect-conditional-trash-receipt | --publish-conditional-trash-receipt | --worker-discard-conditional-rename-receipt | --worker-reconcile-conditional-rename | --worker-intervening-edit-before-trash | --worker-timeout-after-conditional-trash | --worker-resume-timed-out-conditional-trash | --worker-reserved-create-lost-receipt | --worker-reconcile-reserved-create | --worker-ordinary-name-create | --worker-ordinary-name-collision | --worker-bounded-binary-create | --worker-streamed-binary-create | --worker-streamed-lost-registration | --worker-reconcile-streamed-registration | --worker-streamed-lost-content | --worker-retry-streamed-content | --worker-create-folder | --worker-discard-folder-receipt | --worker-reconcile-folder | --worker-discard-empty-folder-trash | --worker-reconcile-empty-folder-trash]"
        ),
    };
    let state = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-gui-connect-validation/state")
        .canonicalize()
        .context("isolated iCloud validation state is unavailable")?;
    let settings = Settings::load(&state)?;
    let accounts: Vec<_> = settings
        .accounts
        .iter()
        .filter(|account| {
            matches!(account.registration, AppRegistration::ICloud)
                && account.label == "iCloudGuiValidation"
        })
        .collect();
    if accounts.len() != 1 {
        bail!("expected exactly one isolated iCloud validation account");
    }
    let account = accounts[0];
    let vault = SealedSessionVault::new(&state, &account.id)?;
    let snapshot = vault
        .load(&account.credential_id)
        .await?
        .context("isolated iCloud validation session is missing; sign in again locally")?;
    let mut session =
        ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
    let root = session
        .list_root()
        .await
        .context("saved iCloud session is not usable")?;
    if mode == 82 {
        let run_id = Uuid::parse_str(&arguments[1]).context("invalid populated-move run ID")?;
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../.local-state/icloud-worker-populated-folder-move-{run_id}"
        ));
        let fixture: PopulatedFolderMoveFixture =
            serde_json::from_slice(&fs::read(directory.join("fixture.json"))?)?;
        if fixture.account_id != account.id {
            bail!("populated-folder move record belongs to a different account");
        }
        let source = session
            .validation_folder_at_root(&fixture.source_id)
            .await?;
        let destination = session
            .validation_folder_at_root(&fixture.destination_id)
            .await?;
        if source.name() != fixture.source_name
            || destination.name() != fixture.destination_name
            || source.id() == destination.id()
        {
            bail!("recorded populated-move parent identity changed");
        }
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &directory,
            &account.id,
            8192,
        )?));
        let record = {
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let rows = guard.list_mutations(0, 2)?;
            if rows.len() != 1 || rows[0].state != MutationState::VerifyRequired {
                bail!("expected exactly one uncertain populated-folder move");
            }
            rows.into_iter().next().context("populated move missing")?
        };
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &record.request.intent
        else {
            bail!("uncertain mutation is not a folder move");
        };
        if record.request.scope.account != account.id
            || before.kind != NodeKind::Folder
            || before.id != fixture.nested_id
            || before.name != fixture.nested_name
            || before.etag.as_deref() != Some(fixture.nested_etag.as_str())
            || before.parent_id.as_deref() != Some(source.id())
            || parent != destination.id()
            || name != &fixture.nested_name
            || record.prepared_item.as_deref() != Some(before.id.as_str())
        {
            bail!("uncertain populated move lacks its exact prepared identity");
        }
        let provider = ICloudOwnedFixtureFolderMove::for_reconciliation(
            record.request.scope.clone(),
            session,
            source,
            destination,
            before.clone(),
        )?;
        if !provider.matches_child_record(
            &fixture.file_id,
            &fixture.file_document_id,
            &fixture.file_etag,
            fixture.file_size,
            &fixture.file_sha256,
        ) {
            bail!("journal child identity differs from synced fixture record");
        }
        let provider = Arc::new(provider);
        let worker = MutationWorker::new(journal.clone(), provider, CancellationToken::new());
        let result = worker
            .run_once()
            .await?
            .context("populated-folder move was not claimed")?;
        let saved = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .mutation(record.id)?;
        if result.id != record.id
            || result.state != MutationState::Applied
            || result.issue.is_some()
            || !saved
                .receipt
                .as_ref()
                .is_some_and(|receipt| record.request.accepts(receipt))
        {
            bail!("restarted worker could not reconcile exact populated-folder move");
        }
        println!(
            "Restarted worker reconciled exact folder and child bytes without another move. Operation: {}.",
            record.id
        );
        return Ok(());
    }
    if mode == 80 {
        let run_id = Uuid::parse_str(&arguments[1]).context("invalid populated-move run ID")?;
        let path = populated_folder_move_fixture_directory().join(format!("{run_id}.json"));
        let fixture: PopulatedFolderMoveFixture = serde_json::from_slice(&fs::read(path)?)?;
        if fixture.account_id != account.id
            || fixture.source_id == fixture.destination_id
            || fixture.file_size == 0
            || fixture.file_size > 4096
            || fixture.file_sha256.len() != 64
            || !fixture
                .file_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || fixture
                .nested_name
                .strip_prefix("Cirrove Nested Move-")
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
        {
            bail!("invalid populated-folder move record");
        }
        let source = session
            .validation_folder_at_root(&fixture.source_id)
            .await?;
        let destination = session
            .validation_folder_at_root(&fixture.destination_id)
            .await?;
        if source.name() != fixture.source_name
            || destination.name() != fixture.destination_name
            || source.id() == destination.id()
        {
            bail!("recorded populated-move parent identity changed");
        }
        let source_items = session.list_folder(source.id()).await?;
        let destination_items = session.list_folder(destination.id()).await?;
        let nested_items = session.list_folder(&fixture.nested_id).await?;
        if nested_items.len() != 1
            || nested_items[0].drivewsid != fixture.file_id
            || nested_items[0].docwsid != fixture.file_document_id
            || nested_items[0].display_name() != "created-by-cirrove.txt"
            || nested_items[0].size != fixture.file_size
            || nested_items[0].is_folder()
            || hex::encode(sha2::Sha256::digest(
                session
                    .read_small_file_in_folder(&fixture.nested_id, &fixture.file_id)
                    .await?,
            )) != fixture.file_sha256
        {
            bail!("nested file ID or full bytes changed after folder move");
        }
        if source_items.is_empty()
            && destination_items.len() == 1
            && destination_items[0].drivewsid == fixture.nested_id
            && destination_items[0].display_name() == fixture.nested_name
            && destination_items[0].is_folder()
        {
            println!(
                "Read-only restart found the exact folder and file IDs at destination with full bytes intact."
            );
        } else if source_items.len() == 1
            && source_items[0].drivewsid == fixture.nested_id
            && source_items[0].display_name() == fixture.nested_name
            && source_items[0].etag == fixture.nested_etag
            && destination_items.is_empty()
        {
            println!("Read-only restart found the populated folder unchanged at source.");
        } else {
            bail!("populated-folder move remains indeterminate by exact IDs");
        }
        return Ok(());
    }
    if mode == 78 {
        let run_id = Uuid::parse_str(&arguments[1]).context("invalid folder-move run ID")?;
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../.local-state/icloud-worker-folder-move-{run_id}"
        ));
        let fixture: EmptyFolderMoveFixture =
            serde_json::from_slice(&fs::read(directory.join("fixture.json"))?)?;
        if fixture.account_id != account.id {
            bail!("folder-move record belongs to a different account");
        }
        let source = session
            .validation_folder_at_root(&fixture.source_id)
            .await?;
        let destination = session
            .validation_folder_at_root(&fixture.destination_id)
            .await?;
        if source.name() != fixture.source_name
            || destination.name() != fixture.destination_name
            || source.id() == destination.id()
        {
            bail!("recorded folder-move parent identity changed");
        }
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &directory,
            &account.id,
            8192,
        )?));
        let record = {
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let rows = guard.list_mutations(0, 2)?;
            if rows.len() != 1 || rows[0].state != MutationState::VerifyRequired {
                bail!("expected exactly one uncertain folder move");
            }
            rows.into_iter().next().context("folder move missing")?
        };
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &record.request.intent
        else {
            bail!("uncertain mutation is not a folder move");
        };
        if before.kind != NodeKind::Folder
            || before.id != fixture.nested_id
            || before.name != fixture.nested_name
            || before.etag.as_deref() != Some(fixture.nested_etag.as_str())
            || before.parent_id.as_deref() != Some(source.id())
            || parent != destination.id()
            || name != &fixture.nested_name
            || record.prepared_item.as_deref() != Some(before.id.as_str())
        {
            bail!("uncertain folder move lacks its exact prepared identity");
        }
        let provider = Arc::new(ICloudOwnedFixtureFolderMove::for_reconciliation(
            record.request.scope.clone(),
            session,
            source,
            destination,
            before.clone(),
        )?);
        let worker = MutationWorker::new(journal.clone(), provider, CancellationToken::new());
        let result = worker
            .run_once()
            .await?
            .context("folder move was not claimed")?;
        let saved = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .mutation(record.id)?;
        if result.id != record.id
            || result.state != MutationState::Applied
            || result.issue.is_some()
            || !saved
                .receipt
                .as_ref()
                .is_some_and(|receipt| record.request.accepts(receipt))
        {
            bail!("restarted worker could not reconcile exact empty-folder move");
        }
        println!(
            "Restarted worker reconciled exact empty-folder move without another request. Operation: {}.",
            record.id
        );
        return Ok(());
    }
    if mode == 76 {
        let run_id = Uuid::parse_str(&arguments[1]).context("invalid folder-move run ID")?;
        let path = empty_folder_move_fixture_directory().join(format!("{run_id}.json"));
        let fixture: EmptyFolderMoveFixture = serde_json::from_slice(&fs::read(path)?)?;
        if fixture.account_id != account.id
            || fixture.source_id == fixture.destination_id
            || ![&fixture.source_name, &fixture.destination_name]
                .iter()
                .all(|name| {
                    name.strip_prefix("Cirrove Write Validation-")
                        .is_some_and(|suffix| Uuid::parse_str(suffix).is_ok())
                })
            || fixture
                .nested_name
                .strip_prefix("Cirrove Nested Move-")
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
            || fixture.nested_etag.is_empty()
        {
            bail!("invalid owned empty-folder move record");
        }
        let source = session
            .validation_folder_at_root(&fixture.source_id)
            .await?;
        let destination = session
            .validation_folder_at_root(&fixture.destination_id)
            .await?;
        if source.name() != fixture.source_name || destination.name() != fixture.destination_name {
            bail!("recorded folder-move parent identity changed");
        }
        let source_items = session.list_folder(source.id()).await?;
        let destination_items = session.list_folder(destination.id()).await?;
        let nested_items = session.list_folder(&fixture.nested_id).await?;
        if !nested_items.is_empty() {
            bail!("moved validation folder is no longer empty");
        }
        if source_items.is_empty()
            && destination_items.len() == 1
            && destination_items[0].drivewsid == fixture.nested_id
            && destination_items[0].display_name() == fixture.nested_name
            && destination_items[0].is_folder()
        {
            println!("Read-only restart found the exact empty folder ID in the destination.");
        } else if source_items.len() == 1
            && source_items[0].drivewsid == fixture.nested_id
            && source_items[0].display_name() == fixture.nested_name
            && source_items[0].etag == fixture.nested_etag
            && destination_items.is_empty()
        {
            println!("Read-only restart found the exact empty folder unchanged at source.");
        } else {
            bail!("empty-folder move remains indeterminate by exact ID");
        }
        return Ok(());
    }
    if mode == 72 {
        let run_id = Uuid::parse_str(&arguments[1]).context("invalid owned-move run ID")?;
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../.local-state/icloud-worker-move-{run_id}"));
        let folders: OwnedMoveFolders =
            serde_json::from_slice(&fs::read(directory.join("fixture.json"))?)?;
        let source = session
            .validation_folder_at_root(&folders.source_id)
            .await?;
        let destination = session
            .validation_folder_at_root(&folders.destination_id)
            .await?;
        if source.name() != folders.source_name
            || destination.name() != folders.destination_name
            || source.id() == destination.id()
        {
            bail!("owned move folder identity changed");
        }
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &directory,
            &account.id,
            8192,
        )?));
        let record = {
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let rows = guard.list_mutations(0, 2)?;
            if rows.len() != 1 || rows[0].state != MutationState::VerifyRequired {
                bail!("expected one uncertain owned move");
            }
            rows.into_iter().next().context("owned move missing")?
        };
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &record.request.intent
        else {
            bail!("uncertain mutation is not a move");
        };
        if before.parent_id.as_deref() != Some(source.id())
            || parent != destination.id()
            || name != "created-by-cirrove.txt"
            || record.prepared_item.as_deref() != Some(before.id.as_str())
        {
            bail!("uncertain owned move lacks its bound identity");
        }
        let provider = Arc::new(ICloudOwnedFixtureMove::for_reconciliation(
            record.request.scope.clone(),
            session,
            source,
            destination,
            before.clone(),
        )?);
        let worker = MutationWorker::new(journal.clone(), provider, CancellationToken::new());
        let result = worker
            .run_once()
            .await?
            .context("owned move was not claimed")?;
        let saved = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .mutation(record.id)?;
        if result.id != record.id
            || result.state != MutationState::Applied
            || result.issue.is_some()
            || !saved
                .receipt
                .as_ref()
                .is_some_and(|receipt| record.request.accepts(receipt))
        {
            bail!("restarted worker could not reconcile the exact owned move");
        }
        println!(
            "Restarted worker reconciled the exact owned move without another request. Operation: {}.",
            record.id
        );
        return Ok(());
    }
    if mode == 69 {
        let run_id = Uuid::parse_str(&arguments[1]).context("invalid occupied-move run ID")?;
        let path = occupied_move_fixture_directory().join(format!("{run_id}.json"));
        let fixture: OccupiedMoveFixture = serde_json::from_slice(&fs::read(path)?)?;
        if fixture.account_id != account.id
            || fixture.source_folder_id == fixture.destination_folder_id
            || fixture.moving_file_id == fixture.occupant_file_id
            || ![&fixture.source_folder_id, &fixture.destination_folder_id]
                .iter()
                .all(|id| id.starts_with("FOLDER::com.apple.CloudDocs::"))
            || ![&fixture.moving_file_id, &fixture.occupant_file_id]
                .iter()
                .all(|id| id.starts_with("FILE::com.apple.CloudDocs::"))
            || ![
                &fixture.source_folder_name,
                &fixture.destination_folder_name,
            ]
            .iter()
            .all(|name| {
                name.strip_prefix("Cirrove Write Validation-")
                    .is_some_and(|suffix| Uuid::parse_str(suffix).is_ok())
            })
            || fixture.moving_size == 0
            || fixture.moving_size > 4096
            || fixture.occupant_size == 0
            || fixture.occupant_size > 4096
            || [
                fixture.moving_sha256.as_str(),
                fixture.occupant_sha256.as_str(),
            ]
            .iter()
            .any(|hash| hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            bail!("invalid owned occupied-move fixture record");
        }
        for (id, name) in [
            (&fixture.source_folder_id, &fixture.source_folder_name),
            (
                &fixture.destination_folder_id,
                &fixture.destination_folder_name,
            ),
        ] {
            if root
                .iter()
                .filter(|entry| {
                    entry.drivewsid == *id && entry.display_name() == **name && entry.is_folder()
                })
                .count()
                != 1
            {
                bail!("recorded owned folder identity changed");
            }
        }
        let source = session.list_folder(&fixture.source_folder_id).await?;
        let destination = session.list_folder(&fixture.destination_folder_id).await?;
        let at_source: Vec<_> = source
            .iter()
            .filter(|entry| entry.drivewsid == fixture.moving_file_id)
            .collect();
        let at_destination: Vec<_> = destination
            .iter()
            .filter(|entry| entry.drivewsid == fixture.moving_file_id)
            .collect();
        let occupant: Vec<_> = destination
            .iter()
            .filter(|entry| entry.drivewsid == fixture.occupant_file_id)
            .collect();
        if at_source.len() > 1 || at_destination.len() > 1 || occupant.len() > 1 {
            bail!("duplicate exact file identity in occupied-move listing");
        }
        let source_hash = if let Some(entry) = at_source.first() {
            Some((
                entry,
                hex::encode(sha2::Sha256::digest(
                    session
                        .read_small_file_in_folder(&fixture.source_folder_id, &entry.drivewsid)
                        .await?,
                )),
            ))
        } else {
            None
        };
        let destination_hash = if let Some(entry) = at_destination.first() {
            Some((
                entry,
                hex::encode(sha2::Sha256::digest(
                    session
                        .read_small_file_in_folder(&fixture.destination_folder_id, &entry.drivewsid)
                        .await?,
                )),
            ))
        } else {
            None
        };
        let occupant_hash = if let Some(entry) = occupant.first() {
            Some((
                entry,
                hex::encode(sha2::Sha256::digest(
                    session
                        .read_small_file_in_folder(&fixture.destination_folder_id, &entry.drivewsid)
                        .await?,
                )),
            ))
        } else {
            None
        };
        let occupant_intact = occupant_hash.as_ref().is_some_and(|(entry, hash)| {
            !entry.is_folder()
                && entry.size == fixture.occupant_size
                && entry.etag == fixture.occupant_file_etag
                && entry.display_name() == "created-by-cirrove.txt"
                && *hash == fixture.occupant_sha256
        });
        let source_intact = source_hash.as_ref().is_some_and(|(entry, hash)| {
            !entry.is_folder()
                && entry.size == fixture.moving_size
                && entry.etag == fixture.moving_file_etag
                && entry.display_name() == "created-by-cirrove.txt"
                && *hash == fixture.moving_sha256
        });
        let destination_intact = destination_hash.as_ref().is_some_and(|(entry, hash)| {
            !entry.is_folder()
                && entry.size == fixture.moving_size
                && *hash == fixture.moving_sha256
        });
        if source.len() == 1 && destination.len() == 1 && source_intact && occupant_intact {
            println!("Recorded occupied move did not change either exact file ID or bytes.");
        } else if source.is_empty()
            && destination.len() == 2
            && destination_intact
            && occupant_intact
        {
            let moved_name = destination_hash
                .as_ref()
                .map(|(entry, _)| entry.display_name())
                .context("moved item missing")?;
            println!(
                "Recorded occupied move kept both exact IDs and bytes; moving name={moved_name}"
            );
        } else {
            bail!("recorded occupied move remains indeterminate by exact IDs and bytes");
        }
        return Ok(());
    }
    if mode == 68 {
        let folder_id = &arguments[1];
        let folder = root
            .iter()
            .find(|entry| {
                entry.drivewsid == *folder_id
                    && entry.is_folder()
                    && entry
                        .display_name()
                        .starts_with("Cirrove Write Validation-")
            })
            .context("specified folder is not an owned validation folder at root")?;
        let children = session.list_folder(&folder.drivewsid).await?;
        println!("folder={} children={}", folder.drivewsid, children.len());
        for item in &children {
            if item.is_folder() || item.size > 4096 {
                println!(
                    "item={} name={} kind={} size={}",
                    item.drivewsid,
                    item.display_name(),
                    item.kind,
                    item.size
                );
                continue;
            }
            let bytes = session
                .read_small_file_in_folder(&folder.drivewsid, &item.drivewsid)
                .await?;
            let role = if bytes.starts_with(b"Cirrove occupied move destination ") {
                "occupied_move_destination"
            } else if bytes.starts_with(b"Cirrove write validation ") {
                "validation_source_candidate"
            } else {
                "other_owned_fixture"
            };
            println!(
                "item={} document={} name={} etag={} size={} role={} sha256={}",
                item.drivewsid,
                item.docwsid,
                item.display_name(),
                item.etag,
                item.size,
                role,
                hex::encode(sha2::Sha256::digest(&bytes))
            );
        }
        return Ok(());
    }
    if mode == 67 {
        let candidates: Vec<_> = root
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.is_folder()
                    && entry
                        .display_name()
                        .starts_with("Cirrove Write Validation-")
            })
            .collect();
        if candidates.len() > 256 {
            bail!("too many owned validation folders for bounded inspection");
        }
        println!("owned_validation_folders={}", candidates.len());
        for (root_index, folder) in candidates {
            let children = session.list_folder(&folder.drivewsid).await?;
            if children.len() > 16 {
                continue;
            }
            let mut summaries = Vec::new();
            for item in children.iter().filter(|item| {
                !item.is_folder()
                    && item.display_name() == "created-by-cirrove.txt"
                    && item.size <= 4096
            }) {
                let bytes = session
                    .read_small_file_in_folder(&folder.drivewsid, &item.drivewsid)
                    .await?;
                let role = if bytes.starts_with(b"Cirrove occupied move destination ") {
                    "occupied_move_destination"
                } else if bytes.starts_with(b"Cirrove write validation ") {
                    "validation_source_candidate"
                } else {
                    "other_owned_fixture"
                };
                summaries.push(format!(
                    "{}:{}:{}:{}:{}",
                    role,
                    item.drivewsid,
                    item.docwsid,
                    item.etag,
                    hex::encode(sha2::Sha256::digest(&bytes))
                ));
            }
            if !summaries.is_empty() {
                println!(
                    "root_index={} folder={} children={} files={}",
                    root_index,
                    folder.drivewsid,
                    children.len(),
                    summaries.join(",")
                );
            }
        }
        return Ok(());
    }
    let folder_recovery_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-folder-lost-receipt-validation");
    if mode == 59 && folder_recovery_directory.exists() {
        bail!("lost folder receipt validation journal already exists");
    }
    if mode == 60 {
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &folder_recovery_directory,
            &account.id,
            8192,
        )?));
        let record = {
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let records = guard.list_mutations(0, 2)?;
            if records.len() != 1 || records[0].state != MutationState::VerifyRequired {
                bail!("expected exactly one uncertain folder creation");
            }
            records
                .into_iter()
                .next()
                .context("folder mutation missing")?
        };
        let MutationIntent::CreateFolder { parent, .. } = &record.request.intent else {
            bail!("uncertain mutation is not a folder creation");
        };
        let folder = session.validation_folder_at_root(parent).await?;
        let provider = Arc::new(
            ICloudOwnedFixtureFolderCreate::new(
                record.request.scope.clone(),
                session,
                folder,
                Arc::new(DesktopVault),
            )?
            .reconciliation_only(),
        );
        let worker = MutationWorker::new(journal.clone(), provider, CancellationToken::new());
        let result = worker
            .run_once()
            .await?
            .context("folder mutation was not claimed")?;
        if result.id != record.id
            || result.state != MutationState::Applied
            || result.issue.is_some()
        {
            bail!("restarted worker could not reconcile the exact folder ID");
        }
        let saved = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .mutation(record.id)?;
        let Some(MutationReceipt::Upsert(node)) = saved.receipt else {
            bail!("restarted folder mutation lacks its exact receipt");
        };
        if !record
            .request
            .accepts(&MutationReceipt::Upsert(node.clone()))
        {
            bail!("restarted folder receipt does not match the request");
        }
        println!(
            "Restarted worker reconciled the saved exact folder ID without another create request. Operation: {}.",
            record.id
        );
        return Ok(());
    }
    let folder_remove_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-empty-folder-trash-validation");
    if mode == 61 && folder_remove_directory.exists() {
        bail!("empty-folder Trash validation journal already exists");
    }
    if mode == 62 {
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &folder_remove_directory,
            &account.id,
            8192,
        )?));
        let record = {
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let rows = guard.list_mutations(0, 2)?;
            if rows.len() != 1 || rows[0].state != MutationState::VerifyRequired {
                bail!("expected exactly one uncertain empty-folder Trash mutation");
            }
            rows.into_iter()
                .next()
                .context("folder Trash mutation missing")?
        };
        let MutationIntent::RemoveFolder { before } = &record.request.intent else {
            bail!("uncertain mutation is not empty-folder removal");
        };
        if record.prepared_item.as_deref() != Some(before.id.as_str()) {
            bail!("uncertain folder removal lacks its exact prepared identity");
        }
        let parent = before
            .parent_id
            .as_deref()
            .context("folder has no parent ID")?;
        let folder = session.validation_folder_at_root(parent).await?;
        let provider = Arc::new(
            ICloudOwnedFixtureFolderRemove::new(
                record.request.scope.clone(),
                session,
                folder,
                before.clone(),
            )?
            .reconciliation_only(),
        );
        let worker = MutationWorker::new(journal.clone(), provider, CancellationToken::new());
        let result = worker
            .run_once()
            .await?
            .context("folder Trash was not claimed")?;
        if result.id != record.id
            || result.state != MutationState::Applied
            || result.issue.is_some()
        {
            bail!("restarted worker could not reconcile exact empty-folder Trash");
        }
        let saved = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .mutation(record.id)?;
        if !matches!(saved.receipt, Some(MutationReceipt::Removed { item }) if item == before.id) {
            bail!("reconciled folder Trash receipt has a different item ID");
        }
        println!(
            "Restarted worker reconciled the exact empty folder in Trash without another delete request. Operation: {}.",
            record.id
        );
        return Ok(());
    }
    if mode == 61 {
        let creator = UploadJournal::open(&folder_recovery_directory, &account.id, 8192)?;
        let rows = creator.list_mutations(0, 2)?;
        if rows.len() != 1 || rows[0].state != MutationState::Applied {
            bail!("owned folder Create receipt is unavailable");
        }
        let created = &rows[0];
        let Some(MutationReceipt::Upsert(before)) = &created.receipt else {
            bail!("owned folder Create has no exact upsert receipt");
        };
        if !created
            .request
            .accepts(&MutationReceipt::Upsert(before.clone()))
        {
            bail!("owned folder Create receipt does not match its request");
        }
        let parent = before
            .parent_id
            .as_deref()
            .context("created folder has no parent")?;
        let folder = session.validation_folder_at_root(parent).await?;
        let scope = created.request.scope.clone();
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::RemoveFolder {
                before: before.clone(),
            },
        };
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &folder_remove_directory,
            &account.id,
            8192,
        )?));
        let queued = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .enqueue_mutation(request)?;
        let provider = Arc::new(
            ICloudOwnedFixtureFolderRemove::new(scope, session, folder, before.clone())?
                .with_discarded_trash_receipt(),
        );
        let worker = MutationWorker::new(journal.clone(), provider, CancellationToken::new());
        let result = worker
            .run_once()
            .await?
            .context("empty-folder Trash was not claimed")?;
        let saved = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .mutation(queued.id)?;
        if result.id != queued.id
            || result.state != MutationState::VerifyRequired
            || saved.prepared_item.as_deref() != Some(before.id.as_str())
        {
            bail!("discarded empty-folder Trash response lost its exact prepared identity");
        }
        println!(
            "Empty-folder Trash response deliberately discarded; exact ID retained as VerifyRequired. Operation: {}.",
            queued.id
        );
        return Ok(());
    }
    let handoff_recovery_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-handoff-lost-old-validation");
    let handoff_new_recovery_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-handoff-lost-new-validation");
    let conditional_trash_recovery_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-conditional-trash-lost-validation");
    let conditional_rename_recovery_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-conditional-rename-lost-validation");
    let conditional_trash_timeout_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-conditional-trash-timeout-validation");
    if mode == 30 && handoff_recovery_directory.exists() {
        bail!("lost old-rename validation journal already exists");
    }
    if mode == 32 && handoff_new_recovery_directory.exists() {
        bail!("lost new-rename validation journal already exists");
    }
    if mode == 37 && conditional_trash_recovery_directory.exists() {
        bail!("lost conditional-Trash validation journal already exists");
    }
    if mode == 43 && conditional_rename_recovery_directory.exists() {
        bail!("lost conditional-rename validation journal already exists");
    }
    if mode == 46 && conditional_trash_timeout_directory.exists() {
        bail!("timed-out conditional-Trash validation journal already exists");
    }
    if matches!(mode, 39 | 41 | 42) {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local-state")
            .canonicalize()?;
        let directory = std::path::PathBuf::from(std::env::var("CIRROVE_ICLOUD_INSPECT_JOURNAL")?)
            .canonicalize()?;
        if directory.parent() != Some(base.as_path())
            || !directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("icloud-worker-handoff-"))
        {
            bail!("inspect journal is outside isolated iCloud worker state");
        }
        let mut journal = UploadJournal::open(&directory, &account.id, 8192)?;
        let records = journal.list(0, 2)?;
        if records.len() != 1 || records[0].state != UploadState::VerifyRequired {
            bail!("expected exactly one uncertain fixture handoff");
        }
        let record = &records[0];
        let request = UploadRequest {
            scope: record.scope.clone(),
            intent: record.intent.clone(),
            size: record.size,
            sha256: record.sha256.clone(),
        };
        let checkpoint = DesktopVault
            .load(&format!("upload/{}", record.id))
            .await?
            .context("uncertain handoff lacks its saved checkpoint")?;
        let provider =
            ICloudOwnedFixtureHandoff::from_checkpoint(&request, record.id, &checkpoint, session)?;
        if mode == 39 {
            println!(
                "Read-only exact-ID fixture phase: {:?}.",
                provider.inspect_owned_fixture().await?
            );
        } else if mode == 41 {
            let (current, backup) = provider.inspect_owned_receipt().await?;
            println!(
                "Read-only exact-ID receipt: current size matches journal: {}; backup is in Trash: {}; both revisions present: {}.",
                current.size == record.size,
                backup.parent_id.as_deref() == Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT"),
                current.content_revision().is_some() && backup.content_revision().is_some(),
            );
        } else {
            // Provider I/O completes before a local journal transaction starts.
            // This mode never sends an iCloud mutation or fabricates a receipt.
            let (current, backup) = provider.inspect_owned_receipt().await?;
            let claimed = journal.claim_verification(record.id)?;
            let attempt = claimed.attempt.context("verification attempt missing")?;
            if let Err(error) =
                journal.acknowledge_identity_handoff(record.id, attempt, current, backup)
            {
                journal.stop_attempt(record.id, attempt, UploadState::VerifyRequired)?;
                return Err(error.into());
            }
            let committed = journal.get(record.id)?;
            if committed.state != UploadState::Uploaded {
                bail!("verified two-ID receipt was not published");
            }
            println!(
                "Verified exact-ID Trash handoff published in the durable journal without a cloud mutation."
            );
        }
        return Ok(());
    }
    if matches!(mode, 31 | 33 | 38 | 40 | 44 | 47) {
        let recovery_directory = if mode == 31 {
            handoff_recovery_directory
        } else if mode == 38 {
            conditional_trash_recovery_directory
        } else if mode == 44 {
            conditional_rename_recovery_directory
        } else if mode == 47 {
            conditional_trash_timeout_directory
        } else if mode == 40 {
            let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local-state")
                .canonicalize()?;
            let directory =
                std::path::PathBuf::from(std::env::var("CIRROVE_ICLOUD_RESUME_JOURNAL")?)
                    .canonicalize()?;
            if directory.parent() != Some(base.as_path())
                || !directory
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("icloud-worker-handoff-"))
            {
                bail!("resume journal is outside isolated iCloud worker state");
            }
            directory
        } else {
            handoff_new_recovery_directory
        };
        if !recovery_directory.exists() {
            bail!("lost-rename validation journal is absent");
        }
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &recovery_directory,
            &account.id,
            8192,
        )?));
        let record = {
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let records = guard.list(0, 2)?;
            if records.len() != 1 || records[0].state != UploadState::VerifyRequired {
                bail!("expected exactly one uncertain handoff upload");
            }
            records
                .into_iter()
                .next()
                .context("handoff upload is absent")?
        };
        let request = UploadRequest {
            scope: record.scope.clone(),
            intent: record.intent.clone(),
            size: record.size,
            sha256: record.sha256.clone(),
        };
        let UploadIntent::Replace { item, .. } = &record.intent else {
            bail!("uncertain handoff is not a replacement");
        };
        let vault = Arc::new(DesktopVault);
        let checkpoint = vault
            .load(&format!("upload/{}", record.id))
            .await?
            .context("uncertain handoff lacks its saved checkpoint")?;
        let mut provider =
            ICloudOwnedFixtureHandoff::from_checkpoint(&request, record.id, &checkpoint, session)?;
        if matches!(mode, 33 | 44) {
            provider = provider.reconciliation_only();
        }
        let provider = Arc::new(provider);
        let worker =
            TransferWorker::new(journal.clone(), provider, vault, CancellationToken::new());
        for _ in 0..5 {
            if let Some(result) = worker.run_once().await? {
                if result.id != record.id {
                    bail!("restarted worker claimed a different handoff");
                }
                if result.state == UploadState::Uploaded {
                    let guard = journal
                        .lock()
                        .map_err(|_| anyhow::anyhow!("journal lock"))?;
                    let saved = guard.get(record.id)?;
                    if saved.remote.as_ref().is_none_or(|node| node.id == *item)
                        || guard
                            .namespace_by_remote(&record.scope, item)?
                            .is_none_or(|backup| {
                                !backup.unlinked
                                    || !backup.remote_owned
                                    || (matches!(mode, 38 | 40 | 44 | 47)
                                        && backup
                                            .remote
                                            .as_ref()
                                            .and_then(|node| node.parent_id.as_deref())
                                            != Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT"))
                            })
                    {
                        bail!("restarted handoff lacks its exact two-ID receipt");
                    }
                    if mode == 44 {
                        println!(
                            "Read-only restarted worker reconciled the complete conditional Trash handoff without another rename or Trash request. Operation: {}.",
                            record.id
                        );
                    } else if matches!(mode, 38 | 40 | 47) {
                        println!(
                            "Restarted worker reconciled the conditional Trash handoff and published both exact IDs without a second Trash request. Operation: {}.",
                            record.id
                        );
                    } else if mode == 33 {
                        println!(
                            "Read-only restarted worker reconciled the exact completed two-ID handoff without either rename request. Operation: {}.",
                            record.id
                        );
                    } else {
                        println!(
                            "Restarted worker completed the old-then-new handoff without replaying the old rename. Operation: {}.",
                            record.id
                        );
                    }
                    return Ok(());
                }
                if result.state != UploadState::VerifyRequired {
                    bail!("restarted handoff needs review");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        }
        bail!("restarted handoff remains uncertain; no rename was blindly replayed");
    }
    if mode == 20 {
        let listing = session.inspect_trash_listing().await?;
        println!(
            "Trash metadata: {} entries, complete count: {}, restore-path entries: {}.",
            listing.entries, listing.complete, listing.entries_with_restore_path
        );
        return Ok(());
    }
    if mode == 7 {
        for candidate in root.iter().filter(|entry| {
            entry.is_folder()
                && entry
                    .display_name()
                    .starts_with("Cirrove Write Validation-")
        }) {
            let children = session.list_folder(&candidate.drivewsid).await?;
            if children.len() != 2 {
                continue;
            }
            let labels: Vec<_> = children
                .iter()
                .map(|entry| {
                    let name = entry.display_name();
                    if name.starts_with("created-by-cirrove")
                        || name.starts_with("staged-by-cirrove-")
                    {
                        name
                    } else {
                        "other".into()
                    }
                })
                .collect();
            println!("Two-item validation folder: {labels:?}");
        }
        return Ok(());
    }
    let journal_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(match mode {
        11 | 12 => "../../.local-state/icloud-lost-old-receipt-validation",
        13 | 14 => "../../.local-state/icloud-lost-new-receipt-validation",
        15..=17 => "../../.local-state/icloud-lost-registration-receipt-validation",
        _ => "../../.local-state/icloud-durable-handoff-validation",
    });
    if matches!(mode, 10 | 12 | 14) {
        let mut journal = Journal::load(&journal_directory, &account.id)?;
        journal.resume(&mut session).await?;
        println!(
            "Durable iCloud validation handoff reconciled and completed; both versions remain."
        );
        return Ok(());
    }
    if mode == 16 {
        let mut journal = StageJournal::load(&journal_directory, &account.id)?;
        journal.resume(&mut session).await?;
        println!(
            "Staged iCloud file registration reconciled by exact parent, unique name, remote ID and full hash; no request was replayed."
        );
        return Ok(());
    }
    if mode == 17 {
        let mut stage = StageJournal::load(&journal_directory, &account.id)?;
        let plan = stage.prepare_handoff_plan(&mut session).await?;
        let mut handoff = Journal::create(&journal_directory, &account.id, plan)?;
        handoff.resume(&mut session).await?;
        println!(
            "Previously reconciled staged registration completed its durable two-ID handoff; both versions remain."
        );
        return Ok(());
    }
    let worker_recovery_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-lost-receipt-validation");
    let reserved_create_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-reserved-create-validation");
    let streamed_registration_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-streamed-registration-validation");
    let streamed_content_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-streamed-content-validation");
    if mode == 57 {
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &streamed_content_directory,
            &account.id,
            64 * 1024 * 1024,
        )?));
        let record = {
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let rows = guard.list(0, 2)?;
            if rows.len() != 1 || rows[0].state != UploadState::VerifyRequired {
                bail!("expected exactly one uncertain streamed content upload");
            }
            rows.into_iter()
                .next()
                .context("streamed content upload is absent")?
        };
        let UploadIntent::Create { parent, .. } = &record.intent else {
            bail!("streamed content operation is not a create");
        };
        let folder = session.validation_folder_at_root(parent).await?;
        let provider = Arc::new(ICloudOwnedFixtureUpload::new(
            record.scope.clone(),
            session,
            folder,
        )?);
        let request = UploadRequest {
            scope: record.scope.clone(),
            intent: record.intent.clone(),
            size: record.size,
            sha256: record.sha256.clone(),
        };
        let checkpoint = DesktopVault
            .load(&format!("upload/{}", record.id))
            .await?
            .context("content upload checkpoint is missing")?;
        if provider.content_receipt_saved(&request, &checkpoint)? {
            bail!("content receipt unexpectedly reached the vault");
        }
        let abandoned_id = provider
            .reserved_document_id(&request, &checkpoint)?
            .context("content upload lacks its allocated document ID")?;
        let worker = TransferWorker::new(
            journal.clone(),
            provider,
            Arc::new(DesktopVault),
            CancellationToken::new(),
        );
        let first = worker
            .run_once()
            .await?
            .context("uncertain content was not claimed")?;
        if first.id != record.id || first.state != UploadState::Pending {
            bail!("content-only uncertainty was not classified as uncommitted");
        }
        let mut second = None;
        for _ in 0..4 {
            if let Some(result) = worker.run_once().await? {
                second = Some(result);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let second = second.context("fresh-slot retry was not claimed")?;
        if second.id != record.id || second.state != UploadState::Uploaded || second.issue.is_some()
        {
            bail!("fresh-slot retry did not reach Uploaded");
        }
        let remote = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .get(record.id)?
            .remote
            .context("retry has no remote receipt")?;
        if remote.id.rsplit("::").next() == Some(abandoned_id.as_str())
            || remote.parent_id.as_deref() != Some(parent.as_str())
            || remote.size != record.size
        {
            bail!("retry reused the uncertain slot or returned a wrong item");
        }
        let mut verify =
            ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
        let items = verify.list_folder(parent).await?;
        if items.len() != 1 || items[0].drivewsid != remote.id || items[0].docwsid == abandoned_id {
            bail!("uncertain content slot became a visible second file");
        }
        println!(
            "A fresh slot completed the exact 20 MiB file after a discarded content response; the abandoned document ID is not visible. Operation: {}.",
            record.id
        );
        return Ok(());
    }
    if matches!(mode, 24 | 49 | 55) {
        let recovery_directory = if mode == 55 {
            streamed_registration_directory.clone()
        } else if mode == 49 {
            reserved_create_directory.clone()
        } else {
            worker_recovery_directory.clone()
        };
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &recovery_directory,
            &account.id,
            if mode == 55 { 64 * 1024 * 1024 } else { 8192 },
        )?));
        let record = {
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let rows = guard.list(0, 2)?;
            if rows.len() != 1 || rows[0].state != UploadState::VerifyRequired {
                bail!("expected exactly one uncertain validation upload");
            }
            rows.into_iter()
                .next()
                .context("validation upload is absent")?
        };
        let UploadIntent::Create { parent, .. } = &record.intent else {
            bail!("validation upload is not a create");
        };
        let folder = session.validation_folder_at_root(parent).await?;
        let provider = ICloudOwnedFixtureUpload::new(record.scope.clone(), session, folder)?
            .reconciliation_only();
        let reserved_id = if matches!(mode, 49 | 55) {
            let request = UploadRequest {
                scope: record.scope.clone(),
                intent: record.intent.clone(),
                size: record.size,
                sha256: record.sha256.clone(),
            };
            let checkpoint = DesktopVault
                .load(&format!("upload/{}", record.id))
                .await?
                .context("reserved create checkpoint is missing")?;
            if mode == 55 && !provider.content_receipt_saved(&request, &checkpoint)? {
                bail!("streamed registration lacks the saved content receipt");
            }
            Some(
                provider
                    .reserved_document_id(&request, &checkpoint)?
                    .context("create checkpoint lacks its allocated document ID")?,
            )
        } else {
            None
        };
        let provider = Arc::new(provider);
        let worker = TransferWorker::new(
            journal.clone(),
            provider,
            Arc::new(DesktopVault),
            CancellationToken::new(),
        );
        let mut result = None;
        for _ in 0..4 {
            if let Some(claimed) = worker.run_once().await? {
                result = Some(claimed);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let result = result.context("uncertain validation upload was not claimed")?;
        if result.id != record.id || result.state != UploadState::Uploaded || result.issue.is_some()
        {
            bail!("reconciled upload did not reach Uploaded");
        }
        let receipt = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .get(record.id)?;
        let remote = receipt
            .remote
            .context("reconciled upload lacks remote item")?;
        if remote.id.is_empty()
            || remote.parent_id.as_deref() != Some(parent.as_str())
            || remote.size != record.size
            || reserved_id
                .as_ref()
                .is_some_and(|id| remote.id.rsplit("::").next() != Some(id.as_str()))
        {
            bail!("reconciled upload has a mismatched remote receipt");
        }
        println!(
            "Restarted worker reconciled the exact remote item and reached Uploaded without permitting another upload request. Operation: {}.",
            record.id
        );
        return Ok(());
    }
    let trash_recovery_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-worker-trash-lost-receipt-validation");
    if mode == 28 {
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &trash_recovery_directory,
            &account.id,
            8192,
        )?));
        let record = {
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let rows = guard.list_mutations(0, 2)?;
            if rows.len() != 1 || rows[0].state != MutationState::VerifyRequired {
                bail!("expected exactly one uncertain Trash mutation");
            }
            rows.into_iter()
                .next()
                .context("Trash mutation is absent")?
        };
        let MutationIntent::RemoveFile { before } = &record.request.intent else {
            bail!("uncertain Trash mutation is not a file removal");
        };
        if record.prepared_item.as_deref() != Some(before.id.as_str()) {
            bail!("uncertain Trash mutation lacks its exact prepared identity");
        }
        let parent = before
            .parent_id
            .as_deref()
            .context("Trash source has no parent")?;
        let folder = session.validation_folder_at_root(parent).await?;
        let provider = Arc::new(ICloudOwnedFixtureRemove::for_reconciliation(
            record.request.scope.clone(),
            session,
            folder,
            before.clone(),
        )?);
        let worker = MutationWorker::new(journal.clone(), provider, CancellationToken::new());
        for _ in 0..4 {
            if let Some(result) = worker.run_once().await? {
                if result.id != record.id
                    || result.state != MutationState::Applied
                    || result.issue.is_some()
                {
                    bail!("restarted Trash worker could not reconcile exact removal");
                }
                let saved = journal
                    .lock()
                    .map_err(|_| anyhow::anyhow!("journal lock"))?
                    .mutation(record.id)?;
                if !matches!(saved.receipt, Some(MutationReceipt::Removed { item }) if item == before.id)
                {
                    bail!("restarted Trash worker saved a different receipt");
                }
                println!(
                    "Restarted worker reconciled the exact Trash item without permitting another delete request. Operation: {}.",
                    record.id
                );
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        bail!("uncertain Trash mutation was not claimed for verification");
    }
    if mode == 23 && worker_recovery_directory.exists() {
        bail!("lost-receipt validation journal already exists");
    }
    if mode == 48 && reserved_create_directory.exists() {
        bail!("reserved create validation journal already exists");
    }
    if mode == 27 && trash_recovery_directory.exists() {
        bail!("lost Trash receipt validation journal already exists");
    }
    if mode == 51 {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local-state")
            .canonicalize()?;
        let prior_directory =
            std::path::PathBuf::from(std::env::var("CIRROVE_ICLOUD_PRIOR_CREATE_JOURNAL")?)
                .canonicalize()?;
        if prior_directory.parent() != Some(base.as_path())
            || !prior_directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("icloud-worker-create-"))
        {
            bail!("prior create journal is outside the isolated fixture state");
        }
        let prior = UploadJournal::open(&prior_directory, &account.id, 8192)?;
        let rows = prior.list(0, 2)?;
        if rows.len() != 1 || rows[0].state != UploadState::Uploaded {
            bail!("prior owned filename fixture is absent");
        }
        let original = &rows[0];
        let UploadIntent::Create { parent, name } = &original.intent else {
            bail!("prior fixture is not a created file");
        };
        if name != "Résumé 2026 final.txt"
            || original.scope.account != account.id
            || original.scope.provider != "icloud"
            || original.scope.collection != "drive"
        {
            bail!("prior fixture has the wrong account or name");
        }
        let old = original
            .remote
            .as_ref()
            .context("prior fixture has no remote identity")?;
        let folder = session.validation_folder_at_root(parent).await?;
        let initial = session.list_folder(folder.id()).await?;
        if initial.len() != 1
            || initial[0].drivewsid != old.id
            || initial[0].display_name() != *name
            || hex::encode(sha2::Sha256::digest(
                session
                    .read_small_file_in_folder(folder.id(), &old.id)
                    .await?,
            )) != original.sha256
        {
            bail!("prior owned filename fixture changed");
        }
        let contents = format!("Cirrove collision validation {}\n", Uuid::new_v4());
        let directory = base.join(format!("icloud-worker-collision-{}", Uuid::new_v4()));
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &directory,
            &account.id,
            8192,
        )?));
        let record = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .enqueue(
                original.scope.clone(),
                original.intent.clone(),
                contents.as_bytes(),
            )?;
        let provider = Arc::new(ICloudOwnedFixtureUpload::new(
            original.scope.clone(),
            session,
            folder,
        )?);
        let worker = TransferWorker::new(
            journal.clone(),
            provider.clone(),
            Arc::new(DesktopVault),
            CancellationToken::new(),
        );
        let result = worker
            .run_once()
            .await?
            .context("collision worker did not run")?;
        if result.id != record.id || result.state != UploadState::Conflict {
            bail!("occupied owned filename did not stop as a conflict");
        }
        let saved = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .get(record.id)?;
        if saved.remote.is_some() || saved.state != UploadState::Conflict {
            bail!("occupied filename was incorrectly acknowledged");
        }
        let request = UploadRequest {
            scope: record.scope.clone(),
            intent: record.intent.clone(),
            size: record.size,
            sha256: record.sha256.clone(),
        };
        let checkpoint = DesktopVault
            .load(&format!("upload/{}", record.id))
            .await?
            .context("collision checkpoint was not saved")?;
        let reserved_id = provider
            .reserved_document_id(&request, &checkpoint)?
            .context("collision lacks its allocated document ID")?;
        let mut verify =
            ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
        let after = verify.list_folder(parent).await?;
        if after.len() != 1
            || after[0].drivewsid != old.id
            || after[0].display_name() != *name
            || after[0].docwsid == reserved_id
            || hex::encode(sha2::Sha256::digest(
                verify.read_small_file_in_folder(parent, &old.id).await?,
            )) != original.sha256
        {
            bail!("occupied filename or original bytes changed");
        }
        println!(
            "The existing owned filename and full bytes remained intact; a different reserved create identity stopped at Conflict. Operation: {}.",
            record.id
        );
        return Ok(());
    }
    let name = format!("Cirrove Write Validation-{}", Uuid::new_v4());
    let folder = session.create_validation_folder(&name).await?;
    println!("Created and listed the isolated iCloud validation folder.");
    if matches!(mode, 79 | 81) {
        let destination = session
            .create_validation_folder(&format!("Cirrove Write Validation-{}", Uuid::new_v4()))
            .await?;
        let nested_name = format!("Cirrove Nested Move-{}", Uuid::new_v4());
        let (nested, _) = session
            .create_empty_nested_move_fixture(&folder, &nested_name)
            .await?;
        let bytes = format!("Cirrove nested move file {}\n", Uuid::new_v4());
        let file = session
            .create_file_in_nested_move_fixture(&folder, &nested, bytes.as_bytes())
            .await?;
        let source_items = session.list_folder(folder.id()).await?;
        if source_items.len() != 1
            || source_items[0].drivewsid != nested.id()
            || source_items[0].display_name() != nested.name()
            || source_items[0].etag.is_empty()
        {
            bail!("populated folder no longer has its exact source identity");
        }
        let nested_etag = source_items[0].etag.clone();
        let fixture = PopulatedFolderMoveFixture {
            account_id: account.id.clone(),
            source_id: folder.id().to_owned(),
            source_name: folder.name().to_owned(),
            destination_id: destination.id().to_owned(),
            destination_name: destination.name().to_owned(),
            nested_id: nested.id().to_owned(),
            nested_name: nested.name().to_owned(),
            nested_etag: nested_etag.clone(),
            file_id: file.id().to_owned(),
            file_document_id: file.document_id().to_owned(),
            file_etag: file.etag().to_owned(),
            file_size: bytes.len() as u64,
            file_sha256: hex::encode(sha2::Sha256::digest(bytes.as_bytes())),
        };
        if mode == 81 {
            let scope = Scope {
                account: account.id.clone(),
                provider: "icloud".into(),
                collection: "drive".into(),
            };
            let provider = ICloudOwnedFixtureFolderMove::new_with_child(
                scope.clone(),
                session,
                folder,
                destination.clone(),
                nested,
                nested_etag,
                &file,
                bytes.as_bytes(),
            )?;
            let request = MutationRequest {
                scope,
                intent: MutationIntent::Relocate {
                    before: provider.before_node(),
                    parent: destination.id().to_owned(),
                    name: nested_name,
                },
            };
            let run_id = Uuid::new_v4();
            let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../.local-state/icloud-worker-populated-folder-move-{run_id}"
            ));
            let journal = Arc::new(Mutex::new(UploadJournal::open(
                &directory,
                &account.id,
                8192,
            )?));
            let mut fixture_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join("fixture.json"))?;
            fixture_file.write_all(&serde_json::to_vec_pretty(&fixture)?)?;
            fixture_file.write_all(b"\n")?;
            fixture_file.sync_all()?;
            fs::File::open(&directory)?.sync_all()?;
            let queued = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?
                .enqueue_mutation(request.clone())?;
            println!(
                "Owned populated-folder move journal recorded as {run_id} before the request."
            );
            let worker = MutationWorker::new(
                journal.clone(),
                Arc::new(provider.with_discarded_response()),
                CancellationToken::new(),
            );
            let result = worker
                .run_once()
                .await?
                .context("populated-folder move was not claimed")?;
            let saved = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?
                .mutation(queued.id)?;
            if result.id != queued.id
                || result.state != MutationState::VerifyRequired
                || saved.prepared_item.as_deref()
                    != request.intent.before().map(|node| node.id.as_str())
            {
                bail!("discarded populated-folder move response lost prepared identity");
            }
            println!(
                "Populated-folder move response deliberately discarded; run --worker-reconcile-populated-folder-move {run_id} in a new process."
            );
            return Ok(());
        }
        let run_id = save_populated_folder_move_fixture(&fixture)?;
        println!(
            "Populated-folder move identities and full digest recorded as {run_id} before the request."
        );
        match session
            .probe_populated_nested_folder_move(
                &folder,
                &destination,
                &nested,
                &nested_etag,
                &file,
                bytes.as_bytes(),
            )
            .await?
        {
            PopulatedFolderMoveOutcome::MovedExactTree => println!(
                "One conditional folder move retained the exact folder and file IDs and full bytes at destination."
            ),
            PopulatedFolderMoveOutcome::RejectedIntact => println!(
                "Populated-folder move was rejected; exact folder and file bytes remain at source."
            ),
            PopulatedFolderMoveOutcome::Indeterminate => {
                bail!("populated-folder move left an indeterminate owned fixture state")
            }
        }
        return Ok(());
    }
    if mode == 77 {
        let destination = session
            .create_validation_folder(&format!("Cirrove Write Validation-{}", Uuid::new_v4()))
            .await?;
        let nested_name = format!("Cirrove Nested Move-{}", Uuid::new_v4());
        let (nested, etag) = session
            .create_empty_nested_move_fixture(&folder, &nested_name)
            .await?;
        let fixture = EmptyFolderMoveFixture {
            account_id: account.id.clone(),
            source_id: folder.id().to_owned(),
            source_name: folder.name().to_owned(),
            destination_id: destination.id().to_owned(),
            destination_name: destination.name().to_owned(),
            nested_id: nested.id().to_owned(),
            nested_name: nested.name().to_owned(),
            nested_etag: etag.clone(),
        };
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let provider = ICloudOwnedFixtureFolderMove::new(
            scope.clone(),
            session,
            folder,
            destination.clone(),
            nested,
            etag,
        )?;
        let request = MutationRequest {
            scope,
            intent: MutationIntent::Relocate {
                before: provider.before_node(),
                parent: destination.id().to_owned(),
                name: nested_name,
            },
        };
        let run_id = Uuid::new_v4();
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../.local-state/icloud-worker-folder-move-{run_id}"
        ));
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &directory,
            &account.id,
            8192,
        )?));
        let mut fixture_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("fixture.json"))?;
        fixture_file.write_all(&serde_json::to_vec_pretty(&fixture)?)?;
        fixture_file.write_all(b"\n")?;
        fixture_file.sync_all()?;
        fs::File::open(&directory)?.sync_all()?;
        let queued = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .enqueue_mutation(request.clone())?;
        println!("Owned empty-folder move journal recorded as {run_id} before the request.");
        let provider = Arc::new(provider.with_discarded_response());
        let worker = MutationWorker::new(journal.clone(), provider, CancellationToken::new());
        let result = worker
            .run_once()
            .await?
            .context("empty-folder move was not claimed")?;
        let saved = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .mutation(queued.id)?;
        if result.id != queued.id
            || result.state != MutationState::VerifyRequired
            || saved.prepared_item.as_deref()
                != request.intent.before().map(|node| node.id.as_str())
        {
            bail!("discarded empty-folder move response lost prepared identity");
        }
        println!(
            "Empty-folder move response deliberately discarded; run --worker-reconcile-folder-move {run_id} in a new process."
        );
        return Ok(());
    }
    if mode == 75 {
        let destination = session
            .create_validation_folder(&format!("Cirrove Write Validation-{}", Uuid::new_v4()))
            .await?;
        let nested_name = format!("Cirrove Nested Move-{}", Uuid::new_v4());
        let (nested, etag) = session
            .create_empty_nested_move_fixture(&folder, &nested_name)
            .await?;
        let fixture = EmptyFolderMoveFixture {
            account_id: account.id.clone(),
            source_id: folder.id().to_owned(),
            source_name: folder.name().to_owned(),
            destination_id: destination.id().to_owned(),
            destination_name: destination.name().to_owned(),
            nested_id: nested.id().to_owned(),
            nested_name: nested.name().to_owned(),
            nested_etag: etag.clone(),
        };
        let run_id = save_empty_folder_move_fixture(&fixture)?;
        println!("Empty-folder move identities recorded as {run_id} before the request.");
        match session
            .probe_empty_nested_folder_move(&folder, &destination, &nested, &etag)
            .await?
        {
            EmptyFolderMoveOutcome::MovedExactId => {
                println!("One conditional move retained the exact empty folder ID at destination.")
            }
            EmptyFolderMoveOutcome::RejectedIntact => {
                println!("The folder move was rejected; the exact folder remained at source.")
            }
            EmptyFolderMoveOutcome::Indeterminate => {
                bail!("empty-folder move left an indeterminate owned fixture state")
            }
        }
        return Ok(());
    }
    if matches!(mode, 58 | 59) {
        let directory = if mode == 59 {
            folder_recovery_directory
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../.local-state/icloud-worker-folder-{}",
                Uuid::new_v4()
            ))
        };
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: folder.id().into(),
                name: "Nested Cirrove folder".into(),
            },
        };
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &directory,
            &account.id,
            8192,
        )?));
        let queued = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .enqueue_mutation(request.clone())?;
        let mut provider =
            ICloudOwnedFixtureFolderCreate::new(scope, session, folder, Arc::new(DesktopVault))?;
        if mode == 59 {
            provider = provider.with_discarded_receipt();
        }
        let worker = MutationWorker::new(
            journal.clone(),
            Arc::new(provider),
            CancellationToken::new(),
        );
        let result = worker
            .run_once()
            .await?
            .context("folder creation was not claimed")?;
        if result.id != queued.id {
            bail!("worker claimed a different folder mutation");
        }
        let saved = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .mutation(queued.id)?;
        if mode == 59 {
            if result.state != MutationState::VerifyRequired
                || DesktopVault
                    .load(&format!(
                        "icloud-folder-create/{}/{}",
                        account.id, queued.id
                    ))
                    .await?
                    .is_none()
            {
                bail!("discarded folder receipt did not preserve the exact ID for verification");
            }
            println!(
                "Folder response deliberately discarded after saving the allocated ID; journal retained VerifyRequired. Operation: {}.",
                queued.id
            );
        } else if result.state == MutationState::Applied
            && saved
                .receipt
                .as_ref()
                .is_some_and(|receipt| request.accepts(receipt))
        {
            println!(
                "Shared worker created and confirmed the nested folder by exact ID. Operation: {}.",
                queued.id
            );
        } else {
            bail!("owned folder creation did not receive an exact worker receipt");
        }
        return Ok(());
    }
    if matches!(mode, 22 | 23 | 48 | 50 | 52 | 53 | 54 | 56) {
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let file_name = if mode == 50 {
            "Résumé 2026 final.txt".to_owned()
        } else if matches!(mode, 52 | 53 | 54 | 56) {
            format!(
                "Cirrove payload {}MiB.bin",
                if matches!(mode, 53 | 54 | 56) { 20 } else { 1 }
            )
        } else {
            format!("staged-by-cirrove-{}.txt", Uuid::new_v4())
        };
        let contents = if matches!(mode, 52 | 53 | 54 | 56) {
            let mut state = u64::from_le_bytes(Uuid::new_v4().as_bytes()[..8].try_into()?);
            let size = if matches!(mode, 53 | 54 | 56) {
                20 * 1024 * 1024
            } else {
                1024 * 1024
            };
            let mut bytes = Vec::with_capacity(size);
            for _ in 0..size {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                bytes.push((state >> 32) as u8);
            }
            bytes
        } else {
            format!("Cirrove worker validation {}\n", Uuid::new_v4()).into_bytes()
        };
        let directory = if mode == 23 {
            worker_recovery_directory
        } else if mode == 48 {
            reserved_create_directory
        } else if mode == 54 {
            streamed_registration_directory
        } else if mode == 56 {
            streamed_content_directory
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../.local-state/icloud-worker-create-{}",
                Uuid::new_v4()
            ))
        };
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &directory,
            &account.id,
            if matches!(mode, 53 | 54 | 56) {
                64 * 1024 * 1024
            } else if mode == 52 {
                8 * 1024 * 1024
            } else {
                8192
            },
        )?));
        let record = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .enqueue(
                scope.clone(),
                UploadIntent::Create {
                    parent: folder.id().to_owned(),
                    name: file_name,
                },
                contents.as_slice(),
            )?;
        let mut provider = ICloudOwnedFixtureUpload::new(scope, session, folder)?;
        if matches!(mode, 23 | 48 | 54) {
            provider = provider.with_discarded_registration_receipt();
        } else if mode == 56 {
            provider = provider.with_discarded_content_receipt();
        }
        let provider = Arc::new(provider);
        let worker = TransferWorker::new(
            journal.clone(),
            provider.clone(),
            Arc::new(DesktopVault),
            CancellationToken::new(),
        );
        let result = worker
            .run_once()
            .await?
            .context("worker did not claim upload")?;
        println!(
            "Worker upload state: {:?}; operation: {}; journal: {}. The owned fixture remains in iCloud Drive.",
            result.state,
            record.id,
            directory.display()
        );
        if matches!(mode, 23 | 48 | 54 | 56) {
            if result.id != record.id || result.state != UploadState::VerifyRequired {
                bail!("discarded registration receipt did not require verification");
            }
            if matches!(mode, 48 | 54 | 56) {
                let request = UploadRequest {
                    scope: record.scope.clone(),
                    intent: record.intent.clone(),
                    size: record.size,
                    sha256: record.sha256.clone(),
                };
                let checkpoint = DesktopVault
                    .load(&format!("upload/{}", record.id))
                    .await?
                    .context("reserved create checkpoint was not saved")?;
                provider
                    .reserved_document_id(&request, &checkpoint)?
                    .context("create checkpoint lacks its allocated document ID")?;
                if provider.content_receipt_saved(&request, &checkpoint)? != (mode == 54) {
                    bail!("content receipt checkpoint has the wrong phase");
                }
            }
            if mode == 56 {
                println!(
                    "The content-only uncertainty is durably retained; no registration or retry was sent."
                );
            } else {
                println!(
                    "The uncertain registration is durably retained for a fresh-process inspection; no retry was made."
                );
            }
            return Ok(());
        }
        if let Some(issue) = result.issue {
            bail!("worker upload needs review: {issue}");
        }
        if result.id != record.id || result.state != UploadState::Uploaded {
            bail!("worker upload did not reach its durable uploaded state");
        }
        let receipt = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .get(record.id)?;
        let remote = receipt
            .remote
            .context("uploaded journal receipt lacks remote item")?;
        if remote.id.is_empty()
            || remote.size != record.size
            || (mode == 50 && remote.name != "Résumé 2026 final.txt")
            || (mode == 52 && remote.name != "Cirrove payload 1MiB.bin")
            || (matches!(mode, 53 | 54 | 56) && remote.name != "Cirrove payload 20MiB.bin")
        {
            bail!("uploaded journal receipt lacks the expected remote identity");
        }
        return Ok(());
    }
    let content = format!("Cirrove write validation {}\n", Uuid::new_v4());
    let file = session
        .create_validation_file(&folder, content.as_bytes())
        .await?;
    println!(
        "Created and read back the isolated validation file byte for byte. The fixture remains in iCloud Drive."
    );
    if matches!(mode, 70 | 71 | 73) {
        let destination = session
            .create_validation_folder(&format!("Cirrove Write Validation-{}", Uuid::new_v4()))
            .await?;
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let provider = ICloudOwnedFixtureMove::new(
            scope.clone(),
            session,
            folder.clone(),
            destination.clone(),
            file,
            content.as_bytes(),
        )?;
        let before = provider.before_node();
        let request = MutationRequest {
            scope,
            intent: MutationIntent::Relocate {
                before,
                parent: destination.id().to_owned(),
                name: "created-by-cirrove.txt".into(),
            },
        };
        let run_id = Uuid::new_v4();
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../.local-state/icloud-worker-move-{run_id}"));
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &directory,
            &account.id,
            8192,
        )?));
        let folders = OwnedMoveFolders {
            source_id: folder.id().to_owned(),
            source_name: folder.name().to_owned(),
            destination_id: destination.id().to_owned(),
            destination_name: destination.name().to_owned(),
        };
        let mut fixture_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("fixture.json"))?;
        fixture_file.write_all(&serde_json::to_vec_pretty(&folders)?)?;
        fixture_file.write_all(b"\n")?;
        fixture_file.sync_all()?;
        fs::File::open(&directory)?.sync_all()?;
        let queued = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .enqueue_mutation(request.clone())?;
        println!("Owned move journal recorded as {run_id} before the one-shot request.");
        if mode == 73 {
            let pause = Arc::new(ICloudOwnedMovePause::default());
            let provider = Arc::new(provider.with_pause_before_send(pause.clone()));
            let worker = MutationWorker::new(journal.clone(), provider, CancellationToken::new());
            let task = tokio::spawn(async move { worker.run_once().await });
            if tokio::time::timeout(Duration::from_secs(240), pause.reached())
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
                bail!("owned move did not reach the controlled pre-request pause");
            }
            let occupant_bytes = format!("Cirrove move race occupant {}\n", Uuid::new_v4());
            let mut collision_session =
                ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
            let occupant_result = collision_session
                .create_validation_file(&destination, occupant_bytes.as_bytes())
                .await;
            pause.resume();
            let result = task.await??.context("owned race move not claimed")?;
            let occupant = occupant_result?;
            let saved = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?
                .mutation(queued.id)?;
            if result.id != queued.id
                || result.state == MutationState::Applied
                || saved.receipt.is_some()
                || saved.prepared_item.as_deref()
                    != request.intent.before().map(|node| node.id.as_str())
            {
                bail!("occupied destination was incorrectly acknowledged as the requested move");
            }
            let mut verify =
                ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
            let source = verify.list_folder(folder.id()).await?;
            let destination_items = verify.list_folder(destination.id()).await?;
            let moving_id = &request.intent.before().context("missing moved file")?.id;
            let occupant_at_destination = destination_items.iter().find(|entry| {
                entry.drivewsid == occupant.id() && entry.display_name() == "created-by-cirrove.txt"
            });
            let occupant_intact = occupant_at_destination.is_some()
                && verify
                    .read_small_file_in_folder(destination.id(), occupant.id())
                    .await?
                    == occupant_bytes.as_bytes();
            let moved_at_destination = destination_items
                .iter()
                .find(|entry| &entry.drivewsid == moving_id);
            let moved_with_new_name = source.is_empty()
                && destination_items.len() == 2
                && moved_at_destination
                    .is_some_and(|entry| entry.display_name() != "created-by-cirrove.txt")
                && verify
                    .read_small_file_in_folder(destination.id(), moving_id)
                    .await?
                    == content.as_bytes();
            let rejected_intact = source.len() == 1
                && destination_items.len() == 1
                && source[0].drivewsid == *moving_id
                && verify
                    .read_small_file_in_folder(folder.id(), moving_id)
                    .await?
                    == content.as_bytes();
            if !occupant_intact || !(moved_with_new_name || rejected_intact) {
                bail!("occupied move race left an indeterminate owned fixture state");
            }
            println!(
                "Occupied move race retained both exact IDs and bytes, produced no accepted receipt, and left journal state {:?}; moved with new name: {}.",
                result.state, moved_with_new_name
            );
            return Ok(());
        }
        let provider = Arc::new(if mode == 71 {
            provider.with_discarded_response()
        } else {
            provider
        });
        let worker = MutationWorker::new(journal.clone(), provider, CancellationToken::new());
        let result = worker.run_once().await?.context("owned move not claimed")?;
        let saved = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock"))?
            .mutation(queued.id)?;
        if mode == 71 {
            if result.id != queued.id
                || result.state != MutationState::VerifyRequired
                || saved.prepared_item.as_deref()
                    != request.intent.before().map(|node| node.id.as_str())
            {
                bail!("discarded move response lost its exact prepared identity");
            }
            println!(
                "Owned move response deliberately discarded; run --worker-reconcile-move {run_id} in a new process."
            );
        } else if result.id != queued.id
            || result.state != MutationState::Applied
            || result.issue.is_some()
            || !saved
                .receipt
                .as_ref()
                .is_some_and(|receipt| request.accepts(receipt))
        {
            bail!("owned move did not produce a matching applied receipt");
        } else {
            println!(
                "Owned move applied with its exact receipt. Operation: {}.",
                queued.id
            );
        }
        return Ok(());
    }
    if mode == 66 {
        let destination = session
            .create_validation_folder(&format!("Cirrove Write Validation-{}", Uuid::new_v4()))
            .await?;
        let occupant_bytes = format!("Cirrove occupied move destination {}\n", Uuid::new_v4());
        let occupant = session
            .create_validation_file(&destination, occupant_bytes.as_bytes())
            .await?;
        let fixture = OccupiedMoveFixture {
            account_id: account.id.clone(),
            source_folder_id: folder.id().to_owned(),
            source_folder_name: folder.name().to_owned(),
            destination_folder_id: destination.id().to_owned(),
            destination_folder_name: destination.name().to_owned(),
            moving_file_id: file.id().to_owned(),
            moving_file_etag: file.etag().to_owned(),
            moving_sha256: hex::encode(sha2::Sha256::digest(content.as_bytes())),
            moving_size: content.len() as u64,
            occupant_file_id: occupant.id().to_owned(),
            occupant_file_etag: occupant.etag().to_owned(),
            occupant_sha256: hex::encode(sha2::Sha256::digest(occupant_bytes.as_bytes())),
            occupant_size: occupant_bytes.len() as u64,
        };
        let run_id = save_occupied_move_fixture(&fixture)?;
        println!("Occupied-move fixture recorded as {run_id} before the one-shot request.");
        match session
            .probe_occupied_move_name(
                &folder,
                &destination,
                &file,
                content.as_bytes(),
                &occupant,
                occupant_bytes.as_bytes(),
            )
            .await?
        {
            MoveCollisionOutcome::RejectedBothIntact => println!(
                "Occupied-name move was rejected; both exact IDs and full contents remain intact."
            ),
            MoveCollisionOutcome::DuplicateNameAfterMove => {
                println!("Occupied-name move left two exact IDs with the same destination name.")
            }
            MoveCollisionOutcome::RenamedOnCollision => println!(
                "Occupied-name move kept both exact IDs and full bytes but renamed the moving item."
            ),
            MoveCollisionOutcome::DestinationDisplaced => println!(
                "Occupied-name move displaced the prior destination ID; mounted move must remain disabled."
            ),
            MoveCollisionOutcome::Indeterminate => {
                bail!("occupied-name move left an indeterminate owned fixture state")
            }
        }
        return Ok(());
    }
    if mode == 65 {
        let destination = session
            .create_validation_folder(&format!("Cirrove Write Validation-{}", Uuid::new_v4()))
            .await?;
        match session
            .probe_metadata_stale_move(&folder, &destination, &file, content.as_bytes())
            .await?
        {
            MoveProbeOutcome::StaleAccepted => println!(
                "Metadata-stale ETag move was accepted; this moveItems shape cannot protect a newer namespace revision."
            ),
            MoveProbeOutcome::StaleRejectedCurrentIntact => println!(
                "Metadata-stale ETag move was rejected; the renamed exact ID and bytes remain in the source."
            ),
            MoveProbeOutcome::Indeterminate => {
                bail!("metadata-stale move left an indeterminate owned fixture state")
            }
        }
        return Ok(());
    }
    if mode == 64 {
        let destination = session
            .create_validation_folder(&format!("Cirrove Write Validation-{}", Uuid::new_v4()))
            .await?;
        if session
            .probe_fresh_etag_move(&folder, &destination, &file, content.as_bytes())
            .await?
        {
            println!(
                "Fresh ETag move was accepted; the exact ID and full bytes appear only in the destination."
            );
        } else {
            println!(
                "Fresh ETag move was rejected; the exact ID and full bytes remain only in the source."
            );
        }
        return Ok(());
    }
    if mode == 63 {
        let destination = session
            .create_validation_folder(&format!("Cirrove Write Validation-{}", Uuid::new_v4()))
            .await?;
        let revised = format!("Cirrove move revision {}\n", Uuid::new_v4());
        if session
            .probe_same_id_update(&folder, &file, content.as_bytes(), revised.as_bytes())
            .await?
            != SameIdUpdateOutcome::Updated
        {
            bail!("owned move fixture did not establish a newer revision");
        }
        match session
            .probe_stale_etag_move(&folder, &destination, &file, revised.as_bytes())
            .await?
        {
            MoveProbeOutcome::StaleAccepted => println!(
                "Stale ETag move was accepted; this moveItems request cannot protect a newer revision."
            ),
            MoveProbeOutcome::StaleRejectedCurrentIntact => println!(
                "Stale ETag move was rejected; the newer exact ID and bytes remain in the source."
            ),
            MoveProbeOutcome::Indeterminate => {
                bail!("stale-ETag move left an indeterminate owned fixture state")
            }
        }
        return Ok(());
    }
    if mode == 34 {
        session
            .probe_owned_trash_download(&folder, &file, content.as_bytes())
            .await?;
        println!(
            "Exact owned Trash item remained byte-readable with stable ETag and restore metadata."
        );
        return Ok(());
    }
    if mode == 35 {
        let staged_bytes = format!("Cirrove conditional stage {}\n", Uuid::new_v4());
        let revised_bytes = format!("Cirrove concurrent revision {}\n", Uuid::new_v4());
        let staged = session
            .create_staged_file(&folder, &file, staged_bytes.as_bytes())
            .await?;
        session
            .probe_conditional_trash_handoff(
                &folder,
                &file,
                &staged,
                content.as_bytes(),
                revised_bytes.as_bytes(),
                staged_bytes.as_bytes(),
            )
            .await?;
        println!(
            "Stale ETag preserved both files; current ETag moved the old ID to byte-readable Trash and the staged ID reached the original name."
        );
        return Ok(());
    }
    if matches!(mode, 29 | 30 | 32 | 36 | 37 | 43 | 45 | 46) {
        let staged_bytes = format!("Cirrove worker handoff {}\n", Uuid::new_v4());
        let staged = session
            .create_staged_file(&folder, &file, staged_bytes.as_bytes())
            .await?;
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let directory = match mode {
            30 => handoff_recovery_directory,
            32 => handoff_new_recovery_directory,
            37 => conditional_trash_recovery_directory,
            43 => conditional_rename_recovery_directory,
            46 => conditional_trash_timeout_directory,
            _ => Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../.local-state/icloud-worker-handoff-{}",
                Uuid::new_v4()
            )),
        };
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &directory,
            &account.id,
            8192,
        )?));
        let record = {
            let mut guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let original = Node {
                id: file.id().into(),
                parent_id: Some(folder.id().into()),
                name: "created-by-cirrove.txt".into(),
                kind: NodeKind::File,
                size: content.len() as u64,
                modified_unix: 0,
                etag: Some(file.etag().into()),
                content_version: Some(file.etag().into()),
                target: None,
                package: false,
            };
            let working =
                guard.create_working(scope.clone(), original, false, content.as_bytes())?;
            guard.write_working(working.id, 0, staged_bytes.as_bytes())?;
            guard.truncate_working(working.id, staged_bytes.len() as u64)?;
            guard
                .seal_working(working.id)?
                .context("staged replacement was not sealed")?
        };
        if !matches!(&record.intent, UploadIntent::Replace { item, expected_etag } if item == file.id() && expected_etag == file.etag())
            || record.size != staged_bytes.len() as u64
        {
            bail!("journal did not seal the expected fixture replacement");
        }
        let plan = session
            .prepare_durable_handoff_named(
                &folder,
                &file,
                &staged,
                content.as_bytes(),
                staged_bytes.as_bytes(),
                format!("recovery-by-cirrove-{}.txt", record.id),
            )
            .await?;
        let mut provider = if matches!(mode, 36 | 37 | 43 | 45 | 46) {
            ICloudOwnedFixtureHandoff::new_conditional_trash(
                scope.clone(),
                record.id,
                plan,
                record.size,
                session,
            )?
        } else {
            ICloudOwnedFixtureHandoff::new(scope.clone(), record.id, plan, record.size, session)?
        };
        if matches!(mode, 30 | 37) {
            provider = provider.with_discarded_old_receipt();
        } else if matches!(mode, 32 | 43) {
            provider = provider.with_discarded_new_receipt();
        } else if mode == 45 {
            provider = provider.with_intervening_old_edit();
        } else if mode == 46 {
            provider = provider.with_delayed_old_receipt();
        }
        let provider = Arc::new(provider);
        let worker = TransferWorker::new(
            journal.clone(),
            provider.clone(),
            Arc::new(DesktopVault),
            CancellationToken::new(),
        );
        if mode == 45 {
            let result = worker
                .run_once()
                .await?
                .context("worker did not claim the concurrent-edit fixture")?;
            if result.id != record.id
                || result.state != UploadState::Conflict
                || !provider.stale_trash_refusal_verified()
            {
                bail!("concurrent-edit trial did not stop at a verified stale Trash refusal");
            }
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let saved = guard.get(record.id)?;
            let reserved = guard.namespace_objects()?.iter().any(|object| {
                object.unlinked
                    && !object.remote_owned
                    && object
                        .remote
                        .as_ref()
                        .is_some_and(|node| node.id == file.id())
            });
            if saved.remote.is_some() || saved.session_key.is_none() || !reserved {
                bail!("concurrent-edit conflict lost its local checkpoint or old-ID reservation");
            }
            println!(
                "Intervening edit changed the old exact ID; stale conditional Trash refused it. Both remote versions remain byte-readable; worker retained Conflict without publishing a replacement. Operation: {}.",
                record.id
            );
            return Ok(());
        }
        if matches!(mode, 30 | 32 | 37 | 46) {
            let result = worker
                .run_once()
                .await?
                .context("worker did not claim the handoff")?;
            if result.id != record.id || result.state != UploadState::VerifyRequired {
                bail!("discarded rename receipt did not retain verification state");
            }
            if mode == 46 && !provider.old_receipt_delay_started() {
                bail!("worker stopped before the accepted Trash response was held");
            }
            let guard = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?;
            let saved = guard.get(record.id)?;
            let reserved = guard.namespace_objects()?.iter().any(|object| {
                object.unlinked
                    && !object.remote_owned
                    && object
                        .remote
                        .as_ref()
                        .is_some_and(|node| node.id == file.id())
            });
            if saved.session_key.is_none() || !reserved {
                bail!("uncertain handoff lost its checkpoint or recovery reservation");
            }
            if mode == 46 {
                println!(
                    "Accepted conditional Trash response held past the worker deadline; VerifyRequired retains the exact-ID recovery reservation. Operation: {}.",
                    record.id
                );
            } else if mode == 37 {
                println!(
                    "Conditional Trash response discarded; the shared worker retained VerifyRequired and its exact-ID recovery reservation. Operation: {}.",
                    record.id
                );
            } else if mode == 32 {
                println!(
                    "New rename response discarded; shared worker retained VerifyRequired and its recovery reservation. Operation: {}.",
                    record.id
                );
            } else {
                println!(
                    "Old rename response discarded; shared worker retained VerifyRequired and its recovery reservation. Operation: {}.",
                    record.id
                );
            }
            return Ok(());
        }
        if mode == 43 {
            for _ in 0..120 {
                if let Some(result) = worker.run_once().await? {
                    if result.id != record.id {
                        bail!("worker claimed a different fixture operation");
                    }
                    if !provider.new_receipt_discard_pending() {
                        if result.state != UploadState::VerifyRequired {
                            bail!(
                                "discarded staged-rename receipt did not retain verification state"
                            );
                        }
                        let guard = journal
                            .lock()
                            .map_err(|_| anyhow::anyhow!("journal lock"))?;
                        let saved = guard.get(record.id)?;
                        let reserved = guard.namespace_objects()?.iter().any(|object| {
                            object.unlinked
                                && !object.remote_owned
                                && object
                                    .remote
                                    .as_ref()
                                    .is_some_and(|node| node.id == file.id())
                        });
                        if saved.session_key.is_none() || !reserved {
                            bail!(
                                "lost staged-rename receipt lacks its checkpoint or old-ID reservation"
                            );
                        }
                        println!(
                            "Conditional staged-rename response discarded; shared worker retained VerifyRequired and both identity reservations. Operation: {}.",
                            record.id
                        );
                        return Ok(());
                    }
                    if result.state != UploadState::VerifyRequired {
                        bail!("worker stopped before the staged-rename fault was reached");
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            }
            bail!("staged-rename fault was not reached; inspect the retained journal");
        }
        for _ in 0..5 {
            if let Some(result) = worker.run_once().await? {
                if result.id != record.id {
                    bail!("worker claimed a different fixture operation");
                }
                if result.state == UploadState::Uploaded {
                    let guard = journal
                        .lock()
                        .map_err(|_| anyhow::anyhow!("journal lock"))?;
                    let saved = guard.get(record.id)?;
                    let remote = saved
                        .remote
                        .context("handoff lacks current remote receipt")?;
                    if remote.id == file.id() || remote.id != staged.id() {
                        bail!("handoff published the wrong current identity");
                    }
                    let backup = guard
                        .namespace_by_remote(&scope, file.id())?
                        .context("handoff lacks the old recovery identity")?;
                    if !backup.unlinked
                        || !backup.remote_owned
                        || !backup.node.name.starts_with("recovery-by-cirrove-")
                        || (mode == 36
                            && backup
                                .remote
                                .as_ref()
                                .and_then(|node| node.parent_id.as_deref())
                                != Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT"))
                    {
                        bail!("handoff recovery binding is not durable");
                    }
                    println!(
                        "Shared worker published both exact iCloud identities as a durable replacement. Operation: {}.",
                        record.id
                    );
                    return Ok(());
                }
                if result.state != UploadState::VerifyRequired {
                    bail!(
                        "worker handoff needs review; journal: {}",
                        directory.display()
                    );
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        }
        bail!(
            "worker handoff remains uncertain; inspect the retained journal at {}",
            directory.display()
        );
    }
    if matches!(mode, 25..=27) {
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let mut provider = ICloudOwnedFixtureRemove::new(
            scope.clone(),
            session,
            folder,
            file,
            content.as_bytes(),
        )?;
        if mode == 27 {
            provider = provider.with_discarded_trash_receipt();
        }
        let provider = Arc::new(provider);
        let request = MutationRequest {
            scope,
            intent: MutationIntent::RemoveFile {
                before: provider.before_node(),
            },
        };
        let cancel = CancellationToken::new();
        if matches!(mode, 26 | 27) {
            let directory = if mode == 27 {
                trash_recovery_directory
            } else {
                Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                    "../../.local-state/icloud-worker-trash-{}",
                    Uuid::new_v4()
                ))
            };
            let journal = Arc::new(Mutex::new(UploadJournal::open(
                &directory,
                &account.id,
                8192,
            )?));
            let queued = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock"))?
                .enqueue_mutation(request.clone())?;
            let worker = MutationWorker::new(journal.clone(), provider, cancel);
            if mode == 27 {
                let result = worker
                    .run_once()
                    .await?
                    .context("worker did not claim Trash mutation")?;
                if result.id != queued.id || result.state != MutationState::VerifyRequired {
                    bail!("discarded Trash receipt did not require verification");
                }
                let saved = journal
                    .lock()
                    .map_err(|_| anyhow::anyhow!("journal lock"))?
                    .mutation(queued.id)?;
                if saved.prepared_item.as_deref()
                    != request.intent.before().map(|node| node.id.as_str())
                {
                    bail!("uncertain Trash mutation lost its prepared identity");
                }
                println!(
                    "Trash receipt deliberately discarded; exact prepared identity retained as VerifyRequired. Operation: {}.",
                    queued.id
                );
                return Ok(());
            }
            for _ in 0..6 {
                if let Some(result) = worker.run_once().await? {
                    println!(
                        "Worker trash state: {:?}; operation: {}.",
                        result.state, queued.id
                    );
                    if result.id != queued.id {
                        bail!("worker claimed a different validation mutation");
                    }
                    if result.state == MutationState::Applied {
                        let record = journal
                            .lock()
                            .map_err(|_| anyhow::anyhow!("journal lock"))?
                            .mutation(queued.id)?;
                        if record.prepared_item.as_deref()
                            != request.intent.before().map(|node| node.id.as_str())
                            || !matches!(record.receipt, Some(MutationReceipt::Removed { .. }))
                        {
                            bail!("worker trash receipt lacks its prepared exact identity");
                        }
                        println!(
                            "The shared mutation journal committed the recoverable Trash receipt."
                        );
                        return Ok(());
                    }
                    if result.state != MutationState::VerifyRequired {
                        bail!("worker trash mutation needs review");
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            }
            bail!(
                "worker trash mutation remains uncertain; inspect the retained journal at {}",
                directory.display()
            );
        }
        let prepared = provider
            .prepare_mutation(&request, &cancel)
            .await?
            .context("owned trash mutation lacks prepared identity")?;
        let receipt = provider
            .mutate_prepared(&request, Some(&prepared), &cancel)
            .await?;
        if !request.accepts(&receipt) || !matches!(receipt, MutationReceipt::Removed { .. }) {
            bail!("owned trash mutation lacks an exact receipt");
        }
        if !matches!(
            provider
                .reconcile_prepared_mutation(&request, Some(&prepared), &cancel)
                .await?,
            MutationReconciliation::Applied(MutationReceipt::Removed { .. })
        ) {
            bail!("owned trash mutation cannot be reconciled");
        }
        println!(
            "Owned-file mutation adapter moved the exact test item to recoverable Trash and independently reconciled the receipt."
        );
        return Ok(());
    }
    if mode == 21 {
        match session
            .probe_trash_restore_cycle(&folder, &file, content.as_bytes())
            .await?
        {
            TrashRestoreOutcome::RestoredSameIdAndBytes => println!(
                "The exact test ID entered Trash and returned to its parent with the same ID and bytes."
            ),
            TrashRestoreOutcome::RestoredNewIdAndBytes => println!(
                "The test file returned from Trash with a new item ID; its document ID and bytes match."
            ),
            TrashRestoreOutcome::RestoredNewDocumentIdAndBytes => println!(
                "The test bytes returned from Trash, but the document ID changed; the original name remains."
            ),
            TrashRestoreOutcome::RestoredDifferentNameAndBytes => println!(
                "The test bytes and document ID returned from Trash, but the displayed name changed."
            ),
            TrashRestoreOutcome::RestoredDifferentDocumentIdAndNameWithBytes => println!(
                "The test bytes returned from Trash, but both document ID and displayed name changed."
            ),
            TrashRestoreOutcome::Indeterminate(stage) => {
                bail!("iCloud Trash restore stopped uncertain at {stage}; no request was replayed")
            }
        }
        return Ok(());
    }
    if matches!(mode, 18 | 19) {
        let revised = format!("Cirrove trash revision {}\n", Uuid::new_v4());
        if session
            .probe_same_id_update(&folder, &file, content.as_bytes(), revised.as_bytes())
            .await?
            != SameIdUpdateOutcome::Updated
        {
            bail!("iCloud did not establish the newer trash-test revision");
        }
        match session
            .probe_stale_etag_trash(&folder, &file, revised.as_bytes(), mode == 19)
            .await?
        {
            TrashProbeOutcome::StaleAcceptedAbsentFromParent => println!(
                "Stale ETag was accepted for the exact test file; its ID is absent from the parent. This request shape is not conditional."
            ),
            TrashProbeOutcome::StaleRejectedCurrentIntact => println!(
                "Stale ETag was rejected; the exact test file and newer bytes remain in its parent."
            ),
            TrashProbeOutcome::StaleRejectedFreshAcceptedAbsentFromParent => println!(
                "Stale ETag was rejected, then current ETag was accepted; the exact test ID is absent from its parent."
            ),
            TrashProbeOutcome::StaleRejectedFreshRejectedCurrentIntact => println!(
                "Both stale and current ETags were rejected; the exact test file remains intact."
            ),
            TrashProbeOutcome::Indeterminate => {
                bail!("stale-ETag trash request left an indeterminate test fixture state")
            }
        }
        return Ok(());
    }
    if mode == 15 {
        let staged_bytes = format!("Cirrove registration validation {}\n", Uuid::new_v4());
        let plan = session
            .prepare_staged_registration(
                &folder,
                &file,
                content.as_bytes(),
                staged_bytes.as_bytes(),
            )
            .await?;
        let mut journal = StageJournal::create(&journal_directory, &account.id, plan)?;
        journal
            .drop_registration_receipt(&mut session, staged_bytes.as_bytes())
            .await?;
        println!(
            "Staged file registration request sent; its receipt was deliberately discarded. Start --durable-resume-registration in a new process."
        );
        return Ok(());
    }
    if (1..=3).contains(&mode) || mode == 5 {
        let replacement = format!("Cirrove revised validation {}\n", Uuid::new_v4());
        let outcome = session
            .probe_same_id_update(&folder, &file, content.as_bytes(), replacement.as_bytes())
            .await?;
        match outcome {
            SameIdUpdateOutcome::Updated => {
                println!("Same-ID update accepted; the exact revised bytes were read back.");
            }
            SameIdUpdateOutcome::RejectedUnchanged => {
                println!("Same-ID update rejected; the original ID, revision and bytes remain.");
            }
            SameIdUpdateOutcome::Indeterminate => {
                bail!(
                    "same-ID update left an indeterminate fixture state; inspect it before any retry"
                );
            }
        }
        if mode == 2 || mode == 5 {
            let mut candidate = replacement.as_bytes().to_vec();
            candidate[0] = b'X';
            let outcome = session
                .probe_stale_etag_update(
                    &folder,
                    &file,
                    replacement.as_bytes(),
                    &candidate,
                    mode == 5,
                )
                .await?;
            match outcome {
                SameIdUpdateOutcome::Updated => {
                    println!(
                        "Stale ETag was accepted despite the selected precondition; this request shape cannot protect existing edits."
                    );
                }
                SameIdUpdateOutcome::RejectedUnchanged => {
                    println!("Stale ETag was rejected; the newer revision and bytes remain.");
                }
                SameIdUpdateOutcome::Indeterminate => {
                    bail!("stale-ETag request left an indeterminate fixture state");
                }
            }
        }
        if mode == 3 {
            let outcome = session
                .probe_conditional_rename(&folder, &file, replacement.as_bytes())
                .await?;
            match outcome {
                RenameProbeOutcome::StaleAccepted => {
                    println!(
                        "Stale ETag rename was accepted; namespace ETags do not protect this request shape."
                    );
                }
                RenameProbeOutcome::StaleRejectedFreshAccepted => {
                    println!(
                        "Stale ETag rename was rejected; fresh ETag rename kept the same ID and exact bytes."
                    );
                }
                RenameProbeOutcome::StaleRejectedFreshRejected => {
                    println!(
                        "Both stale and fresh ETag renames were rejected; the original remained intact."
                    );
                }
                RenameProbeOutcome::Indeterminate => {
                    bail!("conditional rename left an indeterminate fixture state");
                }
            }
        }
    }
    if mode == 4 {
        let outcome = session
            .probe_metadata_rename_revision(&folder, &file, content.as_bytes())
            .await?;
        match outcome {
            RenameProbeOutcome::StaleAccepted => {
                println!(
                    "Stale metadata ETag rename was accepted; this request shape is not a namespace CAS."
                );
            }
            RenameProbeOutcome::StaleRejectedFreshAccepted => {
                println!(
                    "Stale metadata ETag rename was rejected; current ETag rename succeeded with stable ID and bytes."
                );
            }
            RenameProbeOutcome::StaleRejectedFreshRejected => {
                println!(
                    "Stale and current ETag renames were both rejected; original file remains."
                );
            }
            RenameProbeOutcome::Indeterminate => {
                bail!("metadata rename trial left an indeterminate fixture state");
            }
        }
    }
    if mode == 6 {
        let staged_bytes = format!("Cirrove staged validation {}\n", Uuid::new_v4());
        let staged = session
            .create_staged_file(&folder, &file, staged_bytes.as_bytes())
            .await?;
        println!("Created and read back a second, separately identified staging file.");
        let outcome = session
            .probe_occupied_name_rename(
                &folder,
                &file,
                &staged,
                content.as_bytes(),
                staged_bytes.as_bytes(),
            )
            .await?;
        match outcome {
            OccupiedNameOutcome::RejectedBothIntact => {
                println!(
                    "Occupied-name rename was rejected; both exact IDs and byte sequences remain."
                );
            }
            OccupiedNameOutcome::AcceptedDuplicateName => {
                println!("Occupied-name rename was accepted; two distinct IDs now share the name.");
            }
            OccupiedNameOutcome::ConflictRenamedBothIntact => {
                println!(
                    "Apple gave the staged file a conflict suffix; both exact IDs and bytes remain."
                );
            }
            OccupiedNameOutcome::OriginalNoLongerListed => {
                println!(
                    "Occupied-name rename removed the original ID from the folder; no fallback may use it."
                );
            }
            OccupiedNameOutcome::Indeterminate => {
                bail!("occupied-name rename left an indeterminate fixture state");
            }
        }
    }
    if mode == 8 {
        let staged_bytes = format!("Cirrove staged handoff {}\n", Uuid::new_v4());
        let staged = session
            .create_staged_file(&folder, &file, staged_bytes.as_bytes())
            .await?;
        println!("Created and read back a second, separately identified staging file.");
        let outcome = session
            .probe_staged_handoff(
                &folder,
                &file,
                &staged,
                content.as_bytes(),
                staged_bytes.as_bytes(),
            )
            .await?;
        match outcome {
            HandoffOutcome::BothPreservedAtTargetAndRecovery => {
                println!(
                    "Staged handoff reached the original name; both exact IDs and bytes remain, with the old file under recovery."
                );
            }
            HandoffOutcome::RecoveryMovedStagingUnchanged => {
                println!(
                    "Old file moved to recovery; staged file remained under its unique name. Both IDs and bytes remain."
                );
            }
            HandoffOutcome::Indeterminate => {
                bail!(
                    "staged handoff left an indeterminate fixture state; no request will be retried"
                );
            }
        }
    }
    if matches!(mode, 9 | 11 | 13) {
        let staged_bytes = format!("Cirrove durable handoff {}\n", Uuid::new_v4());
        let staged = session
            .create_staged_file(&folder, &file, staged_bytes.as_bytes())
            .await?;
        let plan = session
            .prepare_durable_handoff(
                &folder,
                &file,
                &staged,
                content.as_bytes(),
                staged_bytes.as_bytes(),
            )
            .await?;
        let mut journal = Journal::create(&journal_directory, &account.id, plan)?;
        if mode == 9 {
            journal.stop_after_recovery(&mut session).await?;
            println!(
                "Durable checkpoint saved after moving the old validation item to recovery. Start --durable-resume in a new process."
            );
        } else if mode == 11 {
            journal.drop_old_rename_receipt(&mut session).await?;
            println!(
                "Old rename request sent; its receipt was deliberately discarded. Start --durable-resume-lost-old in a new process."
            );
        } else {
            journal.drop_new_rename_receipt(&mut session).await?;
            println!(
                "New rename request sent; its receipt was deliberately discarded. Start --durable-resume-lost-new in a new process."
            );
        }
    }
    Ok(())
}
