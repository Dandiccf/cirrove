//! The manager must honor the account budget named in save-refusal messages.
use super::*;
use crate::journal::JournalError;
use cirrove_core::upload::UploadIntent;
use std::io::Read;

fn account(mount_path: PathBuf, budget: u64) -> Account {
    Account {
        id: uuid::Uuid::new_v4().to_string(),
        label: "write-budget-fixture".into(),
        registration: cirrove_auth::AppRegistration::Microsoft {
            client_id: uuid::Uuid::new_v4().to_string(),
            authority: "common".into(),
        },
        identity: cirrove_auth::Identity {
            tenant_id: String::new(),
            subject: "synthetic".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: "fixture".into(),
            display_name: "Fixture".into(),
        },
        credential_id: uuid::Uuid::new_v4().to_string(),
        access: cirrove_auth::AccessMode::ReadWrite,
        drive: cirrove_core::CollectionInfo {
            id: "fixture".into(),
            name: "Fixture".into(),
            drive_type: "business".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: "root".into(),
        mount_path,
        enabled: false,
        poll_seconds: 3600,
        cache_bytes: budget,
    }
}
async fn open(account: &Account, state: &Path) -> Result<(Arc<Engine>, WriteContext)> {
    // Construction needs no network; loopback prevents reaching a real service.
    let read = Arc::new(cirrove_onedrive::OneDrive::synthetic_loopback(
        account.id.clone(),
        "http://127.0.0.1:9/",
    )?);
    let engine = Engine::new(account.clone(), read, state.to_owned()).await?;
    let context = WriteContext::open(&engine, state).await?;
    Ok((engine, context))
}
#[tokio::test]
async fn account_write_budget_allows_large_reservations_and_preserves_pending_bytes_when_reduced()
-> Result<()> {
    const MIB: u64 = 1024 * 1024;
    let temp = tempfile::tempdir()?;
    let mut account = account(temp.path().join("mount"), 96 * MIB);
    let (engine, context) = open(&account, temp.path()).await?;
    let scope = engine.scope("fixture");
    let pending = {
        let mut journal = context.journal.lock().expect("journal");
        let reservation = journal
            .reserve_working(80 * MIB)
            .expect("account budget exceeds former fixed limit");
        assert!(matches!(
            journal.reserve_working(17 * MIB),
            Err(JournalError::Quota)
        ));
        drop(reservation);
        journal.enqueue(
            scope.clone(),
            UploadIntent::Create {
                parent: "root".into(),
                name: "Retained.bin".into(),
            },
            std::io::repeat(0x35).take(9 * MIB),
        )?
    };
    drop(context);
    drop(engine);
    // Lowering a budget must refuse growth, never evict unsent bytes or prevent recovery.
    account.cache_bytes = 8 * MIB;
    let (engine, context) = open(&account, temp.path()).await?;
    {
        let mut journal = context.journal.lock().expect("journal");
        assert!(matches!(
            journal.reserve_working(1),
            Err(JournalError::Quota)
        ));
        assert_eq!(journal.get(pending.id)?.size, 9 * MIB);
        let mut payload = journal.payload(pending.id)?;
        assert_eq!(payload.metadata()?.len(), 9 * MIB);
        let mut first = [0; 4096];
        payload.read_exact(&mut first)?;
        assert!(first.iter().all(|byte| *byte == 0x35));
    }
    drop(context);
    drop(engine);
    account.cache_bytes = 96 * MIB;
    let (_engine, context) = open(&account, temp.path()).await?;
    let mut journal = context.journal.lock().expect("journal");
    let _reservation = journal.reserve_working(80 * MIB)?;
    assert!(matches!(
        journal.reserve_working(8 * MIB),
        Err(JournalError::Quota)
    ));
    assert_eq!(journal.get(pending.id)?.sha256, pending.sha256);
    Ok(())
}
