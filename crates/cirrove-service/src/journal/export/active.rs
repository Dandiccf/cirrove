//! Active working-byte recovery: copy privately, validate generation, then publish.
use super::*;

/// Selected under the journal owner's lock; copying never retains that mutex.
pub struct WorkingExportSource {
    file: File,
    source: WorkingRecovery,
    scope: Scope,
    working: PathBuf,
    journal_root: PathBuf,
    // Keep the same journal owner alive, including after account shutdown.
    owner: File,
}
/// Private copied bytes with no authority to publish until journal validation.
pub struct PreparedWorkingExport {
    source: WorkingRecovery,
    scope: Scope,
    working: PathBuf,
    copy: PreparedLocalCopy,
    owner: File,
}
/// A coherent selected generation, independent of subsequent working-file edits.
pub struct VerifiedWorkingExport {
    source: WorkingRecovery,
    copy: PreparedLocalCopy,
    _owner: File,
}
impl UploadJournal {
    pub fn working_export_source(&self, id: Uuid, generation: u64) -> Result<WorkingExportSource> {
        let record = self.working_file(id)?;
        if record.id != id
            || record.scope.account != self.account
            || record.generation != generation
            || !(record.dirty || record.unlinked)
        {
            return Err(JournalError::Stale);
        }
        let flags =
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC;
        let directory = File::from(
            rustix::fs::open(
                &self.working,
                flags | rustix::fs::OFlags::DIRECTORY,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let metadata = directory.metadata()?;
        if metadata.uid() != self._owner.metadata()?.uid()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(JournalError::Storage);
        }
        let file = File::from(
            rustix::fs::openat(
                &directory,
                id.to_string(),
                flags | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        owned_private(&file)?;
        let source = WorkingRecovery {
            file: id,
            generation,
            name: record.node.name,
            size: file.metadata()?.len(),
            recorded_size: record.node.size,
            unlinked: record.unlinked,
        };
        Ok(WorkingExportSource {
            file,
            source,
            scope: record.scope,
            working: self.working.clone(),
            journal_root: self
                .working
                .parent()
                .ok_or(JournalError::Storage)?
                .canonicalize()?,
            owner: self._owner.try_clone()?,
        })
    }

    /// Only journal reads under the caller's lock: no copying, syncing or publication.
    /// Every normal byte mutation increments generation before touching the file.
    pub fn verify_working_export(
        &self,
        prepared: PreparedWorkingExport,
    ) -> Result<VerifiedWorkingExport> {
        let record = self.working_file(prepared.source.file)?;
        if prepared.working != self.working
            || prepared.scope.account != self.account
            || record.id != prepared.source.file
            || record.scope != prepared.scope
            || record.generation != prepared.source.generation
            || !(record.dirty || record.unlinked)
        {
            return Err(JournalError::Stale);
        }
        Ok(VerifiedWorkingExport {
            source: prepared.source,
            copy: prepared.copy,
            _owner: prepared.owner,
        })
    }
}
impl WorkingExportSource {
    pub fn source(&self) -> &WorkingRecovery {
        &self.source
    }
    /// Blocking bounded-memory staging, outside the journal mutex. No visible file.
    pub fn prepare_copy(
        self,
        destination: &Path,
        cancel: &CancellationToken,
        progress: impl FnMut(u64),
    ) -> Result<PreparedWorkingExport> {
        let copy = prepare_local_copy(
            self.file,
            &self.journal_root,
            self.source.size,
            None,
            destination,
            cancel,
            progress,
        )?;
        Ok(PreparedWorkingExport {
            source: self.source,
            scope: self.scope,
            working: self.working,
            copy,
            owner: self.owner,
        })
    }
}
impl VerifiedWorkingExport {
    /// Publish outside the journal mutex. Later edits cannot alter these copied bytes;
    /// the receipt identifies the selected generation, not the current latest one.
    pub fn publish(self, cancel: &CancellationToken) -> Result<WorkingExportReceipt> {
        let destination = self.copy.destination.clone();
        let (_, sha256) = self.copy.publish(cancel)?;
        Ok(WorkingExportReceipt {
            source: self.source,
            sha256,
            destination,
        })
    }
}
