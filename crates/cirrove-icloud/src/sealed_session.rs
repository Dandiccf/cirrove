//! iCloud's large web session is sealed on disk with a short key held by the
//! desktop Secret Service. This is scoped to Cirrove's own private account dir.
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use cirrove_auth::{CredentialVault, DesktopVault};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use secrecy::{ExposeSecret, SecretString};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const KEY_PREFIX: &str = "icloud-seal-v1:";
const MAGIC: &[u8] = b"ICLDS1";
const MAX_SEALED_BYTES: u64 = 128 * 1024 + 64;

/// An iCloud-only vault. Older direct keyring snapshots remain readable until
/// the next foreground re-sign-in; no background migration exposes a grant.
pub struct SealedSessionVault {
    account_dir: PathBuf,
    account_id: String,
    file_name: &'static str,
    temp_prefix: &'static str,
    aad_domain: &'static str,
}

impl SealedSessionVault {
    pub fn new(state: &Path, account_id: &str) -> Result<Self> {
        Uuid::parse_str(account_id).context("invalid iCloud account identifier")?;
        if !state.is_absolute() {
            bail!("iCloud state directory must be absolute");
        }
        Ok(Self {
            account_dir: state.join("accounts").join(account_id),
            account_id: account_id.into(),
            file_name: "icloud-session.sealed",
            temp_prefix: ".icloud-session",
            aad_domain: "icloud-session",
        })
    }

    fn path(&self) -> PathBuf {
        self.account_dir.join(self.file_name)
    }

    fn aad(&self, credential_id: &str) -> Vec<u8> {
        format!(
            "cirrove:{}:v1:{}:{credential_id}",
            self.aad_domain, self.account_id
        )
        .into_bytes()
    }

    fn key(value: &SecretString) -> Result<Option<[u8; 32]>> {
        let Some(encoded) = value.expose_secret().strip_prefix(KEY_PREFIX) else {
            return Ok(None);
        };
        let decoded = STANDARD_NO_PAD
            .decode(encoded)
            .context("invalid iCloud sealing key")?;
        Ok(Some(decoded.try_into().map_err(|_| {
            anyhow::anyhow!("invalid iCloud sealing key length")
        })?))
    }

    fn sealed_key(key: &[u8; 32]) -> SecretString {
        SecretString::from(format!("{KEY_PREFIX}{}", STANDARD_NO_PAD.encode(key)))
    }

    fn cipher(key: &[u8; 32]) -> Result<LessSafeKey> {
        Ok(LessSafeKey::new(
            UnboundKey::new(&aead::AES_256_GCM, key)
                .map_err(|_| anyhow::anyhow!("invalid iCloud sealing key"))?,
        ))
    }

