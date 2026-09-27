//! Private, feature-gated crash experiment for a two-ID iCloud handoff.
//! No secret or response body enters this file. Pending requests are never
//! blindly replayed after a process boundary.
use anyhow::{Context, Result, bail};
use cirrove_icloud::{HandoffObserved, HandoffPlan, ICloudReadSession};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAX_RECORD: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    OldRenamePending,
    OldAtRecovery,
    NewRenamePending,
    Complete,
}

#[derive(Deserialize, Serialize)]
struct Record {
    version: u8,
    account_id: String,
    collection: String,
    phase: Phase,
    plan: HandoffPlan,
}

pub struct Journal {
    directory: PathBuf,
    path: PathBuf,
    record: Record,
}

fn private_directory(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.permissions().mode() & 0o077 != 0
        || meta.uid() != fs::metadata("/proc/self")?.uid()
    {
        bail!("iCloud handoff journal directory is not private");
    }
    Ok(())
}

impl Journal {
    pub fn create(directory: &Path, account_id: &str, plan: HandoffPlan) -> Result<Self> {
        Uuid::parse_str(account_id).context("invalid iCloud validation account ID")?;
        private_directory(directory)?;
        let path = directory.join("record.json");
        let record = Record {
            version: 1,
            account_id: account_id.into(),
            collection: "drive".into(),
            phase: Phase::Prepared,
            plan,
        };
        let bytes = serde_json::to_vec(&record)?;
        if bytes.len() > MAX_RECORD as usize {
            bail!("iCloud handoff checkpoint exceeds limit");
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .context(
                "iCloud handoff record already exists; inspect it instead of starting again",
            )?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        File::open(directory)?.sync_all()?;
        Ok(Self {
            directory: directory.into(),
            path,
            record,
        })
    }

    pub fn load(directory: &Path, account_id: &str) -> Result<Self> {
        private_directory(directory)?;
        let path = directory.join("record.json");
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() > MAX_RECORD
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != fs::metadata("/proc/self")?.uid()
        {
            bail!("iCloud handoff record is not a private regular file");
        }
        let record: Record =
            serde_json::from_slice(&fs::read(&path)?).context("invalid iCloud handoff record")?;
        if record.version != 1 || record.account_id != account_id || record.collection != "drive" {
            bail!("iCloud handoff record belongs to a different account or format");
        }
        Ok(Self {
            directory: directory.into(),
            path,
            record,
        })
    }

    fn save(&mut self, phase: Phase) -> Result<()> {
        self.record.phase = phase;
        let bytes = serde_json::to_vec(&self.record)?;
        if bytes.len() > MAX_RECORD as usize {
            bail!("iCloud handoff checkpoint exceeds limit");
        }
        let temp = self
            .directory
            .join(format!("record-{}.tmp", Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, &self.path)?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    async fn move_old(&mut self, session: &mut ICloudReadSession) -> Result<()> {
        self.save(Phase::OldRenamePending)?;
        let _accepted = session.move_old_to_recovery(&self.record.plan).await?;
        if session.inspect_durable_handoff(&self.record.plan).await?
            != HandoffObserved::OldAtRecovery
        {
            bail!("old iCloud rename is uncertain; no request will be replayed");
        }
        self.save(Phase::OldAtRecovery)
    }

    async fn move_new(&mut self, session: &mut ICloudReadSession) -> Result<()> {
        self.save(Phase::NewRenamePending)?;
        let _accepted = session.move_staged_to_target(&self.record.plan).await?;
        if session.inspect_durable_handoff(&self.record.plan).await? != HandoffObserved::Complete {
            bail!("new iCloud rename is uncertain; no request will be replayed");
        }
        self.save(Phase::Complete)
    }

    /// The first process intentionally exits after the first verified rename.
    pub async fn stop_after_recovery(&mut self, session: &mut ICloudReadSession) -> Result<()> {
        if self.record.phase != Phase::Prepared
            || session.inspect_durable_handoff(&self.record.plan).await?
                != HandoffObserved::Prepared
        {
            bail!("iCloud handoff is not freshly prepared");
        }
        self.move_old(session).await
    }

    /// Fault injection: send the old rename after fsyncing the pending phase,
    /// then intentionally discard its receipt and exit before reconciliation.
    pub async fn drop_old_rename_receipt(&mut self, session: &mut ICloudReadSession) -> Result<()> {
        if self.record.phase != Phase::Prepared
            || session.inspect_durable_handoff(&self.record.plan).await?
                != HandoffObserved::Prepared
        {
            bail!("iCloud handoff is not freshly prepared");
        }
        self.save(Phase::OldRenamePending)?;
        let _discarded_receipt = session.move_old_to_recovery(&self.record.plan).await?;
        Ok(())
    }

    /// Fault injection after the second rename request. The first rename is
    /// already verified and checkpointed; the second receipt is discarded.
    pub async fn drop_new_rename_receipt(&mut self, session: &mut ICloudReadSession) -> Result<()> {
        self.stop_after_recovery(session).await?;
        self.save(Phase::NewRenamePending)?;
        let _discarded_receipt = session.move_staged_to_target(&self.record.plan).await?;
        Ok(())
    }

    /// A new process reconciles pending phases by exact IDs and hashes. It
    /// never resends a request that may have reached Apple.
    pub async fn resume(&mut self, session: &mut ICloudReadSession) -> Result<()> {
        let observed = session.inspect_durable_handoff(&self.record.plan).await?;
        match self.record.phase {
            Phase::Prepared if observed == HandoffObserved::Prepared => {
                self.move_old(session).await?;
            }
            Phase::OldRenamePending if observed == HandoffObserved::OldAtRecovery => {
                self.save(Phase::OldAtRecovery)?;
            }
            Phase::OldAtRecovery if observed == HandoffObserved::OldAtRecovery => {}
            Phase::NewRenamePending if observed == HandoffObserved::Complete => {
                self.save(Phase::Complete)?;
                return Ok(());
            }
            Phase::Complete if observed == HandoffObserved::Complete => return Ok(()),
            _ => bail!("iCloud handoff is uncertain or diverged; no request will be replayed"),
        }
        self.move_new(session).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn private_checkpoint_survives_restart_and_cannot_change_accounts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("handoff");
        let account = Uuid::new_v4().to_string();
        let plan: HandoffPlan = serde_json::from_value(json!({
            "version": 1,
            "folder_id": "FOLDER::com.apple.CloudDocs::folder-1",
            "folder_name": format!("Cirrove Write Validation-{}", Uuid::new_v4()),
            "original_id": "FILE::com.apple.CloudDocs::old-1",
            "original_doc_id": "old-1",
            "staged_id": "FILE::com.apple.CloudDocs::new-1",
            "staged_doc_id": "new-1",
            "staged_name": format!("staged-by-cirrove-{}.txt", Uuid::new_v4()),
            "recovery_name": format!("recovery-by-cirrove-{}.txt", Uuid::new_v4()),
            "original_sha256": "a".repeat(64),
            "staged_sha256": "b".repeat(64)
        }))
        .unwrap();
        let mut journal = Journal::create(&path, &account, plan).unwrap();
        let duplicate: HandoffPlan =
            serde_json::from_slice(&serde_json::to_vec(&journal.record.plan).unwrap()).unwrap();
        assert!(Journal::create(&path, &account, duplicate).is_err());
        journal.save(Phase::OldRenamePending).unwrap();
        let restored = Journal::load(&path, &account).unwrap();
        assert_eq!(restored.record.phase, Phase::OldRenamePending);
        assert!(Journal::load(&path, &Uuid::new_v4().to_string()).is_err());
        assert_eq!(
            fs::metadata(path.join("record.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
