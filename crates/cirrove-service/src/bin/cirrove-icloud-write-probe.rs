//! Explicit, one-shot live mutation experiment against Cirrove-owned fixtures.
//! This does not expose iCloud as a writable filesystem.
#[path = "cirrove-icloud-write-probe/icloud_handoff_journal.rs"]
mod icloud_handoff_journal;

use anyhow::{Context, Result, bail};
use cirrove_auth::{AppRegistration, CredentialVault};
use cirrove_icloud::{
    HandoffOutcome, ICloudReadSession, OccupiedNameOutcome, RenameProbeOutcome,
    SameIdUpdateOutcome, SealedSessionVault,
};
use cirrove_service::accounts::Settings;
use icloud_handoff_journal::Journal;
use std::path::Path;
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
        _ => bail!(
            "usage: cirrove-icloud-write-probe [--same-id | --stale-etag | --rename-conflict | --metadata-rename | --http-if-match | --occupied-name | --inspect-occupied | --staged-handoff | --durable-stop-after-recovery | --durable-resume | --durable-drop-old-receipt | --durable-resume-lost-old | --durable-drop-new-receipt | --durable-resume-lost-new]"
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
    let name = format!("Cirrove Write Validation-{}", Uuid::new_v4());
    let folder = session.create_validation_folder(&name).await?;
    println!(
        "Created and listed the isolated iCloud validation folder. Testing a small file upload."
    );
    let content = format!("Cirrove write validation {}\n", Uuid::new_v4());
    let file = session
        .create_validation_file(&folder, content.as_bytes())
        .await?;
    println!(
        "Created and read back the isolated validation file byte for byte. The fixture remains in iCloud Drive."
    );
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