    fn private_account_dir(&self) -> Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.account_dir)?;
        let meta = fs::symlink_metadata(&self.account_dir)?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != fs::metadata("/proc/self")?.uid()
        {
            bail!("iCloud account directory is not private");
        }
        Ok(())
    }

    fn read_sealed(&self, credential_id: &str, key: &[u8; 32]) -> Result<SecretString> {
        let path = self.path();
        let meta = fs::symlink_metadata(&path).context("iCloud session file missing")?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() > MAX_SEALED_BYTES
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != fs::metadata("/proc/self")?.uid()
        {
            bail!("iCloud session file is not a private regular file");
        }
        let bytes = fs::read(path).context("cannot read sealed iCloud session")?;
        if bytes.len() < MAGIC.len() + aead::NONCE_LEN + aead::AES_256_GCM.tag_len()
            || !bytes.starts_with(MAGIC)
        {
            bail!("invalid sealed iCloud session");
        }
        let nonce_start = MAGIC.len();
        let nonce_end = nonce_start + aead::NONCE_LEN;
        let nonce = Nonce::try_assume_unique_for_key(&bytes[nonce_start..nonce_end])
            .map_err(|_| anyhow::anyhow!("invalid iCloud session nonce"))?;
        let mut ciphertext = bytes[nonce_end..].to_vec();
        let plaintext = Self::cipher(key)?
            .open_in_place(nonce, Aad::from(self.aad(credential_id)), &mut ciphertext)
            .map_err(|_| anyhow::anyhow!("sealed iCloud session failed authentication"))?;
        let value =
            String::from_utf8(plaintext.to_vec()).context("invalid iCloud session encoding")?;
        Ok(SecretString::from(value))
    }

    fn write_sealed(
        &self,
        credential_id: &str,
        key: &[u8; 32],
        value: &SecretString,
    ) -> Result<()> {
        self.private_account_dir()?;
        let mut nonce_bytes = [0u8; aead::NONCE_LEN];
        getrandom::fill(&mut nonce_bytes)
            .map_err(|_| anyhow::anyhow!("cannot create iCloud session nonce"))?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let mut ciphertext = value.expose_secret().as_bytes().to_vec();
        Self::cipher(key)?
            .seal_in_place_append_tag(nonce, Aad::from(self.aad(credential_id)), &mut ciphertext)
            .map_err(|_| anyhow::anyhow!("cannot seal iCloud session"))?;
        if ciphertext.len() as u64 + MAGIC.len() as u64 + aead::NONCE_LEN as u64 > MAX_SEALED_BYTES
        {
            bail!("iCloud session exceeds sealed storage limit");
        }
        let temp = self
            .account_dir
            .join(format!("{}-{}.tmp", self.temp_prefix, Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(MAGIC)?;
            file.write_all(&nonce_bytes)?;
            file.write_all(&ciphertext)?;
            file.sync_all()?;
            fs::rename(&temp, self.path())?;
            File::open(&self.account_dir)?.sync_all()?;
            Ok(())
        })();
        if temp.exists() {
            let _ = fs::remove_file(temp);
        }
        result
    }

    async fn load_with(
        &self,
        key_id: &str,
        key_vault: &dyn CredentialVault,
    ) -> Result<Option<SecretString>> {
        let Some(saved) = key_vault.load(key_id).await? else {
            return Ok(None);
        };
        match Self::key(&saved)? {
            Some(key) => self.read_sealed(key_id, &key).map(Some),
            None => Ok(Some(saved)),
        }
    }

    async fn save_with(
        &self,
        key_id: &str,
        value: SecretString,
        key_vault: &dyn CredentialVault,
    ) -> Result<()> {
        let existing = key_vault.load(key_id).await?;
        let existing_key = existing.as_ref().map(Self::key).transpose()?.flatten();
        let key = match existing_key {
            Some(key) => key,
            None => {
                let mut key = [0u8; 32];
                getrandom::fill(&mut key)
                    .map_err(|_| anyhow::anyhow!("cannot create iCloud sealing key"))?;
                // Keep a legacy direct-keyring session usable if the new
                // short-key save fails. An unreferenced sealed file is inert.
                self.write_sealed(key_id, &key, &value)?;
                key_vault.save(key_id, Self::sealed_key(&key)).await?;
                key
            }
        };
        if existing_key.is_some() {
            self.write_sealed(key_id, &key, &value)?;
        }
        let restored = self
            .load_with(key_id, key_vault)
            .await?
            .context("iCloud session was not saved")?;
        if restored.expose_secret() != value.expose_secret() {
            bail!("iCloud session did not survive local readback");
        }
        Ok(())
    }
}

/// Each upload operation has its own authenticated checkpoint file. Secret
/// Service holds only the short encryption key, since large iCloud checkpoint
/// values have been observed returning empty after a keyring update.
pub struct SealedUploadCheckpointVault {
    account_dir: PathBuf,
    account_id: String,
}

impl SealedUploadCheckpointVault {
    pub fn new(state: &Path, account_id: &str) -> Result<Self> {
        Uuid::parse_str(account_id).context("invalid iCloud account identifier")?;
        if !state.is_absolute() {
            bail!("iCloud state directory must be absolute");
        }
        Ok(Self {
            account_dir: state.join("accounts").join(account_id),
            account_id: account_id.into(),
        })
    }

    fn operation(&self, key: &str) -> Result<SealedSessionVault> {
        let operation = key
            .strip_prefix("upload/")
            .context("invalid iCloud upload checkpoint key")?;
        Uuid::parse_str(operation).context("invalid iCloud upload operation")?;
        Ok(SealedSessionVault {
            account_dir: self.account_dir.join("upload-checkpoints").join(operation),
            account_id: self.account_id.clone(),
            file_name: "checkpoint.sealed",
            temp_prefix: ".checkpoint",
            aad_domain: "icloud-upload-checkpoint",
        })
    }
}

#[async_trait]
impl CredentialVault for SealedUploadCheckpointVault {
    async fn load(&self, key: &str) -> Result<Option<SecretString>> {
        self.operation(key)?.load_with(key, &DesktopVault).await
    }

    async fn save(&self, key: &str, value: SecretString) -> Result<()> {
        self.operation(key)?
            .save_with(key, value, &DesktopVault)
            .await
    }

    async fn remove(&self, key: &str) -> Result<()> {
        self.operation(key)?.remove(key).await
    }
}

/// One-shot package Trash checkpoints use a distinct keyring purpose as well
/// as a distinct file and authenticated domain. Import and Trash intentionally
/// share the operation UUID, but must never share a sealing-key identity.
#[cfg(feature = "write-probe")]
pub(crate) struct SealedPackageTrashCheckpointVault {
    account_dir: PathBuf,
    account_id: String,
}

#[cfg(feature = "write-probe")]
impl SealedPackageTrashCheckpointVault {
    pub fn new(state: &Path, account_id: &str) -> Result<Self> {
        Uuid::parse_str(account_id).context("invalid iCloud account identifier")?;
        if !state.is_absolute() {
            bail!("iCloud state directory must be absolute");
        }
        Ok(Self {
            account_dir: state.join("accounts").join(account_id),
            account_id: account_id.into(),
        })
    }

