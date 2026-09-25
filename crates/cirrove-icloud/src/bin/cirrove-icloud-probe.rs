//! Temporary read-only protocol probe. Optional sessions live only in Cirrove's keyring.
use anyhow::{Result, bail};
use cirrove_auth::{CredentialVault, DesktopVault};
use cirrove_core::{CancellationToken, Checkpoint, MetadataProvider, Scope};
use cirrove_icloud::{DriveEntry, ICloudDrive, ICloudReadSession, SignInStep, probe_session_key};
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let apple_id = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: cirrove-icloud-probe APPLE_ID [FOLDER_ID]"))?;
    let mut operation = args.next();
    let snapshot_limit = if operation.as_deref() == Some("--snapshot-pages") {
        let value = args
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing snapshot page limit"))?
            .parse::<usize>()?;
        if !(1..=1000).contains(&value) {
            bail!("snapshot page limit must be between 1 and 1000");
        }
        Some(value)
    } else {
        None
    };
    let read_target = match operation.as_deref() {
        Some("--read") | Some("--read-in") | Some("--range-in") => {
            let folder = if operation.as_deref() != Some("--read") {
                Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("missing folder ID"))?,
                )
            } else {
                None
            };
            let item = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing file ID"))?;
            let range = if operation.as_deref() == Some("--range-in") {
                let offset = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing range offset"))?
                    .parse::<u64>()?;
                let length = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing range length"))?
                    .parse::<u32>()?;
                Some((offset, length))
            } else {
                None
            };
            let output = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing output path"))?;
            Some((folder, item, output, range))
        }
        _ => None,
    };
    let revision_target = if operation.as_deref() == Some("--revision-check-in") {
        Some((
            args.next()
                .ok_or_else(|| anyhow::anyhow!("missing folder ID"))?,
            args.next()
                .ok_or_else(|| anyhow::anyhow!("missing file ID"))?,
        ))
    } else {
        None
    };
    let trailing = args.next();
    if args.next().is_some()
        || trailing
            .as_deref()
            .is_some_and(|value| !matches!(value, "--save-session" | "--resume-session"))
        || (trailing.is_some()
            && matches!(
                operation.as_deref(),
                Some("--save-session" | "--resume-session")
            ))
    {
        bail!(
            "usage: cirrove-icloud-probe APPLE_ID [FOLDER_ID | --read FILE_ID OUTPUT | --read-in FOLDER_ID FILE_ID OUTPUT | --range-in FOLDER_ID FILE_ID OFFSET LENGTH OUTPUT | --revision-check-in FOLDER_ID FILE_ID | --snapshot-pages LIMIT | --save-session | --resume-session] [--save-session | --resume-session]"
        );
    }
    let save_session = operation.as_deref() == Some("--save-session")
        || trailing.as_deref() == Some("--save-session");
    let resume_session = operation.as_deref() == Some("--resume-session")
        || trailing.as_deref() == Some("--resume-session");
    let vault = DesktopVault;
    let mut client = if resume_session {
        let key = probe_session_key(&apple_id)?;
        let saved = vault
            .load(&key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no saved iCloud probe session; sign in again"))?;
        ICloudReadSession::from_session_snapshot(&saved, &apple_id).map_err(|_| {
            anyhow::anyhow!("saved iCloud probe session is unusable; sign in and save it again")
        })?
    } else {
        if save_session {
            DesktopVault::reachable().await?;
        }
        let password =
            SecretString::new(rpassword::prompt_password("Apple account password: ")?.into());
        let mut client = ICloudReadSession::new()?;
        match client.sign_in(&apple_id, &password).await? {
            SignInStep::Ready => {}
            SignInStep::NeedsTrustedDeviceCode => {
                client.request_trusted_device_code().await?;
                let code =
                    SecretString::new(rpassword::prompt_password("Trusted-device code: ")?.into());
                client.verify_trusted_device_code(&code).await?;
            }
        }
        client
    };
    if save_session {
        let snapshot = client.session_snapshot()?;
        vault.save(&probe_session_key(&apple_id)?, snapshot).await?;
        println!("Saved the Cirrove iCloud probe session in the desktop keyring.");
    }
    if matches!(
        operation.as_deref(),
        Some("--save-session" | "--resume-session")
    ) {
        operation = None;
    }
    if let Some(limit) = snapshot_limit {
        let scope = Scope {
            account: probe_session_key(&apple_id)?,
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let snapshot = client.session_snapshot()?;
        let provider = ICloudDrive::from_session_snapshot(scope.clone(), &apple_id, &snapshot)?;
        let mut cursor = None;
        let mut items = 0usize;
        for page_index in 1..=limit {
            let page = provider
                .changes(&scope, cursor.as_ref(), &CancellationToken::new())
                .await?;
            items += page.changes.len();
            match page.checkpoint {
                Checkpoint::Complete(_) => {
                    println!("Snapshot complete: {page_index} pages, {items} metadata nodes.");
                    return Ok(());
                }
                Checkpoint::Continue(next) => {
                    cursor = Some(next);
                    if page_index % 16 == 0 && page_index < limit {
                        eprintln!(
                            "Snapshot scan: {page_index} pages, {items} metadata nodes; incomplete."
                        );
                    }
                }
            }
        }
        println!("Snapshot page limit reached: {limit} pages, {items} metadata nodes; incomplete.");
        return Ok(());
    }
    if let Some((folder_id, file_id)) = revision_target {
        check_content_revision(&mut client, &folder_id, &file_id).await?;
        return Ok(());
    }
    if (save_session || resume_session) && operation.is_none() && read_target.is_none() {
        let count = client.list_root().await?.len();
        println!("iCloud probe session active; {count} root items.");
        return Ok(());
    }
    if let Some((folder_id, file_id, output, range)) = read_target {
        let bytes = if let (Some(folder_id), Some((offset, length))) = (folder_id.as_deref(), range)
        {
            client
                .read_range_in_folder(folder_id, &file_id, offset, length)
                .await?
        } else if let Some(folder_id) = folder_id {
            client
                .read_small_file_in_folder(&folder_id, &file_id)
                .await?
        } else {
            client.read_small_file(&file_id).await?
        };
        let output = Path::new(&output);
        let parent = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist_noclobber(output)?;
        println!("Saved {} verified-size bytes.", bytes.len());
        return Ok(());
    }
    let items = if let Some(folder_id) = operation {
        client.list_folder(&folder_id).await?
    } else {
        client.list_root().await?
    };
    for item in items {
        let kind = if item.is_folder() { "folder" } else { "file" };
        println!(
            "{kind}\t{}\t{}\t{}",
            item.size,
            item.drivewsid,
            item.display_name()
        );
    }
    Ok(())
}

fn unique_file(items: Vec<DriveEntry>, file_id: &str) -> Result<DriveEntry> {
    let mut matches = items.into_iter().filter(|item| item.drivewsid == file_id);
    let file = matches
        .next()
        .ok_or_else(|| anyhow::anyhow!("test file is absent from its parent folder"))?;
    if matches.next().is_some() || file.is_folder() || file.etag.is_empty() {
        bail!("test file does not have one usable file identity and ETag");
    }
    Ok(file)
}

async fn check_content_revision(
    client: &mut ICloudReadSession,
    folder_id: &str,
    file_id: &str,
) -> Result<()> {
    let (before, baseline) = capture_file(client, folder_id, file_id).await?;
    println!(
        "Baseline captured. Change the contents of this small test file in iCloud, wait for sync, then press Enter here."
    );
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let mut etag_changed_without_content = false;
    for attempt in 0..6 {
        let (after, content) = capture_file(client, folder_id, file_id).await?;
        if content != baseline {
            if after.etag == before.etag {
                bail!(
                    "Content changed, but the iCloud ETag did not; mounted caching remains unsafe"
                );
            }
            println!(
                "Content and ETag both changed in this one live test; the mounted cache still needs broader validation."
            );
            return Ok(());
        }
        etag_changed_without_content |= after.etag != before.etag;
        if attempt < 5 {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }
    if etag_changed_without_content {
        bail!("ETag changed, but file contents did not change during the test window");
    }
    bail!("No changed file contents were observed during the test window")
}

async fn capture_file(
    client: &mut ICloudReadSession,
    folder_id: &str,
    file_id: &str,
) -> Result<(DriveEntry, [u8; 32])> {
    let before = unique_file(client.list_folder(folder_id).await?, file_id)?;
    let bytes = client.read_small_file_in_folder(folder_id, file_id).await?;
    let after = unique_file(client.list_folder(folder_id).await?, file_id)?;
    if before.etag != after.etag || before.size != after.size || bytes.len() as u64 != after.size {
        bail!("test file changed while its baseline was being captured");
    }
    Ok((after, Sha256::digest(bytes).into()))
}
