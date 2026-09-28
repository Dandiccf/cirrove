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
    HandoffOutcome, ICloudOwnedFixtureHandoff, ICloudOwnedFixtureRemove, ICloudOwnedFixtureUpload,
    ICloudReadSession, OccupiedNameOutcome, RenameProbeOutcome, SameIdUpdateOutcome,
    SealedSessionVault, TrashProbeOutcome, TrashRestoreOutcome,
};
use cirrove_service::accounts::Settings;
use cirrove_service::journal::{MutationState, UploadIntent, UploadJournal, UploadState};
use cirrove_service::mutations::MutationWorker;
use cirrove_service::transfers::TransferWorker;
use icloud_handoff_journal::{Journal, StageJournal};
use sha2::Digest;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let mode = match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
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
        _ => bail!(
            "usage: cirrove-icloud-write-probe [--same-id | --stale-etag | --rename-conflict | --metadata-rename | --http-if-match | --occupied-name | --staged-handoff | --durable-stop-after-recovery | --durable-resume | --durable-drop-old-receipt | --durable-resume-lost-old | --durable-drop-new-receipt | --durable-resume-lost-new | --durable-drop-registration-receipt | --durable-resume-registration | --durable-handoff-registered | --stale-etag-trash | --stale-then-fresh-trash | --inspect-trash | --trash-restore-cycle | --worker-create | --worker-discard-registration-receipt | --worker-resume-registration | --owned-file-trash-adapter | --worker-owned-trash | --worker-discard-trash-receipt | --worker-resume-trash | --worker-owned-handoff | --worker-discard-old-handoff-receipt | --worker-resume-handoff | --worker-discard-new-handoff-receipt | --worker-reconcile-new-handoff | --owned-trash-download | --conditional-trash-handoff | --worker-conditional-trash-handoff | --worker-discard-conditional-trash-receipt | --worker-resume-conditional-trash | --inspect-conditional-trash-journal | --worker-resume-inspected-conditional-trash | --inspect-conditional-trash-receipt | --publish-conditional-trash-receipt | --worker-discard-conditional-rename-receipt | --worker-reconcile-conditional-rename | --worker-intervening-edit-before-trash | --worker-timeout-after-conditional-trash | --worker-resume-timed-out-conditional-trash | --worker-reserved-create-lost-receipt | --worker-reconcile-reserved-create | --worker-ordinary-name-create | --worker-ordinary-name-collision]"
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
    if matches!(mode, 24 | 49) {
        let recovery_directory = if mode == 49 {
            reserved_create_directory.clone()
        } else {
            worker_recovery_directory.clone()
        };
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &recovery_directory,
            &account.id,
            8192,
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
        let reserved_id = if mode == 49 {
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
    println!(
        "Created and listed the isolated iCloud validation folder. Testing a small file upload."
    );
    if matches!(mode, 22 | 23 | 48 | 50) {
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let file_name = if mode == 50 {
            "Résumé 2026 final.txt".to_owned()
        } else {
            format!("staged-by-cirrove-{}.txt", Uuid::new_v4())
        };
        let contents = format!("Cirrove worker validation {}\n", Uuid::new_v4());
        let directory = if mode == 23 {
            worker_recovery_directory
        } else if mode == 48 {
            reserved_create_directory
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../.local-state/icloud-worker-create-{}",
                Uuid::new_v4()
            ))
        };
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &directory,
            &account.id,
            8192,
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
                contents.as_bytes(),
            )?;
        let mut provider = ICloudOwnedFixtureUpload::new(scope, session, folder)?;
        if matches!(mode, 23 | 48) {
            provider = provider.with_discarded_registration_receipt();
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
        if matches!(mode, 23 | 48) {
            if result.id != record.id || result.state != UploadState::VerifyRequired {
                bail!("discarded registration receipt did not require verification");
            }
            if mode == 48 {
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
            }
            println!(
                "The uncertain registration is durably retained for a fresh-process inspection; no retry was made."
            );
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