    pub(crate) fn key(account: &str, operation: Uuid) -> String {
        format!("icloud-package-trash/{account}/{operation}")
    }

    fn operation(&self, key: &str) -> Result<SealedSessionVault> {
        let prefix = format!("icloud-package-trash/{}/", self.account_id);
        let operation = key
            .strip_prefix(&prefix)
            .context("invalid iCloud package Trash checkpoint key")?;
        Uuid::parse_str(operation).context("invalid iCloud package Trash operation")?;
        Ok(SealedSessionVault {
            account_dir: self
                .account_dir
                .join("package-trash-checkpoints")
                .join(operation),
            account_id: self.account_id.clone(),
            file_name: "checkpoint.sealed",
            temp_prefix: ".checkpoint",
            aad_domain: "icloud-package-trash-checkpoint",
        })
    }
}

#[cfg(feature = "write-probe")]
#[async_trait]
impl CredentialVault for SealedPackageTrashCheckpointVault {
    async fn load(&self, key: &str) -> Result<Option<SecretString>> {
        self.operation(key)?.load_with(key, &DesktopVault).await
    }

    async fn save(&self, key: &str, value: SecretString) -> Result<()> {
        self.operation(key)?
            .save_with(key, value, &DesktopVault)
            .await
    }

    async fn remove(&self, key: &str) -> Result<()> {
        self.operation(key)?.remove(key).await
    }
}

/// Independent one-shot restore history; never reuses or removes the import or
/// Trash credential, even though the owned source operation is the same.
#[cfg(feature = "write-probe")]
pub(crate) struct SealedPackageRestoreCheckpointVault {
    account_dir: PathBuf,
    account_id: String,
}

#[cfg(feature = "write-probe")]
impl SealedPackageRestoreCheckpointVault {
    pub fn new(state: &Path, account_id: &str) -> Result<Self> {
        Uuid::parse_str(account_id).context("invalid iCloud account identifier")?;
        if !state.is_absolute() {
            bail!("iCloud state directory must be absolute");
        }
        Ok(Self {
            account_dir: state.join("accounts").join(account_id),
            account_id: account_id.into(),
        })
    }

    pub(crate) fn key(account: &str, operation: Uuid) -> String {
        format!("icloud-package-restore/{account}/{operation}")
    }

    fn operation(&self, key: &str) -> Result<SealedSessionVault> {
        let prefix = format!("icloud-package-restore/{}/", self.account_id);
        let operation = key
            .strip_prefix(&prefix)
            .context("invalid iCloud package restore checkpoint key")?;
        Uuid::parse_str(operation).context("invalid iCloud package restore operation")?;
        Ok(SealedSessionVault {
            account_dir: self
                .account_dir
                .join("package-restore-checkpoints")
                .join(operation),
            account_id: self.account_id.clone(),
            file_name: "checkpoint.sealed",
            temp_prefix: ".checkpoint",
            aad_domain: "icloud-package-restore-checkpoint",
        })
    }
}

#[cfg(feature = "write-probe")]
#[async_trait]
impl CredentialVault for SealedPackageRestoreCheckpointVault {
    async fn load(&self, key: &str) -> Result<Option<SecretString>> {
        self.operation(key)?.load_with(key, &DesktopVault).await
    }

    async fn save(&self, key: &str, value: SecretString) -> Result<()> {
        self.operation(key)?
            .save_with(key, value, &DesktopVault)
            .await
    }

    async fn remove(&self, key: &str) -> Result<()> {
        self.operation(key)?.remove(key).await
    }
}

/// Production native-document Trash has a separate key, path and authenticated
/// domain from import and from the feature-only owned Trash experiment.
pub struct SealedNativeTrashCheckpointVault {
    account_dir: PathBuf,
    account_id: String,
}

impl SealedNativeTrashCheckpointVault {
    pub fn new(state: &Path, account_id: &str) -> Result<Self> {
        Uuid::parse_str(account_id).context("invalid iCloud account identifier")?;
        if !state.is_absolute() {
            bail!("iCloud state directory must be absolute");
        }
        Ok(Self {
            account_dir: state.join("accounts").join(account_id),
            account_id: account_id.into(),
        })
    }

    pub(crate) fn key(account: &str, operation: Uuid) -> String {
        format!("icloud-native-trash/{account}/{operation}")
    }

