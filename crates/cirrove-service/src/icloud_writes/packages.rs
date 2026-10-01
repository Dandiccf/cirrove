//! Package checkpoints stay provider-owned: no FILE envelope or metadata-dependent recovery.
use super::*;
use cirrove_icloud::ICloudPackageCreate;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
impl ICloudWriteProvider {
    fn package_staging(&self) -> Result<PathBuf> {
        let uid = std::fs::metadata("/proc/self")
            .map_err(|_| UploadError::Uncertain)?
            .uid();
        let mut path = self.state.clone();
        for part in [
            "accounts",
            self.scope.account.as_str(),
            "package-upload-staging",
        ] {
            path.push(part);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(_) => return Err(UploadError::Uncertain),
                    }
                }
                Err(_) => return Err(UploadError::Uncertain),
            }
            let meta = std::fs::symlink_metadata(&path).map_err(|_| UploadError::Uncertain)?;
            let forbidden = if part == "accounts" { 0o022 } else { 0o077 };
            if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & forbidden != 0 {
                return Err(UploadError::Invalid);
            }
        }
        Ok(path)
    }
    pub(super) async fn package_adapter(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
    ) -> Result<Arc<dyn UploadProvider>> {
        self.validate_operation(operation, request)?;
        if request.representation.is_file_bytes()
            || !matches!(request.intent, UploadIntent::Create { .. })
        {
            return Err(UploadError::Unsupported("native package replacement"));
        }
        #[cfg(test)]
        if let Some(adapter) = &self.package_test_adapter {
            return Ok(adapter.clone());
        }
        let staging = self.package_staging()?;
        if let Some(checkpoint) = checkpoint {
            return Ok(Arc::new(
                ICloudPackageCreate::restore_from_sealed_checkpoint(
                    self.scope.clone(),
                    self.apple_id.clone(),
                    self.credential_id.clone(),
                    &self.state,
                    &staging,
                    operation,
                    request,
                    checkpoint,
                )?,
            ));
        }
        let UploadIntent::Create { parent, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let parent = self.parent(parent).await?;
        Ok(Arc::new(ICloudPackageCreate::from_sealed_session(
            self.scope.clone(),
            self.apple_id.clone(),
            self.credential_id.clone(),
            &self.state,
            parent,
            &staging,
        )?))
    }
}
#[cfg(test)]
mod tests;
