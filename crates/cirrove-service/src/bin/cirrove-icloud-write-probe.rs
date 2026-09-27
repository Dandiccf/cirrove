//! Explicit, one-shot live mutation experiment against Cirrove-owned fixtures.
//! This does not expose iCloud as a writable filesystem.
use anyhow::{Context, Result, bail};
use cirrove_auth::{AppRegistration, CredentialVault};
use cirrove_icloud::{ICloudReadSession, SealedSessionVault};
use cirrove_service::accounts::Settings;
use std::path::Path;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().len() != 1 {
        bail!("this validation probe takes no arguments");
    }
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
    session
        .list_root()
        .await
        .context("saved iCloud session is not usable")?;
    let name = format!("Cirrove Write Validation-{}", Uuid::new_v4());
    let folder = session.create_validation_folder(&name).await?;
    println!(
        "Created and listed the isolated iCloud validation folder. Testing a small file upload."
    );
    let content = format!("Cirrove write validation {}\n", Uuid::new_v4());
    let _file_id = session
        .create_validation_file(&folder, content.as_bytes())
        .await?;
    println!(
        "Created and read back the isolated validation file byte for byte. The fixture remains in iCloud Drive."
    );
    Ok(())
}