    fn operation(&self, key: &str) -> Result<SealedSessionVault> {
        let prefix = format!("icloud-native-trash/{}/", self.account_id);
        let operation = key
            .strip_prefix(&prefix)
            .context("invalid iCloud native Trash checkpoint key")?;
        Uuid::parse_str(operation).context("invalid iCloud native Trash operation")?;
        Ok(SealedSessionVault {
            account_dir: self
                .account_dir
                .join("native-trash-checkpoints")
                .join(operation),
            account_id: self.account_id.clone(),
            file_name: "checkpoint.sealed",
            temp_prefix: ".checkpoint",
            aad_domain: "icloud-native-trash-checkpoint",
        })
    }
}

#[async_trait]
impl CredentialVault for SealedNativeTrashCheckpointVault {
    async fn load(&self, key: &str) -> Result<Option<SecretString>> {
        self.operation(key)?.load_with(key, &DesktopVault).await
    }

    async fn save(&self, key: &str, value: SecretString) -> Result<()> {
        self.operation(key)?
            .save_with(key, value, &DesktopVault)
            .await
    }

    async fn remove(&self, key: &str) -> Result<()> {
        self.operation(key)?.remove(key).await
    }
}

/// Durable folder-create receipts, isolated from upload checkpoints. The key
/// carries both the account and journal operation; neither may select a path
/// outside this account's private state.
pub struct SealedFolderCheckpointVault {
    account_dir: PathBuf,
    account_id: String,
}

impl SealedFolderCheckpointVault {
    pub fn new(state: &Path, account_id: &str) -> Result<Self> {
        Uuid::parse_str(account_id).context("invalid iCloud account identifier")?;
        if !state.is_absolute() {
            bail!("iCloud state directory must be absolute");
        }
        Ok(Self {
            account_dir: state.join("accounts").join(account_id),
            account_id: account_id.into(),
        })
    }

    fn operation(&self, key: &str) -> Result<SealedSessionVault> {
        let prefix = format!("icloud-folder-create/{}/", self.account_id);
        let plan_prefix = format!("icloud-folder-plan/{}/", self.account_id);
        let (operation, directory, domain) = if let Some(operation) = key.strip_prefix(&prefix) {
            (operation, "folder-checkpoints", "icloud-folder-checkpoint")
        } else if let Some(operation) = key.strip_prefix(&plan_prefix) {
            (operation, "folder-plans", "icloud-folder-plan")
        } else {
            bail!("invalid iCloud folder checkpoint key");
        };
        Uuid::parse_str(operation).context("invalid iCloud folder operation")?;
        Ok(SealedSessionVault {
            account_dir: self.account_dir.join(directory).join(operation),
            account_id: self.account_id.clone(),
            file_name: "checkpoint.sealed",
            temp_prefix: ".checkpoint",
            aad_domain: domain,
        })
    }
}

#[async_trait]
impl CredentialVault for SealedFolderCheckpointVault {
    async fn load(&self, key: &str) -> Result<Option<SecretString>> {
        self.operation(key)?.load_with(key, &DesktopVault).await
    }

    async fn save(&self, key: &str, value: SecretString) -> Result<()> {
        self.operation(key)?
            .save_with(key, value, &DesktopVault)
            .await
    }

    async fn remove(&self, key: &str) -> Result<()> {
        self.operation(key)?.remove(key).await
    }
}

#[async_trait]
impl CredentialVault for SealedSessionVault {
    async fn load(&self, credential_id: &str) -> Result<Option<SecretString>> {
        self.load_with(credential_id, &DesktopVault).await
    }

    async fn save(&self, credential_id: &str, value: SecretString) -> Result<()> {
        self.save_with(credential_id, value, &DesktopVault).await
    }

