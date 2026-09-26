use cirrove_auth::{CredentialVault, DesktopVault};
use secrecy::{ExposeSecret, SecretString};

fn synthetic_retention_value() -> anyhow::Result<String> {
    Ok(
        match std::env::var("CIRROVE_SYNTHETIC_KEYRING_PATTERN")
            .as_deref()
            .unwrap_or("repeated")
        {
            "empty" => String::new(),
            "repeated" => format!("synthetic-{}", "x".repeat(20_000)),
            "icloud_size_entropy" | "icloud_size_entropy_v2" => {
                // The observed iCloud snapshot was 11,645 bytes. Use a matching,
                // poorly compressible fixture without containing session material.
                const ALPHABET: &[u8; 64] =
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
                let mut state = if std::env::var("CIRROVE_SYNTHETIC_KEYRING_PATTERN").as_deref()
                    == Ok("icloud_size_entropy_v2")
                {
                    0xd1b5_4a32_d192_ed03_u64
                } else {
                    0x9e37_79b9_7f4a_7c15_u64
                };
                let mut value = String::with_capacity(11_645);
                for _ in 0..11_645 {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    value.push(char::from(ALPHABET[(state & 63) as usize]));
                }
                value
            }
            _ => anyhow::bail!("unknown synthetic keyring pattern"),
        },
    )
}

/// Reproduce the size and process boundary of an iCloud session checkpoint
/// without handling a real credential. The child gets only a synthetic key.
#[tokio::test]
#[ignore = "requires an unlocked desktop Secret Service; uses only a temporary synthetic entry"]
async fn desktop_keyring_large_snapshot_survives_another_process() -> anyhow::Result<()> {
    let key = format!("icloud-retention-test-{}", uuid::Uuid::new_v4());
    let value = synthetic_retention_value()?;
    let result = async {
        DesktopVault
            .save(&key, SecretString::from(value.clone()))
            .await?;
        let child = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                "desktop_keyring_large_snapshot_child",
            ])
            .env("CIRROVE_SYNTHETIC_KEYRING_TEST_KEY", &key)
            .status()?;
        anyhow::ensure!(child.success(), "independent keyring read failed");
        let restored = DesktopVault.load(&key).await?;
        anyhow::ensure!(
            restored.is_some_and(|secret| secret.expose_secret() == value),
            "synthetic entry changed after the independent read"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let cleanup = DesktopVault.remove(&key).await;
    result?;
    cleanup?;
    Ok(())
}

#[tokio::test]
#[ignore = "runs only as the child of the large synthetic keyring test"]
async fn desktop_keyring_large_snapshot_child() -> anyhow::Result<()> {
    let Ok(key) = std::env::var("CIRROVE_SYNTHETIC_KEYRING_TEST_KEY") else {
        return Ok(());
    };
    let expected = synthetic_retention_value()?;
    let restored = DesktopVault.load(&key).await?;
    anyhow::ensure!(
        restored.is_some_and(|secret| secret.expose_secret() == expected),
        "synthetic entry was absent or changed in a separate process"
    );
    Ok(())
}

/// A manual, three-process retention probe for the desktop Secret Service.
/// `save` intentionally leaves its unique synthetic item in the keyring;
/// always run `clear` for that same key after the delayed `check`.
#[tokio::test]
#[ignore = "manual delayed keyring retention probe with a unique synthetic key"]
async fn desktop_keyring_delayed_checkpoint_phase() -> anyhow::Result<()> {
    let Ok(key) = std::env::var("CIRROVE_SYNTHETIC_KEYRING_TEST_KEY") else {
        return Ok(());
    };
    anyhow::ensure!(
        key.starts_with("icloud-retention-test-"),
        "refusing to use a non-synthetic key"
    );
    let value = synthetic_retention_value()?;
    match std::env::var("CIRROVE_KEYRING_RETENTION_PHASE")?.as_str() {
        "save" => {
            DesktopVault.save(&key, SecretString::from(value)).await?;
            anyhow::ensure!(
                DesktopVault.load(&key).await?.is_some(),
                "synthetic entry missing after save"
            );
        }
        "check" => {
            let restored = DesktopVault.load(&key).await?;
            anyhow::ensure!(
                restored.is_some_and(|secret| secret.expose_secret() == value),
                "synthetic entry did not retain its value"
            );
        }
        "clear" => {
            DesktopVault.remove(&key).await?;
            anyhow::ensure!(
                DesktopVault.load(&key).await?.is_none(),
                "synthetic entry remained after cleanup"
            );
        }
        _ => anyhow::bail!("unknown synthetic retention phase"),
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires an unlocked desktop Secret Service; uses only a temporary synthetic entry"]
async fn desktop_keyring_checkpoint_updates_survive_new_sessions() -> anyhow::Result<()> {
    let key = format!("upload/roundtrip-test-{}", uuid::Uuid::new_v4());
    let result = async {
        for index in 0..64_u64 {
            let value = serde_json::json!({
                "version": 1,
                "url": format!("https://example.invalid/{}", "synthetic".repeat(512)),
                "offset": index * 5 * 1024 * 1024,
                "length": 29,
                "commit_target": ["synthetic-parent", "Kärnten & Grüße #1.bin"],
            })
            .to_string();
            DesktopVault
                .save(&key, SecretString::from(value.clone()))
                .await?;
            let restored = DesktopVault.load(&key).await?;
            anyhow::ensure!(
                restored.is_some_and(|s| s.expose_secret() == value),
                "synthetic checkpoint did not survive an independent read"
            );
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    // Clean up this uniquely named fixture even if a write/read check failed.
    let cleanup = DesktopVault.remove(&key).await;
    result?;
    cleanup?;
    anyhow::ensure!(
        DesktopVault.load(&key).await?.is_none(),
        "synthetic credential remained after removal"
    );
    Ok(())
}

/// A keyring that can take a grant is asked about before a sign-in, not after.
///
/// Twice a person has done a full Microsoft sign-in and been told afterwards
/// that there was nowhere to keep it: on the Fedora VM the keyring was locked,
/// on a clean Arch machine there was none at all. Both throw away minutes of
/// somebody's attention that cannot be handed back. This is the question that
/// now runs first.
///
/// Ignored like its neighbour: it needs a real desktop Secret Service. On a
/// machine that has one and has it unlocked, it must say so.
#[tokio::test]
#[ignore = "requires an unlocked desktop Secret Service; reads nothing and writes nothing"]
async fn an_unlocked_keyring_reports_itself_as_somewhere_to_keep_a_grant() {
    cirrove_auth::DesktopVault::reachable()
        .await
        .expect("this machine has an unlocked Secret Service");
}
