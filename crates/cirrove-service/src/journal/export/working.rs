//! Working-byte recovery is offline only: the owner lease lasts through copying.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkingRecovery {
    pub file: Uuid,
    pub generation: u64,
    pub name: String,
    /// Actual retained bytes, possibly a partial write after process death.
    pub size: u64,
    pub recorded_size: u64,
    pub unlinked: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkingExportReceipt {
    pub source: WorkingRecovery,
    /// Digest of the recovered bytes, not a prior sealed-save checksum.
    pub sha256: String,
    pub destination: PathBuf,
}
impl RecoveryJournal {
    fn working_source(&self, id: Uuid) -> Result<(WorkingRecovery, File)> {
        let record = self.journal.working_file(id)?;
        if record.id != id
            || record.scope.account != self.journal.account
            || !(record.dirty || record.unlinked)
        {
            return Err(JournalError::Stale);
        }
        let flags =
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC;
        let directory = File::from(
            rustix::fs::openat(
                &self._directory,
                "working",
                flags | rustix::fs::OFlags::DIRECTORY,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let metadata = directory.metadata()?;
        if metadata.uid() != self._directory.metadata()?.uid()
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
        Ok((
            WorkingRecovery {
                file: id,
                generation: record.generation,
                name: record.node.name,
                size: file.metadata()?.len(),
                recorded_size: record.node.size,
                unlinked: record.unlinked,
            },
            file,
        ))
    }
    /// UUID pagination covers all working records, including clean entries.
    /// Clean entries are omitted, but `next` advances over the scanned records.
    pub fn working_list(
        &self,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<(Vec<WorkingRecovery>, Option<Uuid>)> {
        let mut query = self
            .journal
            .db
            .prepare("SELECT id FROM working_files WHERE id > ?1 ORDER BY id LIMIT ?2")?;
        let ids = query
            .query_map(
                params![
                    after.map(|id| id.to_string()).unwrap_or_default(),
                    limit.clamp(1, 200) + 1
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let more = ids.len() > limit.clamp(1, 200) as usize;
        let mut rows = Vec::new();
        let mut next = None;
        for id in ids.into_iter().take(limit.clamp(1, 200) as usize) {
            let id = Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?;
            next = Some(id);
            let record = self.journal.working_file(id)?;
            if record.dirty || record.unlinked {
                rows.push(self.working_source(id)?.0);
            }
        }
        Ok((rows, if more { next } else { None }))
    }
    /// Does not seal, migrate, resume an upload or modify journal records.
    pub fn export_working(
        &self,
        id: Uuid,
        generation: u64,
        destination: &Path,
        cancel: &CancellationToken,
        progress: impl FnMut(u64),
    ) -> Result<WorkingExportReceipt> {
        let (source, file) = self.working_source(id)?;
        if source.generation != generation {
            return Err(JournalError::Stale);
        }
        let root = std::fs::read_link(format!("/proc/self/fd/{}", self._directory.as_raw_fd()))?;
        let (_, sha256) = copy_local_file(
            file,
            &root,
            source.size,
            None,
            destination,
            cancel,
            progress,
        )?;
        Ok(WorkingExportReceipt {
            source,
            sha256,
            destination: destination.into(),
        })
    }
}