    async fn remove(&self, credential_id: &str) -> Result<()> {
        DesktopVault.remove(credential_id).await?;
        match fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("cannot remove sealed iCloud session"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn native_trash_checkpoint_refuses_foreign_key_and_authenticates_purpose() {
        let temp = tempfile::tempdir().expect("fixture");
        let account = Uuid::new_v4().to_string();
        let operation = Uuid::new_v4();
        let vault = SealedNativeTrashCheckpointVault::new(temp.path(), &account).expect("fixture");
        let key = SealedNativeTrashCheckpointVault::key(&account, operation);
        assert!(
            vault
                .operation(&format!("icloud-package-trash/{account}/{operation}"))
                .is_err()
        );
        assert!(
            vault
                .operation(&SealedNativeTrashCheckpointVault::key(
                    &Uuid::new_v4().to_string(),
                    operation
                ))
                .is_err()
        );
        let keyring = LargeValueLost::default();
        let native = vault.operation(&key).expect("fixture");
        native
            .save_with(&key, SecretString::from("native armed"), &keyring)
            .await
            .expect("fixture");
        let mut reopened = vault.operation(&key).expect("fixture");
        assert_eq!(
            reopened
                .load_with(&key, &keyring)
                .await
                .expect("fixture")
                .expect("fixture")
                .expose_secret(),
            "native armed"
        );
        reopened.aad_domain = "icloud-package-trash-checkpoint";
        assert!(reopened.load_with(&key, &keyring).await.is_err());
        reopened.aad_domain = "icloud-native-trash-checkpoint";
        fs::remove_file(reopened.path()).expect("fixture");
        assert!(reopened.load_with(&key, &keyring).await.is_err());
    }

    /// Models the observed failure's outcome (an empty large item), not its
    /// unexplained mechanism. The live gate still needs a delayed restart.
    #[derive(Default)]
    struct LargeValueLost(Mutex<Option<String>>);

    #[async_trait]
    impl CredentialVault for LargeValueLost {
        async fn load(&self, _key: &str) -> Result<Option<SecretString>> {
            Ok(self
                .0
                .lock()
                .expect("synthetic fixture")
                .as_ref()
                .map(|s| SecretString::from(s.clone())))
        }
        async fn save(&self, _key: &str, value: SecretString) -> Result<()> {
            let stored = if value.expose_secret().len() > 80 {
                String::new()
            } else {
                value.expose_secret().to_string()
            };
            *self.0.lock().expect("synthetic fixture") = Some(stored);
            Ok(())
        }
        async fn remove(&self, _key: &str) -> Result<()> {
            *self.0.lock().expect("synthetic fixture") = None;
            Ok(())
        }
    }

    #[tokio::test]
    async fn large_session_survives_when_large_keyring_items_do_not() {
        let temp = tempfile::tempdir().expect("synthetic fixture");
        let account = Uuid::new_v4().to_string();
        let id = Uuid::new_v4().to_string();
        let store = SealedSessionVault::new(temp.path(), &account).expect("synthetic fixture");
        let keyring = LargeValueLost::default();
        let session = SecretString::from("test-session".repeat(1000));
        keyring
            .save(&id, SecretString::from(session.expose_secret().to_string()))
            .await
            .expect("synthetic fixture");
        assert_eq!(
            keyring
                .load(&id)
                .await
                .expect("synthetic fixture")
                .expect("synthetic fixture")
                .expose_secret(),
            ""
        );

        store
            .save_with(
                &id,
                SecretString::from(session.expose_secret().to_string()),
                &keyring,
            )
            .await
            .expect("synthetic fixture");
        assert!(
            keyring
                .load(&id)
                .await
                .expect("synthetic fixture")
                .expect("synthetic fixture")
                .expose_secret()
                .starts_with(KEY_PREFIX)
        );
        assert_eq!(
            store
                .load_with(&id, &keyring)
                .await
                .expect("synthetic fixture")
                .expect("synthetic fixture")
                .expose_secret(),
            session.expose_secret()
        );
        assert_eq!(
            fs::metadata(store.path())
                .expect("synthetic fixture")
                .permissions()
                .mode()
                & 0o077,
            0
        );

        let foreign = SealedSessionVault::new(temp.path(), &Uuid::new_v4().to_string())
            .expect("synthetic fixture");
        foreign.private_account_dir().expect("synthetic fixture");
        fs::copy(store.path(), foreign.path()).expect("synthetic fixture");
        assert!(foreign.load_with(&id, &keyring).await.is_err());

        let mut sealed = fs::read(store.path()).expect("synthetic fixture");
        *sealed.last_mut().expect("synthetic fixture") ^= 1;
        fs::write(store.path(), sealed).expect("synthetic fixture");
        assert!(store.load_with(&id, &keyring).await.is_err());
        keyring.remove(&id).await.expect("synthetic fixture");
        assert!(
            store
                .load_with(&id, &keyring)
                .await
                .expect("synthetic fixture")
                .is_none()
        );
    }

    #[tokio::test]
    async fn legacy_keyring_snapshot_remains_readable() {
        let temp = tempfile::tempdir().expect("synthetic fixture");
        let store = SealedSessionVault::new(temp.path(), &Uuid::new_v4().to_string())
            .expect("synthetic fixture");
        let keyring = LargeValueLost::default();
        let id = Uuid::new_v4().to_string();
        keyring
            .save(&id, SecretString::from("legacy".to_string()))
            .await
            .expect("synthetic fixture");
        assert_eq!(
            store
                .load_with(&id, &keyring)
                .await
                .expect("synthetic fixture")
                .expect("synthetic fixture")
                .expose_secret(),
            "legacy"
        );
    }

    #[tokio::test]
    async fn folder_checkpoint_preserves_legacy_receipt_until_next_save() {
        let temp = tempfile::tempdir().expect("synthetic fixture");
        let account = Uuid::new_v4().to_string();
        let id = format!("icloud-folder-create/{account}/{}", Uuid::new_v4());
        let keyring = LargeValueLost::default();
        let store = SealedFolderCheckpointVault::new(temp.path(), &account)
            .expect("synthetic fixture")
            .operation(&id)
            .expect("synthetic fixture");
        keyring
            .save(&id, SecretString::from("legacy-receipt".to_string()))
            .await
            .expect("synthetic fixture");
        let receipt = store
            .load_with(&id, &keyring)
            .await
            .expect("synthetic fixture")
            .expect("synthetic fixture");
        assert_eq!(receipt.expose_secret(), "legacy-receipt");
        assert!(!store.path().exists());
        store
            .save_with(&id, receipt, &keyring)
            .await
            .expect("synthetic fixture");
        assert!(store.path().exists());
        assert_eq!(
            store
                .load_with(&id, &keyring)
                .await
                .expect("synthetic fixture")
                .expect("synthetic fixture")
                .expose_secret(),
            "legacy-receipt"
        );
    }

    #[tokio::test]
    async fn folder_checkpoint_is_sealed_and_bound_to_account_operation_and_namespace() {
        let temp = tempfile::tempdir().expect("synthetic fixture");
        let account = Uuid::new_v4().to_string();
        let operation = Uuid::new_v4();
        let id = format!("icloud-folder-create/{account}/{operation}");
        let vault =
            SealedFolderCheckpointVault::new(temp.path(), &account).expect("synthetic fixture");
        let keyring = LargeValueLost::default();
        let value = "synthetic-folder-receipt".repeat(1000);
        let store = vault.operation(&id).expect("synthetic fixture");
        store
            .save_with(&id, SecretString::from(value.clone()), &keyring)
            .await
            .expect("synthetic fixture");
        let reopened = SealedFolderCheckpointVault::new(temp.path(), &account)
            .expect("synthetic fixture")
            .operation(&id)
            .expect("synthetic fixture");
        assert_eq!(
            reopened
                .load_with(&id, &keyring)
                .await
                .expect("synthetic fixture")
                .expect("synthetic fixture")
                .expose_secret(),
            &value
        );
        let key = keyring
            .load(&id)
            .await
            .expect("synthetic fixture")
            .expect("synthetic fixture");
        assert!(key.expose_secret().starts_with(KEY_PREFIX));
        assert!(
            !fs::read(store.path())
                .expect("synthetic fixture")
                .windows(24)
                .any(|bytes| bytes == &value.as_bytes()[..24])
        );

        let other_account = Uuid::new_v4().to_string();
        let other_operation = Uuid::new_v4();
        let destinations = [
            (
                format!("icloud-folder-plan/{account}/{operation}"),
                vault
                    .operation(&format!("icloud-folder-plan/{account}/{operation}"))
                    .expect("synthetic fixture"),
            ),
            (
                format!("icloud-folder-create/{other_account}/{operation}"),
                SealedFolderCheckpointVault::new(temp.path(), &other_account)
                    .expect("synthetic fixture")
                    .operation(&format!("icloud-folder-create/{other_account}/{operation}"))
                    .expect("synthetic fixture"),
            ),
            (
                format!("icloud-folder-create/{account}/{other_operation}"),
                vault
                    .operation(&format!("icloud-folder-create/{account}/{other_operation}"))
                    .expect("synthetic fixture"),
            ),
            (
                format!("upload/{operation}"),
                SealedUploadCheckpointVault::new(temp.path(), &account)
                    .expect("synthetic fixture")
                    .operation(&format!("upload/{operation}"))
                    .expect("synthetic fixture"),
            ),
        ];
        for (foreign_id, foreign) in destinations {
            foreign.private_account_dir().expect("synthetic fixture");
            fs::copy(store.path(), foreign.path()).expect("synthetic fixture");
            keyring
                .save(
                    &foreign_id,
                    SecretString::from(key.expose_secret().to_string()),
                )
                .await
                .expect("synthetic fixture");
            assert!(foreign.load_with(&foreign_id, &keyring).await.is_err());
        }
        for invalid in [
            format!("upload/{operation}"),
            format!("icloud-folder-create/{other_account}/{operation}"),
            format!("icloud-folder-create/{account}/../{operation}"),
            format!("icloud-folder-create/{account}/{operation}/extra"),
            format!("icloud-folder-create/{account}/"),
        ] {
            assert!(vault.operation(&invalid).is_err());
        }
    }

    #[tokio::test]
    async fn upload_checkpoint_survives_large_keyring_value_loss_and_reopen() {
        let temp = tempfile::tempdir().expect("synthetic fixture");
        let account = Uuid::new_v4().to_string();
        let operation = format!("upload/{}", Uuid::new_v4());
        let keyring = LargeValueLost::default();
        let checkpoint = SecretString::from("synthetic-checkpoint".repeat(1000));
        keyring
            .save(
                &operation,
                SecretString::from(checkpoint.expose_secret().to_string()),
            )
            .await
            .expect("synthetic fixture");
        assert_eq!(
            keyring
                .load(&operation)
                .await
                .expect("synthetic fixture")
                .expect("synthetic fixture")
                .expose_secret(),
            ""
        );

        let vault =
            SealedUploadCheckpointVault::new(temp.path(), &account).expect("synthetic fixture");
        let first = vault.operation(&operation).expect("synthetic fixture");
        first
            .save_with(
                &operation,
                SecretString::from(checkpoint.expose_secret().to_string()),
                &keyring,
            )
            .await
            .expect("synthetic fixture");
        assert!(
            keyring
                .load(&operation)
                .await
                .expect("synthetic fixture")
                .expect("synthetic fixture")
                .expose_secret()
                .starts_with(KEY_PREFIX)
        );
        let reopened = SealedUploadCheckpointVault::new(temp.path(), &account)
            .expect("synthetic fixture")
            .operation(&operation)
            .expect("synthetic fixture");
        assert_eq!(
            reopened
                .load_with(&operation, &keyring)
                .await
                .expect("synthetic fixture")
                .expect("synthetic fixture")
                .expose_secret(),
            checkpoint.expose_secret()
        );
        let foreign = SealedUploadCheckpointVault::new(temp.path(), &Uuid::new_v4().to_string())
            .expect("synthetic fixture")
            .operation(&operation)
            .expect("synthetic fixture");
        foreign.private_account_dir().expect("synthetic fixture");
        fs::copy(reopened.path(), foreign.path()).expect("synthetic fixture");
        assert!(foreign.load_with(&operation, &keyring).await.is_err());
    }
}

#[cfg(all(test, feature = "write-probe"))]
mod package_trash_tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Mutex};

