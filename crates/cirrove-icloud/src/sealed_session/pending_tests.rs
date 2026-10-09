#![allow(clippy::unwrap_used)]
use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct Keys(Mutex<Option<String>>);
#[async_trait]
impl CredentialVault for Keys {
    async fn load(&self, _: &str) -> Result<Option<SecretString>> {
        Ok(self.0.lock().unwrap().clone().map(SecretString::from))
    }
    async fn save(&self, _: &str, value: SecretString) -> Result<()> {
        *self.0.lock().unwrap() = Some(value.expose_secret().to_owned());
        Ok(())
    }
    async fn remove(&self, _: &str) -> Result<()> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

fn inventory(store: &SealedSessionVault) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = fs::read_dir(&store.account_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn occupied_pending_slot_preserves_key_blob_and_remnant() {
    let root = tempfile::tempdir().unwrap();
    let account = Uuid::new_v4().to_string();
    let operation = Uuid::new_v4();
    let stores = [
        SealedSessionVault::new(root.path(), &account).unwrap(),
        SealedUploadCheckpointVault::new(root.path(), &account)
            .unwrap()
            .operation(&format!("upload/{operation}"))
            .unwrap(),
    ];
    for store in stores {
        let keys = Keys::default();
        let key_id = "synthetic-only";
        store
            .save_with(key_id, SecretString::from("original"), &keys)
            .await
            .unwrap();
        let blob = fs::read(store.path()).unwrap();
        let key = keys.0.lock().unwrap().clone();
        let pending = store
            .account_dir
            .join(format!("{}.pending", store.temp_prefix));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pending)
            .unwrap();
        file.write_all(b"retained unpublished synthetic bytes")
            .unwrap();
        file.sync_all().unwrap();
        let remnant = fs::read(&pending).unwrap();
        let identity = file.metadata().unwrap();
        let names = inventory(&store);
        let result = store
            .save_with(key_id, SecretString::from("replacement"), &keys)
            .await;
        assert!(
            result.is_err(),
            "occupied publication slot must refuse without replacing saved state"
        );
        assert!(fs::read(store.path()).unwrap() == blob);
        assert!(*keys.0.lock().unwrap() == key);
        assert!(fs::read(&pending).unwrap() == remnant);
        let after = fs::symlink_metadata(&pending).unwrap();
        assert_eq!((after.dev(), after.ino()), (identity.dev(), identity.ino()));
        assert_eq!(inventory(&store), names);
        assert!(
            store
                .load_with(key_id, &keys)
                .await
                .unwrap()
                .unwrap()
                .expose_secret()
                == "original"
        );
    }
}
