//! Package checkpoints stay provider-owned: no FILE envelope or metadata-dependent recovery.
use super::*;
use cirrove_core::upload::UploadRepresentation;
use cirrove_icloud::{ICloudFileReplace, ICloudPackageCreate, ICloudSealedSignIn};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
impl ICloudWriteProvider {
    pub(super) fn package_staging(&self) -> Result<PathBuf> {
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
        let operation_id = self.validate_operation(operation, request)?;
        match (&request.representation, &request.intent) {
            (UploadRepresentation::PackageArchive { .. }, UploadIntent::Create { .. }) => {}
            (
                UploadRepresentation::PackageReplacementArchive { .. },
                UploadIntent::Replace { .. },
            ) => {}
            _ => return Err(UploadError::Unsupported("invalid native package operation")),
        }
        #[cfg(test)]
        if let Some(adapter) = &self.package_test_adapter {
            return Ok(adapter.clone());
        }
        let staging = self.package_staging()?;
        if let UploadRepresentation::PackageReplacementArchive { original, .. } =
            &request.representation
        {
            let sign_in = ICloudSealedSignIn {
                apple_id: self.apple_id.clone(),
                credential_id: self.credential_id.clone(),
            };
            if let Some(saved) = checkpoint {
                // The original may already be in Trash. Restore only the exact
                // captured operation/request/parent, never today's path index.
                return Ok(Arc::new(
                    ICloudFileReplace::restore_native_package_from_sealed_checkpoint(
                        request.clone(),
                        operation_id,
                        sign_in,
                        &self.state,
                        &staging,
                        saved,
                    )?,
                ));
            }
            let parent = self
                .parent(original.parent_id.as_deref().ok_or(UploadError::Invalid)?)
                .await?;
            // Construction performs no provider I/O; begin/allocation verify
            // the selected original's actual representation and semantic bytes.
            return Ok(Arc::new(
                ICloudFileReplace::native_package_from_sealed_session(
                    request.clone(),
                    parent,
                    operation_id,
                    sign_in,
                    &self.state,
                    &staging,
                )?,
            ));
        }

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