    #[derive(Default)]
    struct Keyring(Mutex<BTreeMap<String, String>>);

    #[async_trait]
    impl CredentialVault for Keyring {
        async fn load(&self, key: &str) -> Result<Option<SecretString>> {
            Ok(self
                .0
                .lock()
                .expect("fixture")
                .get(key)
                .cloned()
                .map(SecretString::from))
        }
        async fn save(&self, key: &str, value: SecretString) -> Result<()> {
            self.0
                .lock()
                .expect("fixture")
                .insert(key.into(), value.expose_secret().into());
            Ok(())
        }
        async fn remove(&self, key: &str) -> Result<()> {
            self.0.lock().expect("fixture").remove(key);
            Ok(())
        }
    }

    #[tokio::test]
    async fn import_and_trash_same_operation_have_independent_sealed_checkpoints() {
        let temp = tempfile::tempdir().expect("fixture");
        let account = Uuid::new_v4().to_string();
        let operation = Uuid::new_v4();
        let keyring = Keyring::default();
        let upload_key = format!("upload/{operation}");
        let trash_key = SealedPackageTrashCheckpointVault::key(&account, operation);
        let import_state = temp.path().join("import");
        let probe_state = temp.path().join("probe");
        let import = SealedUploadCheckpointVault::new(&import_state, &account)
            .expect("fixture")
            .operation(&upload_key)
            .expect("fixture");
        import
            .save_with(&upload_key, SecretString::from("import receipt"), &keyring)
            .await
            .expect("fixture");
        let import_bytes = fs::read(import.path()).expect("fixture");
        let import_key = keyring
            .load(&upload_key)
            .await
            .expect("fixture")
            .expect("fixture");
        // This is the failed live path: a global upload key exists, but the
        // separate probe state has no matching encrypted upload file.
        let old_probe = SealedUploadCheckpointVault::new(&probe_state, &account)
            .expect("fixture")
            .operation(&upload_key)
            .expect("fixture");
        assert!(old_probe.load_with(&upload_key, &keyring).await.is_err());
        let vault =
            SealedPackageTrashCheckpointVault::new(&probe_state, &account).expect("fixture");
        let trash = vault.operation(&trash_key).expect("fixture");
        assert!(
            trash
                .load_with(&trash_key, &keyring)
                .await
                .expect("fixture")
                .is_none()
        );
        trash
            .save_with(&trash_key, SecretString::from("trash armed"), &keyring)
            .await
            .expect("fixture");
        let reopened = SealedPackageTrashCheckpointVault::new(&probe_state, &account)
            .expect("fixture")
            .operation(&trash_key)
            .expect("fixture");
        assert_eq!(
            reopened
                .load_with(&trash_key, &keyring)
                .await
                .expect("fixture")
                .expect("fixture")
                .expose_secret(),
            "trash armed"
        );
        assert_eq!(
            import
                .load_with(&upload_key, &keyring)
                .await
                .expect("fixture")
                .expect("fixture")
                .expose_secret(),
            "import receipt"
        );
        assert_eq!(fs::read(import.path()).expect("fixture"), import_bytes);
        assert_eq!(
            keyring
                .load(&upload_key)
                .await
                .expect("fixture")
                .expect("fixture")
                .expose_secret(),
            import_key.expose_secret()
        );
        assert_eq!(keyring.0.lock().expect("fixture").len(), 2);
        // Missing encrypted files are never interpreted as an unused operation.
        fs::remove_file(reopened.path()).expect("fixture");
        assert!(reopened.load_with(&trash_key, &keyring).await.is_err());
        assert!(vault.operation(&upload_key).is_err());
        let foreign_key =
            SealedPackageTrashCheckpointVault::key(&Uuid::new_v4().to_string(), operation);
        assert!(vault.operation(&foreign_key).is_err());
    }

