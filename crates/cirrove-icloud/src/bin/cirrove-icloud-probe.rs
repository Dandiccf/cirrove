//! Temporary read-only protocol probe. No credentials or session are persisted.
use anyhow::{Result, bail};
use cirrove_icloud::{ICloudReadSession, SignInStep};
use secrecy::SecretString;
use std::{io::Write, path::Path};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let apple_id = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: cirrove-icloud-probe APPLE_ID [FOLDER_ID]"))?;
    let operation = args.next();
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
    if args.next().is_some() {
        bail!(
            "usage: cirrove-icloud-probe APPLE_ID [FOLDER_ID | --read FILE_ID OUTPUT | --read-in FOLDER_ID FILE_ID OUTPUT | --range-in FOLDER_ID FILE_ID OFFSET LENGTH OUTPUT]"
        );
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
