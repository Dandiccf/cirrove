//! Explicit, one-shot live mutation experiment against Cirrove-owned fixtures.
//! This does not expose iCloud as a writable filesystem.
#[path = "cirrove-icloud-write-probe/icloud_handoff_journal.rs"]
mod icloud_handoff_journal;

use anyhow::{Context, Result, bail};
use cirrove_auth::{AppRegistration, CredentialVault, DesktopVault};
use cirrove_core::mutation::{
    MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation, MutationRequest,
};
use cirrove_core::{CancellationToken, Scope};
use cirrove_icloud::{
    HandoffOutcome, ICloudOwnedFixtureRemove, ICloudOwnedFixtureUpload, ICloudReadSession,
    OccupiedNameOutcome, RenameProbeOutcome, SameIdUpdateOutcome, SealedSessionVault,
    TrashProbeOutcome, TrashRestoreOutcome,
};
use cirrove_service::accounts::Settings;
use cirrove_service::journal::{UploadIntent, UploadJournal, UploadState};
use cirrove_service::transfers::TransferWorker;
use icloud_handoff_journal::{Journal, StageJournal};
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
        _ => bail!(
            "usage: cirrove-icloud-write-probe [--same-id | --stale-etag | --rename-conflict | --metadata-rename | --http-if-match | --occupied-name | --inspect-occupied | --staged-handoff | --durable-stop-after-recovery | --durable-resume | --durable-drop-old-receipt | --durable-resume-lost-old | --durable-drop-new-receipt | --durable-resume-lost-new | --durable-drop-registration-receipt | --durable-resume-registration | --durable-handoff-registered | --stale-etag-trash | --stale-then-fresh-trash | --inspect-trash | --trash-restore-cycle | --worker-create | --worker-discard-registration-receipt | --worker-resume-registration | --owned-file-trash-adapter]"
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
    if mode == 24 {
        let journal = Arc::new(Mutex::new(UploadJournal::open(
            &worker_recovery_directory,
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
        let provider = Arc::new(
            ICloudOwnedFixtureUpload::new(record.scope.clone(), session, folder)?
                .reconciliation_only(),
        );
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
        {
            bail!("reconciled upload has a mismatched remote receipt");
        }
        println!(
            "Restarted worker reconciled the exact remote item and reached Uploaded without permitting another upload request. Operation: {}.",
            record.id
        );
        return Ok(());
    }
    if mode == 23 && worker_recovery_directory.exists() {
        bail!("lost-receipt validation journal already exists");
    }
    let name = format!("Cirrove Write Validation-{}", Uuid::new_v4());
    let folder = session.create_validation_folder(&name).await?;
    println!(
        "Created and listed the isolated iCloud validation folder. Testing a small file upload."
    );
    if matches!(mode, 22 | 23) {
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let file_name = format!("staged-by-cirrove-{}.txt", Uuid::new_v4());
        let contents = format!("Cirrove worker validation {}\n", Uuid::new_v4());
        let directory = if mode == 23 {
            worker_recovery_directory
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
        if mode == 23 {
            provider = provider.with_discarded_registration_receipt();
        }
        let provider = Arc::new(provider);
        let worker = TransferWorker::new(
            journal.clone(),
            provider,
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
        if mode == 23 {
            if result.id != record.id || result.state != UploadState::VerifyRequired {
                bail!("discarded registration receipt did not require verification");
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
        if remote.id.is_empty() || remote.size != record.size {
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
    if mode == 25 {
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let provider = ICloudOwnedFixtureRemove::new(
            scope.clone(),
            session,
            folder,
            file,
            content.as_bytes(),
        )?;
        let request = MutationRequest {
            scope,
            intent: MutationIntent::RemoveFile {
                before: provider.before_node(),
            },
        };
        let cancel = CancellationToken::new();
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