    #[tokio::test]
    async fn trash_checkpoint_authenticates_purpose_even_with_identical_key_material() {
        let temp = tempfile::tempdir().expect("fixture");
        let account = Uuid::new_v4().to_string();
        let operation = Uuid::new_v4();
        let keyring = Keyring::default();
        let upload_key = format!("upload/{operation}");
        let trash_key = SealedPackageTrashCheckpointVault::key(&account, operation);
        let import = SealedUploadCheckpointVault::new(temp.path(), &account)
            .expect("fixture")
            .operation(&upload_key)
            .expect("fixture");
        let mut trash = SealedPackageTrashCheckpointVault::new(temp.path(), &account)
            .expect("fixture")
            .operation(&trash_key)
            .expect("fixture");
        import
            .save_with(&upload_key, SecretString::from("import receipt"), &keyring)
            .await
            .expect("fixture");
        // Force equal encryption keys so authentication refusal cannot be
        // explained merely by independent random keys.
        keyring
            .save(
                &trash_key,
                keyring
                    .load(&upload_key)
                    .await
                    .expect("fixture")
                    .expect("fixture"),
            )
            .await
            .expect("fixture");
        trash.private_account_dir().expect("fixture");
        fs::copy(import.path(), trash.path()).expect("fixture");
        assert!(trash.load_with(&trash_key, &keyring).await.is_err());
        // Holding the key ID constant independently checks the purpose AAD.
        assert!(trash.load_with(&upload_key, &keyring).await.is_err());
        trash
            .save_with(&trash_key, SecretString::from("trash receipt"), &keyring)
            .await
            .expect("fixture");
        trash.aad_domain = "icloud-upload-checkpoint";
        assert!(trash.load_with(&trash_key, &keyring).await.is_err());
    }
    #[tokio::test]
    async fn restore_checkpoint_is_separate_from_import_and_trash_history() {
        let temp = tempfile::tempdir().expect("fixture");
        let account = Uuid::new_v4().to_string();
        let operation = Uuid::new_v4();
        let keyring = Keyring::default();
        let upload_key = format!("upload/{operation}");
        let trash_key = SealedPackageTrashCheckpointVault::key(&account, operation);
        let restore_key = SealedPackageRestoreCheckpointVault::key(&account, operation);
        let upload = SealedUploadCheckpointVault::new(temp.path(), &account)
            .expect("fixture")
            .operation(&upload_key)
            .expect("fixture");
        let trash = SealedPackageTrashCheckpointVault::new(temp.path(), &account)
            .expect("fixture")
            .operation(&trash_key)
            .expect("fixture");
        let vault =
            SealedPackageRestoreCheckpointVault::new(temp.path(), &account).expect("fixture");
        let restore = vault.operation(&restore_key).expect("fixture");
        upload
            .save_with(&upload_key, SecretString::from("import"), &keyring)
            .await
            .expect("fixture");
        trash
            .save_with(&trash_key, SecretString::from("Trash proof"), &keyring)
            .await
            .expect("fixture");
        let upload_bytes = fs::read(upload.path()).expect("fixture");
        let trash_bytes = fs::read(trash.path()).expect("fixture");
        assert!(
            restore
                .load_with(&restore_key, &keyring)
                .await
                .expect("fixture")
                .is_none()
        );
        restore
            .save_with(&restore_key, SecretString::from("restore armed"), &keyring)
            .await
            .expect("fixture");
        assert_eq!(
            restore
                .load_with(&restore_key, &keyring)
                .await
                .expect("fixture")
                .expect("fixture")
                .expose_secret(),
            "restore armed"
        );
        assert_eq!(keyring.0.lock().expect("fixture").len(), 3);
        assert_eq!(fs::read(upload.path()).expect("fixture"), upload_bytes);
        assert_eq!(fs::read(trash.path()).expect("fixture"), trash_bytes);
        assert!(vault.operation(&trash_key).is_err() && vault.operation(&upload_key).is_err());
        fs::remove_file(restore.path()).expect("fixture");
        assert!(restore.load_with(&restore_key, &keyring).await.is_err());
        keyring
            .save(
                &restore_key,
                keyring
                    .load(&trash_key)
                    .await
                    .expect("fixture")
                    .expect("fixture"),
            )
            .await
            .expect("fixture");
        fs::copy(trash.path(), restore.path()).expect("fixture");
        assert!(
            restore.load_with(&trash_key, &keyring).await.is_err(),
            "purpose AAD must bind even with identical key and key ID"
        );
    }
}
