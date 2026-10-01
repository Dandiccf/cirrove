//! Observation-only authority for explicitly abandoning one pre-handoff attempt.
//! The staged object is retained, never deleted or published as successful content.
use super::*;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeReplacementAbandonRecord {
    pub version: u8,
    pub operation: Uuid,
    pub request: UploadRequest,
    pub original: Node,
    /// Historical allocated identity, not a semantic completion receipt.
    pub staged: Node,
    pub checkpoint_sha256: String,
    pub observed_unix: u64,
}
impl NativeReplacementAbandonRecord {
    pub fn validate(&self) -> UploadResult<()> {
        self.request.validate()?;
        let UploadRepresentation::PackageReplacementArchive { original, .. } =
            &self.request.representation
        else {
            return Err(UploadError::Invalid);
        };
        if self.version != 1
            || self.operation.is_nil()
            || original.as_ref() != &self.original
            || self.request.scope.provider != "icloud"
            || self.request.scope.collection != "drive"
            || !matches!(&self.request.intent,UploadIntent::Replace {item,expected_etag} if item==&self.original.id && Some(expected_etag)==self.original.etag.as_ref())
            || self.staged.id == self.original.id
            || !self.staged.id.starts_with("FILE::com.apple.CloudDocs::")
            || self.staged.id.ends_with("::")
            || self.staged.parent_id != self.original.parent_id
            || self.staged.kind != NodeKind::Folder
            || !self.staged.package
            || self.staged.target.is_some()
            || self.staged.content_version.is_some()
            || self.staged.name != format!("staged-by-cirrove-{}.pages", self.operation)
            || self.staged.etag.as_ref().is_none_or(|v| {
                v.is_empty() || v.len() > 4096 || v.contains(['*', '\0', '\r', '\n'])
            })
            || self.checkpoint_sha256.len() != 64
            || !self
                .checkpoint_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }
}
/// No Deserialize or public constructor: only exact read-only observation can
/// mint this capability. Local final-state checks remain the journal's job.
pub struct NativeReplacementAbandonEvidence {
    record: NativeReplacementAbandonRecord,
    observed: Instant,
}
impl NativeReplacementAbandonEvidence {
    pub fn record(&self) -> &NativeReplacementAbandonRecord {
        &self.record
    }
    pub fn is_fresh(&self) -> bool {
        self.observed.elapsed() <= Duration::from_secs(60)
    }
    #[cfg(feature = "test-support")]
    pub fn synthetic_for_test(record: NativeReplacementAbandonRecord) -> UploadResult<Self> {
        record.validate()?;
        Ok(Self {
            record,
            observed: Instant::now(),
        })
    }
}
fn entry(value: Value, id: &str, parent: &str, name: &str) -> UploadResult<crate::DriveEntry> {
    if value.get("restorePath").is_some_and(|v| !v.is_null()) {
        return Err(UploadError::Conflict);
    }
    let e: crate::DriveEntry = serde_json::from_value(value).map_err(|_| UploadError::Conflict)?;
    if e.drivewsid != id
        || e.docwsid != id.rsplit("::").next().unwrap_or_default()
        || e.zone != "com.apple.CloudDocs"
        || e.kind != "FILE"
        || e.parent_id != parent
        || e.parent_id == TRASH_ROOT
        || e.display_name() != name
        || e.etag.is_empty()
        || e.etag.len() > 4096
        || e.etag.contains(['*', '\0', '\r', '\n'])
    {
        return Err(UploadError::Conflict);
    }
    Ok(e)
}
impl ICloudFileReplace {
    /// Read the current account-scoped continuation internally. A caller cannot
    /// present a historical Stage snapshot instead of a later Handoff checkpoint.
    pub async fn inspect_native_stage_abandonment(
        &self,
        operation: &str,
        request: &UploadRequest,
        vault: &crate::SealedUploadCheckpointVault,
        cancel: &CancellationToken,
    ) -> UploadResult<NativeReplacementAbandonEvidence> {
        self.native_check(operation, request)?;
        if !vault.is_for_account(&request.scope.account) {
            return Err(UploadError::CheckpointInvalid);
        }
        tokio::select! {biased;_=cancel.cancelled()=>Err(UploadError::Uncertain),result=tokio::time::timeout(Duration::from_secs(180),async {
            let key=format!("upload/{operation}");
            let checkpoint=vault.load(&key).await.map_err(|_|UploadError::Uncertain)?.ok_or(UploadError::CheckpointInvalid)?;
            let proof=self.inspect_native_stage_abandonment_checkpoint(operation,request,&checkpoint,cancel).await?;
            let current=vault.load(&key).await.map_err(|_|UploadError::Uncertain)?.ok_or(UploadError::CheckpointInvalid)?;
            if current.expose_secret()!=checkpoint.expose_secret() || cancel.is_cancelled() {return Err(UploadError::CheckpointInvalid)}
            Ok(proof)
        })=>result.map_err(|_|UploadError::Uncertain)?}
    }
    /// Read-only; accepts ONLY the durably retained Stage/RegistrationArmed
    /// checkpoint. Handoff checkpoints, even MoveOld, are never abandonment proof.
    pub(super) async fn inspect_native_stage_abandonment_checkpoint(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> UploadResult<NativeReplacementAbandonEvidence> {
        let NativePhase::Stage { inner } = self.native_decode(operation, request, checkpoint)?
        else {
            return Err(UploadError::CheckpointInvalid);
        };
        let context = self.native.as_ref().ok_or(UploadError::Invalid)?;
        let stage = context.provider.registered_stage_for_abandonment(
            operation,
            &self.native_stage_request()?,
            &unpack(inner)?,
            &context.account_hash,
        )?;
        if stage == self.original.id {
            return Err(UploadError::CheckpointInvalid);
        }
        tokio::select! {biased; _=cancel.cancelled()=>Err(UploadError::Uncertain), result=tokio::time::timeout(Duration::from_secs(120),async {
            let mut session=self.native_session(cancel).await?;
            let original=entry(session.item_details(&self.original.id).await.map_err(crate::file_create::map_session_error)?,&self.original.id,&self.folder.id,&self.original.name)?;
            if self.original.etag.as_ref()!=Some(&original.etag) || self.original.size!=original.size {return Err(UploadError::Conflict)}
            let staged=entry(session.item_details(&stage).await.map_err(crate::file_create::map_session_error)?,&stage,&self.folder.id,&self.stage_name)?;
            if !matches!(session.download_representation(&stage).await.map_err(crate::file_create::map_session_error)?,crate::ContentRepresentation::Package(_)) {return Err(UploadError::Conflict)}
            let after_original=entry(session.item_details(&self.original.id).await.map_err(crate::file_create::map_session_error)?,&self.original.id,&self.folder.id,&self.original.name)?;
            let after_staged=entry(session.item_details(&stage).await.map_err(crate::file_create::map_session_error)?,&stage,&self.folder.id,&self.stage_name)?;
            if original!=after_original || staged!=after_staged || cancel.is_cancelled() {return Err(UploadError::Conflict)}
            let record=NativeReplacementAbandonRecord {
                version:1,operation:self.operation,request:request.clone(),original:self.original.clone(),
                staged:Node {id:staged.drivewsid.clone(),parent_id:Some(staged.parent_id.clone()),name:staged.display_name(),kind:NodeKind::Folder,size:staged.size,modified_unix:0,etag:Some(staged.etag.clone()),content_version:None,target:None,package:true},
                checkpoint_sha256:hex::encode(Sha256::digest(checkpoint.expose_secret().as_bytes())),
                observed_unix:SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            };
            record.validate()?;
            Ok(NativeReplacementAbandonEvidence {record,observed:Instant::now()})
        })=>result.map_err(|_|UploadError::Uncertain)?}
    }
}
