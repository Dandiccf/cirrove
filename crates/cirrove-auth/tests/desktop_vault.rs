use cirrove_auth::{CredentialVault, DesktopVault};
use secrecy::{ExposeSecret, SecretString};

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
