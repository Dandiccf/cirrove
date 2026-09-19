//! Google create uploads. This adapter is deliberately not selected by the
//! service yet: the mounted namespace and replacement policy remain read-only.
//! Prepared IDs and resumable session URLs stay inside opaque vault checkpoints.
use super::{files::File, transport::body, *};
use async_trait::async_trait;
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest,
};
use cirrove_core::upload::{
    Reconciliation, Result, UploadError, UploadIntent, UploadProgress, UploadProvider,
    UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Priority, ProviderError, ReadProvider};
use reqwest::{Method, Response, StatusCode, Url};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{future::Future, time::Duration};

const ALIGNMENT: u64 = 256 * 1024;
const PART_SIZE: u32 = 8 * 1024 * 1024;
const MAX_UPLOAD_RESPONSE: usize = 1024 * 1024;
const MAX_CHECKPOINT: usize = 64 * 1024;
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// Exact identity for an isolated folder create. The caller persists this
/// before mutation, so a lost response can be inspected by ID rather than by
/// Google's non-unique sibling name.
#[derive(Clone, Serialize)]
pub struct PreparedFolder {
    id: String,
    parent: String,
    name: String,
}

/// Namespace mutations used only by the isolated write validator.
///
/// Google Drive permits duplicate sibling names and offers no atomic
/// fail-on-collision relocate operation. This adapter performs a bounded
/// destination check for the validator's private folder and can trash an exact
/// regular file conditionally, but ordinary account construction deliberately
/// never exposes it as a writable mount provider.
#[derive(Clone)]
pub struct GoogleValidationMutations {
    drive: GoogleDrive,
}
impl PreparedFolder {
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Result of an isolated test against Drive's undocumented HTTP ETag behavior.
/// The ETag itself is deliberately not exposed or persisted in this result.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum MetadataPreconditionProbe {
    NoStrongEtag {
        item: String,
        version: String,
    },
    Tested {
        item: String,
        before_version: String,
        updated_version: String,
        final_version: String,
        stale_rejected: bool,
        final_name: String,
    },
}
impl MetadataPreconditionProbe {
    pub fn stale_rejected(&self) -> Option<bool> {
        match self {
            Self::NoStrongEtag { .. } => None,
            Self::Tested { stale_rejected, .. } => Some(*stale_rejected),
        }
    }
}

/// Exact item and names for one isolated metadata-precondition probe. Persist
/// this value before calling `probe_metadata_precondition`.
#[derive(Clone, Serialize)]
pub struct MetadataPreconditionPlan {
    item: String,
    parent: String,
    original_name: String,
    accepted_name: String,
    stale_name: String,
}

/// Exact item and content digests for one isolated conditional-content probe.
/// The small deterministic payloads are supplied separately and never logged.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ContentPreconditionPlan {
    item: String,
    parent: String,
    name: String,
    accepted_size: u64,
    accepted_sha256: String,
    stale_size: u64,
    stale_sha256: String,
}

/// Exact item and digests for the large-content variant of the conditional
/// write probe. Session URLs and payload bytes are never part of this plan.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ResumablePreconditionPlan {
    item: String,
    parent: String,
    name: String,
    accepted_size: u64,
    accepted_sha256: String,
    stale_size: u64,
    stale_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ContentPreconditionProbe {
    NoStrongEtag {
        item: String,
        version: String,
    },
    Tested {
        item: String,
        before_version: String,
        updated_version: String,
        final_version: String,
        stale_rejected: bool,
        final_size: u64,
        final_sha256: String,
    },
}
impl ContentPreconditionProbe {
    pub fn stale_rejected(&self) -> Option<bool> {
        match self {
            Self::NoStrongEtag { .. } => None,
            Self::Tested { stale_rejected, .. } => Some(*stale_rejected),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ResumablePreconditionProbe {
    NoStrongEtag {
        item: String,
        version: String,
    },
    Tested {
        item: String,
        before_version: String,
        updated_version: String,
        final_version: String,
        stale_rejected: bool,
        final_size: u64,
        final_sha256: String,
    },
}
impl ResumablePreconditionProbe {
    pub fn stale_rejected(&self) -> Option<bool> {
        match self {
            Self::NoStrongEtag { .. } => None,
            Self::Tested { stale_rejected, .. } => Some(*stale_rejected),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum SavedUpload {
    Prepared {
        version: u8,
        request: UploadRequest,
        id: String,
    },
    Session {
        version: u8,
        request: UploadRequest,
        id: String,
        url: String,
        offset: u64,
        length: u32,
    },
}
impl SavedUpload {
    fn request(&self) -> &UploadRequest {
        match self {
            Self::Prepared { request, .. } | Self::Session { request, .. } => request,
        }
    }
    fn id(&self) -> &str {
        match self {
            Self::Prepared { id, .. } | Self::Session { id, .. } => id,
        }
    }
}

#[derive(Deserialize)]
struct GeneratedIds {
    #[serde(default)]
    ids: Vec<String>,
    space: String,
    kind: String,
}

fn protocol(message: &'static str) -> UploadError {
    ProviderError::Protocol(message).into()
}

fn decimal_version_after(after: &str, before: &str) -> bool {
    let after = after.trim_start_matches('0');
    let before = before.trim_start_matches('0');
    let after = if after.is_empty() { "0" } else { after };
    let before = if before.is_empty() { "0" } else { before };
    after.len() > before.len() || (after.len() == before.len() && after > before)
}

fn strong_etag(response: &Response) -> Option<String> {
    response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value.len() <= 1024
                && value.starts_with('"')
                && value.ends_with('"')
                && !value.starts_with("W/")
        })
        .map(str::to_owned)
}

fn valid_strong_etag(value: &str) -> bool {
    value.len() <= 1024
        && value.starts_with('"')
        && value.ends_with('"')
        && !value.starts_with("W/")
        && !value.contains(['\r', '\n'])
}

impl GoogleDrive {
    /// Build the namespace adapter for a disabled, separately consented
    /// validation account. Normal Google mounts never call this method.
    pub fn validation_mutations(&self) -> GoogleValidationMutations {
        GoogleValidationMutations {
            drive: self.clone(),
        }
    }

    fn check_folder_destination(&self, scope: &Scope, parent: &str, name: &str) -> Result<()> {
        self.check_scope(scope).map_err(UploadError::Provider)?;
        valid_id(parent).map_err(UploadError::Provider)?;
        if name.is_empty()
            || name.len() > 4096
            || matches!(name, "." | "..")
            || name.contains(['/', '\0'])
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }

    fn check_folder_plan(&self, scope: &Scope, plan: &PreparedFolder) -> Result<()> {
        self.check_folder_destination(scope, &plan.parent, &plan.name)?;
        valid_id(&plan.id).map_err(UploadError::Provider)
    }

    fn folder_receipt(
        &self,
        plan: &PreparedFolder,
        file: File,
        etag: Option<String>,
    ) -> Result<Node> {
        if file.id != plan.id
            || file.name != plan.name
            || file.parents.as_slice() != [plan.parent.as_str()]
            || file.mime_type != FOLDER_MIME
            || file.trashed
            || file.drive_id.is_some()
        {
            return Err(UploadError::Uncertain);
        }
        let mut node = file
            .node(&self.collection)
            .map_err(|_| UploadError::Uncertain)?;
        node.name = plan.name.clone();
        node.etag = Some(etag.ok_or_else(|| protocol("missing strong Google ETag"))?);
        if node.kind != NodeKind::Folder {
            return Err(UploadError::Uncertain);
        }
        Ok(node)
    }

    /// Reserve a provider identity without changing Drive. The returned value
    /// must be persisted before `create_prepared_folder` is called.
    pub async fn prepare_folder(
        &self,
        scope: &Scope,
        parent: &str,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<PreparedFolder> {
        self.check_folder_destination(scope, parent, name)?;
        self.upload_call(cancel, Duration::from_secs(125), async {
            let plan = PreparedFolder {
                id: self.generated_id().await?,
                parent: parent.into(),
                name: name.into(),
            };
            self.check_folder_plan(scope, &plan)?;
            Ok(plan)
        })
        .await
    }

    /// Create only the exact prepared folder identity. A transport error is
    /// uncertain; inspect the same plan before attempting anything else.
    pub async fn create_prepared_folder(
        &self,
        scope: &Scope,
        plan: &PreparedFolder,
        cancel: &CancellationToken,
    ) -> Result<Node> {
        self.check_folder_plan(scope, plan)?;
        self.upload_call(cancel, Duration::from_secs(125), async {
            let mut url = self.url(&["files"])?;
            url.query_pairs_mut().append_pair("fields", files::FIELDS);
            let response = self
                .authorized_json_upload(
                    Method::POST,
                    url,
                    &json!({
                        "id": plan.id,
                        "name": plan.name,
                        "parents": [plan.parent],
                        "mimeType": FOLDER_MIME
                    }),
                    None,
                    None,
                )
                .await?;
            let etag = strong_etag(&response);
            let bytes = body(response, MAX_UPLOAD_RESPONSE)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            let file: File = serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
            self.folder_receipt(plan, file, etag)
        })
        .await
    }

    /// Inspect the prepared ID after a lost response. `None` proves only that
    /// this exact identity is not currently present.
    pub async fn inspect_prepared_folder(
        &self,
        scope: &Scope,
        plan: &PreparedFolder,
        cancel: &CancellationToken,
    ) -> Result<Option<Node>> {
        self.check_folder_plan(scope, plan)?;
        self.upload_call(cancel, Duration::from_secs(125), async {
            match self.file_with_strong_etag(&plan.id).await {
                Ok((file, etag)) => self.folder_receipt(plan, file, etag).map(Some),
                Err(UploadError::Provider(ProviderError::NotFound)) => Ok(None),
                Err(error) => Err(error),
            }
        })
        .await
    }

    /// Validate a no-network probe plan. The caller persists the returned value
    /// before the first metadata mutation.
    pub fn prepare_metadata_precondition_probe(
        &self,
        scope: &Scope,
        item: &str,
        parent: &str,
        original_name: &str,
        accepted_name: &str,
        stale_name: &str,
    ) -> Result<MetadataPreconditionPlan> {
        self.check_folder_destination(scope, parent, original_name)?;
        for name in [accepted_name, stale_name] {
            self.check_folder_destination(scope, parent, name)?;
        }
        valid_id(item).map_err(UploadError::Provider)?;
        if accepted_name == original_name
            || stale_name == original_name
            || stale_name == accepted_name
        {
            return Err(UploadError::Invalid);
        }
        Ok(MetadataPreconditionPlan {
            item: item.into(),
            parent: parent.into(),
            original_name: original_name.into(),
            accepted_name: accepted_name.into(),
            stale_name: stale_name.into(),
        })
    }

    /// Characterize whether Drive v3 honors a strong response ETag as an
    /// `If-Match` precondition. This changes only the exact item in a plan that
    /// the caller has already persisted and created for this isolated run.
    pub async fn probe_metadata_precondition(
        &self,
        scope: &Scope,
        plan: &MetadataPreconditionPlan,
        cancel: &CancellationToken,
    ) -> Result<MetadataPreconditionProbe> {
        let checked = self.prepare_metadata_precondition_probe(
            scope,
            &plan.item,
            &plan.parent,
            &plan.original_name,
            &plan.accepted_name,
            &plan.stale_name,
        )?;
        self.upload_call(cancel, Duration::from_secs(125), async {
            let (before, etag) = self.file_with_strong_etag(&checked.item).await?;
            self.check_probe_file(
                &before,
                &checked.item,
                &checked.parent,
                &checked.original_name,
            )?;
            let before_version = self.file_version(&before)?;
            let Some(etag) = etag else {
                return Ok(MetadataPreconditionProbe::NoStrongEtag {
                    item: checked.item.clone(),
                    version: before_version,
                });
            };

            let updated = self
                .conditional_metadata_name(&checked.item, &checked.accepted_name, &etag)
                .await?;
            self.check_probe_file(
                &updated,
                &checked.item,
                &checked.parent,
                &checked.accepted_name,
            )?;
            let updated_version = self.file_version(&updated)?;
            if !decimal_version_after(&updated_version, &before_version) {
                return Err(UploadError::Uncertain);
            }

            let stale_rejected = match self
                .conditional_metadata_name(&checked.item, &checked.stale_name, &etag)
                .await
            {
                Err(UploadError::Conflict) => true,
                Ok(stale) => {
                    self.check_probe_file(
                        &stale,
                        &checked.item,
                        &checked.parent,
                        &checked.stale_name,
                    )?;
                    false
                }
                Err(error) => return Err(error),
            };
            let final_file = self.file(&checked.item).await?;
            let expected_name = if stale_rejected {
                checked.accepted_name.as_str()
            } else {
                checked.stale_name.as_str()
            };
            self.check_probe_file(&final_file, &checked.item, &checked.parent, expected_name)?;
            let final_version = self.file_version(&final_file)?;
            if final_version != updated_version
                && !decimal_version_after(&final_version, &updated_version)
            {
                return Err(UploadError::Uncertain);
            }
            Ok(MetadataPreconditionProbe::Tested {
                item: checked.item.clone(),
                before_version,
                updated_version,
                final_version,
                stale_rejected,
                final_name: expected_name.into(),
            })
        })
        .await
    }

    /// Build the durable, non-secret description of one content-precondition
    /// probe. Payload bytes remain caller-owned and are checked against this
    /// plan before any request is sent.
    pub fn prepare_content_precondition_probe(
        &self,
        scope: &Scope,
        item: &str,
        parent: &str,
        name: &str,
        accepted: &[u8],
        stale: &[u8],
    ) -> Result<ContentPreconditionPlan> {
        self.check_folder_destination(scope, parent, name)?;
        valid_id(item).map_err(UploadError::Provider)?;
        if accepted.is_empty()
            || stale.is_empty()
            || accepted.len() > 5 * 1024 * 1024
            || stale.len() > 5 * 1024 * 1024
        {
            return Err(UploadError::Invalid);
        }
        let accepted_sha256 = hex::encode(Sha256::digest(accepted));
        let stale_sha256 = hex::encode(Sha256::digest(stale));
        if accepted_sha256 == stale_sha256 {
            return Err(UploadError::Invalid);
        }
        Ok(ContentPreconditionPlan {
            item: item.into(),
            parent: parent.into(),
            name: name.into(),
            accepted_size: accepted.len() as u64,
            accepted_sha256,
            stale_size: stale.len() as u64,
            stale_sha256,
        })
    }

    /// Apply one small conditional content update, then try a distinct update
    /// with the now-stale ETag and read the exact item back by ID and SHA-256.
    pub async fn probe_content_precondition(
        &self,
        scope: &Scope,
        plan: &ContentPreconditionPlan,
        accepted: Vec<u8>,
        stale: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Result<ContentPreconditionProbe> {
        let checked = self.prepare_content_precondition_probe(
            scope,
            &plan.item,
            &plan.parent,
            &plan.name,
            &accepted,
            &stale,
        )?;
        if checked != *plan {
            return Err(UploadError::Invalid);
        }
        self.upload_call(cancel, Duration::from_secs(125), async {
            let (before, etag) = self.file_with_strong_etag(&checked.item).await?;
            self.check_probe_file(&before, &checked.item, &checked.parent, &checked.name)?;
            let before_version = self.file_version(&before)?;
            let Some(etag) = etag else {
                return Ok(ContentPreconditionProbe::NoStrongEtag {
                    item: checked.item.clone(),
                    version: before_version,
                });
            };

            let updated = self
                .conditional_content(&checked.item, accepted, &etag)
                .await?;
            self.check_probe_content(
                &updated,
                &checked.item,
                &checked.parent,
                &checked.name,
                checked.accepted_size,
            )?;
            let updated_version = self.file_version(&updated)?;
            if !decimal_version_after(&updated_version, &before_version) {
                return Err(UploadError::Uncertain);
            }

            let stale_rejected = match self.conditional_content(&checked.item, stale, &etag).await {
                Err(UploadError::Conflict) => true,
                Ok(stale) => {
                    self.check_probe_content(
                        &stale,
                        &checked.item,
                        &checked.parent,
                        &checked.name,
                        checked.stale_size,
                    )?;
                    false
                }
                Err(error) => return Err(error),
            };
            let (expected_size, expected_sha256) = if stale_rejected {
                (checked.accepted_size, checked.accepted_sha256.as_str())
            } else {
                (checked.stale_size, checked.stale_sha256.as_str())
            };
            let final_file = self.file(&checked.item).await?;
            self.check_probe_content(
                &final_file,
                &checked.item,
                &checked.parent,
                &checked.name,
                expected_size,
            )?;
            let final_version = self.file_version(&final_file)?;
            if final_version != updated_version
                && !decimal_version_after(&final_version, &updated_version)
            {
                return Err(UploadError::Uncertain);
            }
            let final_node = final_file
                .node(&scope.collection)
                .map_err(|_| UploadError::Uncertain)?;
            let bytes = self
                .read_range(
                    scope,
                    &final_node,
                    0,
                    u32::try_from(expected_size).map_err(|_| UploadError::Invalid)?,
                    cancel,
                )
                .await?;
            let final_sha256 = hex::encode(Sha256::digest(&bytes));
            if bytes.len() as u64 != expected_size || final_sha256 != expected_sha256 {
                return Err(UploadError::Uncertain);
            }
            Ok(ContentPreconditionProbe::Tested {
                item: checked.item.clone(),
                before_version,
                updated_version,
                final_version,
                stale_rejected,
                final_size: expected_size,
                final_sha256,
            })
        })
        .await
    }

    /// Build the durable description of a large conditional replacement. Both
    /// payloads must exercise the resumable path; their bytes remain outside
    /// the persisted plan.
    pub fn prepare_resumable_precondition_probe(
        &self,
        scope: &Scope,
        item: &str,
        parent: &str,
        name: &str,
        accepted: &[u8],
        stale: &[u8],
    ) -> Result<ResumablePreconditionPlan> {
        self.check_folder_destination(scope, parent, name)?;
        valid_id(item).map_err(UploadError::Provider)?;
        if accepted.len() <= 5 * 1024 * 1024
            || stale.len() <= 5 * 1024 * 1024
            || accepted.len() > u32::MAX as usize
            || stale.len() > u32::MAX as usize
        {
            return Err(UploadError::Invalid);
        }
        let accepted_sha256 = hex::encode(Sha256::digest(accepted));
        let stale_sha256 = hex::encode(Sha256::digest(stale));
        if accepted_sha256 == stale_sha256 {
            return Err(UploadError::Invalid);
        }
        Ok(ResumablePreconditionPlan {
            item: item.into(),
            parent: parent.into(),
            name: name.into(),
            accepted_size: accepted.len() as u64,
            accepted_sha256,
            stale_size: stale.len() as u64,
            stale_sha256,
        })
    }

    /// Replace one isolated test file through a resumable session, then try a
    /// second session with the now-stale ETag. A server that defers the
    /// precondition check until the final byte range is handled as well.
    pub async fn probe_resumable_precondition(
        &self,
        scope: &Scope,
        plan: &ResumablePreconditionPlan,
        accepted: Vec<u8>,
        stale: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Result<ResumablePreconditionProbe> {
        let checked = self.prepare_resumable_precondition_probe(
            scope,
            &plan.item,
            &plan.parent,
            &plan.name,
            &accepted,
            &stale,
        )?;
        if checked != *plan {
            return Err(UploadError::Invalid);
        }
        self.upload_call(cancel, Duration::from_secs(15 * 60), async {
            let (before, etag) = self.file_with_strong_etag(&checked.item).await?;
            self.check_probe_file(&before, &checked.item, &checked.parent, &checked.name)?;
            let before_version = self.file_version(&before)?;
            let Some(etag) = etag else {
                return Ok(ResumablePreconditionProbe::NoStrongEtag {
                    item: checked.item.clone(),
                    version: before_version,
                });
            };

            let session = self
                .begin_conditional_resumable(&checked.item, checked.accepted_size, &etag)
                .await?;
            let updated = self.upload_probe_session(session, accepted).await?;
            self.check_probe_content(
                &updated,
                &checked.item,
                &checked.parent,
                &checked.name,
                checked.accepted_size,
            )?;
            let updated_version = self.file_version(&updated)?;
            if !decimal_version_after(&updated_version, &before_version) {
                return Err(UploadError::Uncertain);
            }

            let stale_rejected = match self
                .begin_conditional_resumable(&checked.item, checked.stale_size, &etag)
                .await
            {
                Err(UploadError::Conflict) => true,
                Ok(session) => match self.upload_probe_session(session, stale).await {
                    Err(UploadError::Conflict) => true,
                    Ok(stale) => {
                        self.check_probe_content(
                            &stale,
                            &checked.item,
                            &checked.parent,
                            &checked.name,
                            checked.stale_size,
                        )?;
                        false
                    }
                    Err(error) => return Err(error),
                },
                Err(error) => return Err(error),
            };
            let (expected_size, expected_sha256) = if stale_rejected {
                (checked.accepted_size, checked.accepted_sha256.as_str())
            } else {
                (checked.stale_size, checked.stale_sha256.as_str())
            };
            let final_file = self.file(&checked.item).await?;
            self.check_probe_content(
                &final_file,
                &checked.item,
                &checked.parent,
                &checked.name,
                expected_size,
            )?;
            let final_version = self.file_version(&final_file)?;
            if final_version != updated_version
                && !decimal_version_after(&final_version, &updated_version)
            {
                return Err(UploadError::Uncertain);
            }
            let final_node = final_file
                .node(&scope.collection)
                .map_err(|_| UploadError::Uncertain)?;
            let final_sha256 = self.probe_sha256(scope, &final_node, cancel).await?;
            if final_sha256 != expected_sha256 {
                return Err(UploadError::Uncertain);
            }
            Ok(ResumablePreconditionProbe::Tested {
                item: checked.item.clone(),
                before_version,
                updated_version,
                final_version,
                stale_rejected,
                final_size: expected_size,
                final_sha256,
            })
        })
        .await
    }

    /// Read the exact validation-owned file together with Google's strong HTTP
    /// ETag so the provider-neutral upload journal can exercise a replacement.
    /// Ordinary Google nodes deliberately continue to expose no writable ETag.
    pub async fn validation_replacement_base(
        &self,
        scope: &Scope,
        item: &str,
        parent: &str,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<Node> {
        self.check_folder_destination(scope, parent, name)?;
        valid_id(item).map_err(UploadError::Provider)?;
        self.upload_call(cancel, Duration::from_secs(125), async {
            let (file, etag) = self.file_with_strong_etag(item).await?;
            self.check_probe_file(&file, item, parent, name)?;
            self.validation_mutation_node(file, etag)
        })
        .await
    }

    fn check_upload(&self, request: &UploadRequest) -> Result<()> {
        request.validate()?;
        self.check_scope(&request.scope)
            .map_err(|_| UploadError::Invalid)?;
        match &request.intent {
            UploadIntent::Create { parent, .. } => {
                valid_id(parent).map_err(|_| UploadError::Invalid)
            }
            UploadIntent::Replace {
                item,
                expected_etag,
            } => {
                valid_id(item).map_err(|_| UploadError::Invalid)?;
                if request.size == 0 || !valid_strong_etag(expected_etag) {
                    return Err(UploadError::Invalid);
                }
                Ok(())
            }
        }
    }

    fn prepared_checkpoint(&self, request: &UploadRequest, id: String) -> Result<SecretString> {
        valid_id(&id).map_err(|_| UploadError::Invalid)?;
        serde_json::to_string(&SavedUpload::Prepared {
            version: 1,
            request: request.clone(),
            id,
        })
        .map(SecretString::from)
        .map_err(|_| UploadError::Invalid)
    }

    fn session_checkpoint(
        &self,
        request: &UploadRequest,
        id: String,
        url: String,
        offset: u64,
    ) -> Result<UploadStep> {
        self.session_url(&url)?;
        if offset >= request.size {
            return Err(protocol("Google session offset exceeds snapshot"));
        }
        let remaining = request.size - offset;
        let length = remaining.min(u64::from(PART_SIZE)) as u32;
        if u64::from(length) < remaining && !u64::from(length).is_multiple_of(ALIGNMENT) {
            return Err(protocol("unaligned Google upload range"));
        }
        let saved = SavedUpload::Session {
            version: 1,
            request: request.clone(),
            id,
            url,
            offset,
            length,
        };
        let checkpoint = serde_json::to_string(&saved)
            .map(SecretString::from)
            .map_err(|_| UploadError::Invalid)?;
        Ok(UploadStep::Continue(UploadProgress {
            checkpoint,
            offset,
            length,
        }))
    }

    fn load_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> Result<SavedUpload> {
        if checkpoint.expose_secret().len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: SavedUpload = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        let version = match &saved {
            SavedUpload::Prepared { version, .. } | SavedUpload::Session { version, .. } => {
                *version
            }
        };
        if version != 1 || saved.request() != request || valid_id(saved.id()).is_err() {
            return Err(UploadError::CheckpointInvalid);
        }
        if let SavedUpload::Session {
            url,
            offset,
            length,
            ..
        } = &saved
        {
            self.session_url(url)
                .map_err(|_| UploadError::CheckpointInvalid)?;
            if *length == 0
                || *length > PART_SIZE
                || *offset >= request.size
                || u64::from(*length) > request.size - *offset
                || (*offset + u64::from(*length) < request.size
                    && !u64::from(*length).is_multiple_of(ALIGNMENT))
            {
                return Err(UploadError::CheckpointInvalid);
            }
        }
        Ok(saved)
    }

    fn upload_endpoint(&self) -> Url {
        let mut url = self.endpoint.clone();
        url.set_path("/upload/drive/v3/files");
        url.set_query(None);
        url.set_fragment(None);
        url
    }

    fn session_url(&self, value: &str) -> Result<Url> {
        let url = Url::parse(value).map_err(|_| protocol("invalid Google upload session URL"))?;
        let endpoint = self.upload_endpoint();
        if !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.origin() != endpoint.origin()
            || url.path() != endpoint.path()
        {
            return Err(protocol("unsafe Google upload session URL"));
        }
        Ok(url)
    }

    async fn upload_call<T>(
        &self,
        cancel: &CancellationToken,
        timeout: Duration,
        call: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let _permit = self.budget.acquire(Priority::Upload, cancel).await?;
        if let Some(until) = *self.cooldown.lock().await
            && until > Instant::now()
        {
            return Err(ProviderError::Throttled(until - Instant::now()).into());
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(UploadError::Uncertain),
            result = tokio::time::timeout(timeout, call) => {
                result.unwrap_or(Err(UploadError::Uncertain))
            }
        }
    }

    async fn authorized_json_upload(
        &self,
        method: Method,
        url: Url,
        value: &serde_json::Value,
        size: Option<u64>,
        etag: Option<&str>,
    ) -> Result<Response> {
        for attempt in 0..2 {
            let token = self.tokens.access_token().await?;
            let mut request = self
                .client
                .request(method.clone(), url.clone())
                .bearer_auth(token.expose_secret())
                .header("Accept-Encoding", "identity")
                .json(value);
            if let Some(size) = size {
                request = request
                    .header("X-Upload-Content-Type", "application/octet-stream")
                    .header("X-Upload-Content-Length", size);
            }
            if let Some(etag) = etag {
                request = request.header(reqwest::header::IF_MATCH, etag);
            }
            let response = request.send().await.map_err(|_| UploadError::Uncertain)?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.tokens.invalidate(&token).await;
                continue;
            }
            return self.upload_response(response, false).await;
        }
        Err(ProviderError::Authentication.into())
    }

    async fn file_with_strong_etag(&self, item: &str) -> Result<(File, Option<String>)> {
        let mut url = self.url(&["files", item])?;
        url.query_pairs_mut().append_pair("fields", files::FIELDS);
        let response = self.response(url, None).await?;
        let etag = strong_etag(&response);
        let bytes = body(response, MAX_UPLOAD_RESPONSE)
            .await
            .map_err(UploadError::Provider)?;
        let file = serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
        Ok((file, etag))
    }

    async fn conditional_metadata_name(&self, item: &str, name: &str, etag: &str) -> Result<File> {
        let mut url = self.url(&["files", item])?;
        url.query_pairs_mut().append_pair("fields", files::FIELDS);
        for attempt in 0..2 {
            let token = self.tokens.access_token().await?;
            let response = self
                .client
                .patch(url.clone())
                .bearer_auth(token.expose_secret())
                .header("Accept-Encoding", "identity")
                .header(reqwest::header::IF_MATCH, etag)
                .json(&json!({"name": name}))
                .send()
                .await
                .map_err(|_| UploadError::Uncertain)?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.tokens.invalidate(&token).await;
                continue;
            }
            let response = self.upload_response(response, false).await?;
            let bytes = body(response, MAX_UPLOAD_RESPONSE)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            return serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain);
        }
        Err(ProviderError::Authentication.into())
    }

    async fn conditional_content(&self, item: &str, bytes: Vec<u8>, etag: &str) -> Result<File> {
        let mut url = self.upload_endpoint();
        url.path_segments_mut()
            .map_err(|_| protocol("invalid Google upload endpoint"))?
            .push(item);
        url.query_pairs_mut()
            .append_pair("uploadType", "media")
            .append_pair("fields", files::FIELDS);
        for attempt in 0..2 {
            let token = self.tokens.access_token().await?;
            let response = self
                .client
                .patch(url.clone())
                .bearer_auth(token.expose_secret())
                .header("Accept-Encoding", "identity")
                .header(reqwest::header::IF_MATCH, etag)
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(bytes.clone())
                .send()
                .await
                .map_err(|_| UploadError::Uncertain)?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.tokens.invalidate(&token).await;
                continue;
            }
            let response = self.upload_response(response, false).await?;
            let body = body(response, MAX_UPLOAD_RESPONSE)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            return serde_json::from_slice(&body).map_err(|_| UploadError::Uncertain);
        }
        Err(ProviderError::Authentication.into())
    }

    async fn begin_conditional_resumable(&self, item: &str, size: u64, etag: &str) -> Result<Url> {
        let mut url = self.upload_endpoint();
        url.path_segments_mut()
            .map_err(|_| protocol("invalid Google upload endpoint"))?
            .push(item);
        url.query_pairs_mut()
            .append_pair("uploadType", "resumable")
            .append_pair("fields", files::FIELDS);
        for attempt in 0..2 {
            let token = self.tokens.access_token().await?;
            let response = self
                .client
                .patch(url.clone())
                .bearer_auth(token.expose_secret())
                .header("Accept-Encoding", "identity")
                .header(reqwest::header::IF_MATCH, etag)
                .header("X-Upload-Content-Type", "application/octet-stream")
                .header("X-Upload-Content-Length", size)
                .json(&json!({}))
                .send()
                .await
                .map_err(|_| UploadError::Uncertain)?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.tokens.invalidate(&token).await;
                continue;
            }
            let response = self.upload_response(response, false).await?;
            if response.status() != StatusCode::OK {
                return Err(UploadError::Uncertain);
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| protocol("Google upload session has no location"))?;
            return self.session_url(location);
        }
        Err(ProviderError::Authentication.into())
    }

    async fn upload_probe_session(&self, url: Url, bytes: Vec<u8>) -> Result<File> {
        let size = bytes.len() as u64;
        let mut offset = 0;
        while offset < size {
            let end = (offset + u64::from(PART_SIZE)).min(size);
            let range = format!("bytes {offset}-{}/{size}", end - 1);
            let response = self
                .client
                .put(url.clone())
                .header("Accept-Encoding", "identity")
                .header("Content-Length", end - offset)
                .header("Content-Range", range)
                .body(bytes[offset as usize..end as usize].to_vec())
                .send()
                .await
                .map_err(|_| UploadError::Uncertain)?;
            if response.status() == StatusCode::PERMANENT_REDIRECT {
                if end == size || Self::received_offset(&response, size)? != end {
                    return Err(UploadError::Uncertain);
                }
                offset = end;
                continue;
            }
            let response = self.upload_response(response, false).await?;
            if end != size {
                return Err(UploadError::Uncertain);
            }
            let bytes = body(response, MAX_UPLOAD_RESPONSE)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            return serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain);
        }
        Err(UploadError::Invalid)
    }

    async fn probe_sha256(
        &self,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let mut digest = Sha256::new();
        let mut offset = 0;
        while offset < node.size {
            let length = (node.size - offset).min(4 * 1024 * 1024) as u32;
            let bytes = self.read_range(scope, node, offset, length, cancel).await?;
            if bytes.len() != length as usize {
                return Err(UploadError::Uncertain);
            }
            digest.update(&bytes);
            offset += bytes.len() as u64;
        }
        Ok(hex::encode(digest.finalize()))
    }

    fn check_probe_file(&self, file: &File, item: &str, parent: &str, name: &str) -> Result<()> {
        if file.id != item
            || file.name != name
            || file.parents.as_slice() != [parent]
            || file.mime_type == FOLDER_MIME
            || file.trashed
            || file.drive_id.is_some()
        {
            return Err(UploadError::Uncertain);
        }
        self.file_version(file).map(|_| ())
    }

    fn check_probe_content(
        &self,
        file: &File,
        item: &str,
        parent: &str,
        name: &str,
        size: u64,
    ) -> Result<()> {
        self.check_probe_file(file, item, parent, name)?;
        if file.mime_type != "application/octet-stream"
            || file
                .size
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())
                != Some(size)
        {
            return Err(UploadError::Uncertain);
        }
        Ok(())
    }

    fn replacement_node(
        &self,
        request: &UploadRequest,
        id: &str,
        file: File,
        etag: Option<String>,
    ) -> Result<Node> {
        let UploadIntent::Replace { item, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if item != id
            || file.id != id
            || file.mime_type != "application/octet-stream"
            || file.parents.len() != 1
            || file.trashed
            || file.drive_id.is_some()
        {
            return Err(UploadError::Uncertain);
        }
        let mut node = file
            .node(&request.scope.collection)
            .map_err(|_| UploadError::Uncertain)?;
        node.etag = Some(etag.ok_or_else(|| protocol("missing strong Google ETag"))?);
        if node.kind != NodeKind::File || node.target.is_some() || node.content_revision().is_none()
        {
            return Err(UploadError::Uncertain);
        }
        Ok(node)
    }

    fn file_version(&self, file: &File) -> Result<String> {
        file.version
            .as_deref()
            .filter(|version| {
                !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
            })
            .map(str::to_owned)
            .ok_or_else(|| protocol("missing Google file version"))
    }

    async fn upload_response(&self, response: Response, session: bool) -> Result<Response> {
        let status = response.status();
        if matches!(status, StatusCode::OK | StatusCode::CREATED)
            || (session && status == StatusCode::PERMANENT_REDIRECT)
        {
            return Ok(response);
        }
        match status {
            status
                if session
                    && status.is_client_error()
                    && status != StatusCode::TOO_MANY_REQUESTS =>
            {
                Err(UploadError::SessionGone)
            }
            StatusCode::UNAUTHORIZED => Err(ProviderError::Authentication.into()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound.into()),
            StatusCode::PRECONDITION_FAILED => Err(UploadError::Conflict),
            StatusCode::CONFLICT => Err(UploadError::Uncertain),
            StatusCode::BAD_REQUEST => Err(UploadError::Invalid),
            StatusCode::FORBIDDEN => {
                let bytes = body(response, 64 * 1024)
                    .await
                    .map_err(UploadError::Provider)?;
                #[derive(Deserialize)]
                struct Reason {
                    reason: String,
                }
                #[derive(Deserialize)]
                struct Detail {
                    #[serde(default)]
                    errors: Vec<Reason>,
                }
                #[derive(Deserialize)]
                struct ErrorBody {
                    error: Detail,
                }
                let quota = serde_json::from_slice::<ErrorBody>(&bytes)
                    .ok()
                    .is_some_and(|error| {
                        error.error.errors.iter().any(|error| {
                            matches!(
                                error.reason.as_str(),
                                "rateLimitExceeded"
                                    | "userRateLimitExceeded"
                                    | "storageQuotaExceeded"
                            )
                        })
                    });
                Err(if quota {
                    UploadError::Quota
                } else {
                    ProviderError::Permission.into()
                })
            }
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::from_secs(30))
                    .clamp(Duration::from_secs(1), Duration::from_secs(86400));
                let until = Instant::now() + delay;
                let mut cooldown = self.cooldown.lock().await;
                *cooldown = Some(cooldown.map_or(until, |old| old.max(until)));
                Err(ProviderError::Throttled(delay).into())
            }
            status if status.is_server_error() => Err(UploadError::Uncertain),
            _ => Err(UploadError::UnexpectedStatus(status.as_u16())),
        }
    }

    async fn generated_id(&self) -> Result<String> {
        let mut url = self.url(&["files", "generateIds"])?;
        url.query_pairs_mut()
            .extend_pairs([("count", "1"), ("space", "drive"), ("type", "files")]);
        let generated: GeneratedIds = self.json(url).await?;
        if generated.kind != "drive#generatedIds"
            || generated.space != "drive"
            || generated.ids.len() != 1
        {
            return Err(protocol("invalid Google generated IDs response"));
        }
        let id = generated
            .ids
            .into_iter()
            .next()
            .ok_or_else(|| protocol("invalid Google generated IDs response"))?;
        valid_id(&id).map_err(UploadError::Provider)?;
        Ok(id)
    }

    fn create_metadata(request: &UploadRequest, id: &str) -> serde_json::Value {
        let UploadIntent::Create { parent, name } = &request.intent else {
            unreachable!("checked before metadata construction")
        };
        json!({
            "id": id,
            "name": name,
            "parents": [parent],
            "mimeType": "application/octet-stream"
        })
    }

    async fn begin_session(&self, request: &UploadRequest, id: &str) -> Result<UploadStep> {
        let mut url = self.upload_endpoint();
        let (method, metadata, etag) = match &request.intent {
            UploadIntent::Create { .. } => (Method::POST, Self::create_metadata(request, id), None),
            UploadIntent::Replace {
                item,
                expected_etag,
            } => {
                if item != id {
                    return Err(UploadError::Invalid);
                }
                url.path_segments_mut()
                    .map_err(|_| protocol("invalid Google upload endpoint"))?
                    .push(item);
                (Method::PATCH, json!({}), Some(expected_etag.as_str()))
            }
        };
        url.query_pairs_mut()
            .append_pair("uploadType", "resumable")
            .append_pair("fields", files::FIELDS);
        let response = self
            .authorized_json_upload(method, url, &metadata, Some(request.size), etag)
            .await?;
        if response.status() != StatusCode::OK {
            return Err(UploadError::Uncertain);
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| protocol("Google upload session has no location"))?;
        self.session_checkpoint(request, id.to_owned(), location.to_owned(), 0)
    }

    async fn create_empty(&self, request: &UploadRequest, id: &str) -> Result<UploadStep> {
        let mut url = self.url(&["files"])?;
        url.query_pairs_mut().append_pair("fields", files::FIELDS);
        let response = self
            .authorized_json_upload(
                Method::POST,
                url,
                &Self::create_metadata(request, id),
                None,
                None,
            )
            .await?;
        Ok(UploadStep::Complete(
            self.receipt(request, id, response).await?,
        ))
    }

    async fn receipt(&self, request: &UploadRequest, id: &str, response: Response) -> Result<Node> {
        let etag = strong_etag(&response);
        let bytes = body(response, MAX_UPLOAD_RESPONSE)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        let file: File = serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
        let node = match &request.intent {
            UploadIntent::Create { .. } => self.receipt_from_file(request, id, file)?,
            UploadIntent::Replace { .. } => self.replacement_node(request, id, file, etag)?,
        };
        if node.size != request.size {
            return Err(UploadError::Uncertain);
        }
        Ok(node)
    }

    fn receipt_from_file(&self, request: &UploadRequest, id: &str, file: File) -> Result<Node> {
        if file.id != id
            || file.mime_type != "application/octet-stream"
            || file
                .size
                .as_deref()
                .and_then(|size| size.parse::<u64>().ok())
                != Some(request.size)
        {
            return Err(UploadError::Uncertain);
        }
        let mut node = file
            .node(&request.scope.collection)
            .map_err(|_| UploadError::Uncertain)?;
        match &request.intent {
            UploadIntent::Create { parent, name } => {
                if &file.name != name
                    || file.parents.len() != 1
                    || file.parents.first() != Some(parent)
                {
                    return Err(UploadError::Uncertain);
                }
                // The upload journal confirms the requested provider name. The
                // read-only projection adds an ID suffix at the namespace edge.
                node.name = name.clone();
            }
            UploadIntent::Replace { item, .. } if item == id => {}
            UploadIntent::Replace { .. } => return Err(UploadError::Invalid),
        }
        if node.kind != NodeKind::File || node.content_revision().is_none() {
            return Err(UploadError::Uncertain);
        }
        Ok(node)
    }

    fn received_offset(response: &Response, size: u64) -> Result<u64> {
        let Some(range) = response
            .headers()
            .get("range")
            .and_then(|value| value.to_str().ok())
        else {
            return Ok(0);
        };
        let end = range
            .strip_prefix("bytes=0-")
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| protocol("invalid Google upload status range"))?;
        let offset = end
            .checked_add(1)
            .ok_or_else(|| protocol("invalid Google upload status range"))?;
        if offset >= size {
            return Err(protocol("invalid Google upload status offset"));
        }
        Ok(offset)
    }

    async fn session_request(
        &self,
        method: Method,
        url: Url,
        range: String,
        bytes: Vec<u8>,
        conditional: bool,
    ) -> Result<Response> {
        let response = self
            .client
            .request(method, url)
            .header("Accept-Encoding", "identity")
            .header("Content-Length", bytes.len())
            .header("Content-Range", range)
            .body(bytes)
            .send()
            .await
            .map_err(|_| UploadError::Uncertain)?;
        if conditional && response.status() == StatusCode::PRECONDITION_FAILED {
            return Err(UploadError::Conflict);
        }
        self.upload_response(response, true).await
    }

    async fn inspect_prepared(
        &self,
        request: &UploadRequest,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        match &request.intent {
            UploadIntent::Create { .. } => match self.file(id).await {
                Ok(file) => Ok(UploadStep::Complete(
                    self.receipt_from_file(request, id, file)?,
                )),
                Err(ProviderError::NotFound) if request.size == 0 => {
                    self.create_empty(request, id).await
                }
                Err(ProviderError::NotFound) => self.begin_session(request, id).await,
                Err(error) => Err(error.into()),
            },
            UploadIntent::Replace { expected_etag, .. } => {
                let (file, etag) = self.file_with_strong_etag(id).await?;
                let node = self.replacement_node(request, id, file, etag)?;
                if node.etag.as_deref() == Some(expected_etag) {
                    return self.begin_session(request, id).await;
                }
                if node.size == request.size
                    && self.probe_sha256(&request.scope, &node, cancel).await? == request.sha256
                {
                    Ok(UploadStep::Complete(node))
                } else {
                    Err(UploadError::Conflict)
                }
            }
        }
    }

    fn validation_mutation_node(&self, file: File, etag: Option<String>) -> Result<Node> {
        if file.mime_type != "application/octet-stream"
            || file.parents.len() != 1
            || file.trashed
            || file.drive_id.is_some()
        {
            return Err(UploadError::Uncertain);
        }
        let raw_name = file.name.clone();
        let mut node = file
            .node(&self.collection)
            .map_err(|_| UploadError::Uncertain)?;
        node.name = raw_name;
        node.etag = Some(etag.ok_or_else(|| protocol("missing strong Google ETag"))?);
        if node.kind != NodeKind::File || node.target.is_some() || node.content_revision().is_none()
        {
            return Err(UploadError::Uncertain);
        }
        Ok(node)
    }

    async fn validation_destination_free(
        &self,
        parent: &str,
        name: &str,
        item: &str,
    ) -> Result<()> {
        let folded = name.to_lowercase();
        let mut token = None;
        loop {
            let page = self.list_files(Some(parent), token.as_deref()).await?;
            if page
                .files
                .iter()
                .any(|file| file.id != item && !file.trashed && file.name.to_lowercase() == folded)
            {
                return Err(UploadError::Conflict);
            }
            let Some(next) = page.next_page_token else {
                return Ok(());
            };
            token = Some(next);
        }
    }

    async fn validation_relocate(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> Result<MutationReceipt> {
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &request.intent
        else {
            return Err(UploadError::Unsupported(
                "Google validation adapter only relocates regular files",
            ));
        };
        self.upload_call(cancel, Duration::from_secs(125), async {
            let (file, etag) = self.file_with_strong_etag(&before.id).await?;
            let current = self.validation_mutation_node(file, etag)?;
            if current.etag != before.etag
                || current.name != before.name
                || current.parent_id != before.parent_id
            {
                return Err(UploadError::Conflict);
            }
            self.validation_destination_free(parent, name, &before.id)
                .await?;

            let mut url = self.url(&["files", &before.id])?;
            url.query_pairs_mut().append_pair("fields", files::FIELDS);
            if current.parent_id.as_deref() != Some(parent) {
                url.query_pairs_mut()
                    .append_pair("addParents", parent)
                    .append_pair(
                        "removeParents",
                        current.parent_id.as_deref().ok_or(UploadError::Invalid)?,
                    );
            }
            let response = self
                .authorized_json_upload(
                    Method::PATCH,
                    url,
                    &json!({"name": name}),
                    None,
                    before.etag.as_deref(),
                )
                .await?;
            let response_etag = strong_etag(&response);
            let bytes = body(response, MAX_UPLOAD_RESPONSE)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            let file: File = serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
            let receipt =
                MutationReceipt::Upsert(self.validation_mutation_node(file, response_etag)?);
            if request.accepts(&receipt) {
                Ok(receipt)
            } else {
                Err(UploadError::Uncertain)
            }
        })
        .await
    }

    async fn validation_remove_file(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> Result<MutationReceipt> {
        let MutationIntent::RemoveFile { before } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        self.upload_call(cancel, Duration::from_secs(125), async {
            let (file, etag) = self.file_with_strong_etag(&before.id).await?;
            let current = self.validation_mutation_node(file, etag)?;
            if current.etag != before.etag
                || current.name != before.name
                || current.parent_id != before.parent_id
            {
                return Err(UploadError::Conflict);
            }
            let mut url = self.url(&["files", &before.id])?;
            url.query_pairs_mut().append_pair("fields", files::FIELDS);
            let response = self
                .authorized_json_upload(
                    Method::PATCH,
                    url,
                    &json!({"trashed": true}),
                    None,
                    before.etag.as_deref(),
                )
                .await?;
            let bytes = body(response, MAX_UPLOAD_RESPONSE)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            let file: File = serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
            if file.id != before.id
                || !file.trashed
                || file.mime_type != "application/octet-stream"
                || file.drive_id.is_some()
            {
                return Err(UploadError::Uncertain);
            }
            Ok(MutationReceipt::Removed {
                item: before.id.clone(),
            })
        })
        .await
    }
}

fn mutation_error(error: UploadError) -> MutationError {
    match error {
        UploadError::Provider(error) => error.into(),
        UploadError::Conflict => MutationError::Conflict,
        UploadError::Quota => MutationError::Quota,
        UploadError::Locked => MutationError::Locked,
        UploadError::Invalid | UploadError::CheckpointInvalid => MutationError::Invalid,
        UploadError::Unsupported(message) => MutationError::Unsupported(message),
        _ => MutationError::Uncertain,
    }
}

#[async_trait]
impl MutationProvider for GoogleValidationMutations {
    fn deletion(&self) -> cirrove_core::mutation::DeletionSupport {
        cirrove_core::mutation::DeletionSupport {
            recycle_bin: true,
            permanent: false,
        }
    }

    async fn prepare_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> cirrove_core::mutation::Result<Option<String>> {
        request.validate()?;
        self.drive
            .check_scope(&request.scope)
            .map_err(MutationError::Provider)?;
        match &request.intent {
            MutationIntent::CreateFolder { parent, name } => self
                .drive
                .prepare_folder(&request.scope, parent, name, cancel)
                .await
                .map(|plan| Some(plan.id))
                .map_err(mutation_error),
            _ => Ok(None),
        }
    }

    async fn mutate(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        request.validate()?;
        self.drive
            .check_scope(&request.scope)
            .map_err(MutationError::Provider)?;
        let before = request.intent.before().ok_or(MutationError::Unsupported(
            "Google validation folder creation is prepared separately",
        ))?;
        valid_id(&before.id).map_err(|_| MutationError::Invalid)?;
        if before.kind != NodeKind::File || !before.etag.as_deref().is_some_and(valid_strong_etag) {
            return Err(MutationError::Invalid);
        }
        match &request.intent {
            MutationIntent::Relocate { parent, name, .. } => {
                valid_id(parent).map_err(|_| MutationError::Invalid)?;
                if name.is_empty()
                    || name.len() > 4096
                    || matches!(name.as_str(), "." | "..")
                    || name.contains(['/', '\0'])
                {
                    return Err(MutationError::Invalid);
                }
                self.drive.validation_relocate(request, cancel).await
            }
            MutationIntent::RemoveFile { .. } => {
                self.drive.validation_remove_file(request, cancel).await
            }
            MutationIntent::CreateFolder { .. } | MutationIntent::RemoveFolder { .. } => Err(
                UploadError::Unsupported("Google validation adapter does not mutate folders"),
            ),
        }
        .map_err(mutation_error)
    }

    async fn mutate_prepared(
        &self,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        if let MutationIntent::CreateFolder { parent, name } = &request.intent {
            request.validate()?;
            self.drive
                .check_scope(&request.scope)
                .map_err(MutationError::Provider)?;
            let id = prepared_item.ok_or(MutationError::Invalid)?;
            valid_id(id).map_err(|_| MutationError::Invalid)?;
            let plan = PreparedFolder {
                id: id.into(),
                parent: parent.clone(),
                name: name.clone(),
            };
            self.drive
                .validation_destination_free(parent, name, id)
                .await
                .map_err(mutation_error)?;
            let receipt = MutationReceipt::Upsert(
                self.drive
                    .create_prepared_folder(&request.scope, &plan, cancel)
                    .await
                    .map_err(mutation_error)?,
            );
            return if request.accepts(&receipt) {
                Ok(receipt)
            } else {
                Err(MutationError::Uncertain)
            };
        }
        if prepared_item.is_some() {
            return Err(MutationError::Invalid);
        }
        self.mutate(request, cancel).await
    }

    async fn reconcile_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        request.validate()?;
        self.drive
            .check_scope(&request.scope)
            .map_err(MutationError::Provider)?;
        let Some(before) = request.intent.before() else {
            return Ok(MutationReconciliation::Indeterminate);
        };
        let result = self
            .drive
            .upload_call(cancel, Duration::from_secs(125), async {
                let (file, etag) = self.drive.file_with_strong_etag(&before.id).await?;
                Ok((file, etag))
            })
            .await;
        let (file, etag) = match result {
            Ok(value) => value,
            Err(UploadError::Provider(ProviderError::NotFound)) => {
                return Ok(MutationReconciliation::Indeterminate);
            }
            Err(error) => return Err(mutation_error(error)),
        };
        if matches!(request.intent, MutationIntent::RemoveFile { .. }) && file.trashed {
            if file.id != before.id || file.drive_id.is_some() {
                return Err(MutationError::Uncertain);
            }
            return Ok(MutationReconciliation::Applied(MutationReceipt::Removed {
                item: before.id.clone(),
            }));
        }
        let current = self
            .drive
            .validation_mutation_node(file, etag)
            .map_err(mutation_error)?;
        if let MutationIntent::Relocate { parent, name, .. } = &request.intent {
            let receipt = MutationReceipt::Upsert(current.clone());
            if current.name == *name && current.parent_id.as_ref() == Some(parent) {
                return if request.accepts(&receipt) {
                    Ok(MutationReconciliation::Applied(receipt))
                } else {
                    Err(MutationError::Uncertain)
                };
            }
        }
        if current.etag == before.etag
            && current.name == before.name
            && current.parent_id == before.parent_id
            && current.kind == before.kind
            && current.target == before.target
        {
            Ok(MutationReconciliation::Uncommitted)
        } else {
            Ok(MutationReconciliation::Conflict)
        }
    }

    async fn reconcile_prepared_mutation(
        &self,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        if let MutationIntent::CreateFolder { parent, name } = &request.intent {
            request.validate()?;
            self.drive
                .check_scope(&request.scope)
                .map_err(MutationError::Provider)?;
            let id = prepared_item.ok_or(MutationError::Invalid)?;
            valid_id(id).map_err(|_| MutationError::Invalid)?;
            let plan = PreparedFolder {
                id: id.into(),
                parent: parent.clone(),
                name: name.clone(),
            };
            return match self
                .drive
                .inspect_prepared_folder(&request.scope, &plan, cancel)
                .await
                .map_err(mutation_error)?
            {
                Some(node) => {
                    let receipt = MutationReceipt::Upsert(node);
                    if request.accepts(&receipt) {
                        Ok(MutationReconciliation::Applied(receipt))
                    } else {
                        Err(MutationError::Uncertain)
                    }
                }
                None => Ok(MutationReconciliation::Uncommitted),
            };
        }
        if prepared_item.is_some() {
            return Err(MutationError::Invalid);
        }
        self.reconcile_mutation(request, cancel).await
    }
}

#[async_trait]
impl UploadProvider for GoogleDrive {
    async fn begin_upload(
        &self,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        match &request.intent {
            UploadIntent::Create { .. } => {
                self.upload_call(cancel, Duration::from_secs(125), async {
                    let id = self.generated_id().await?;
                    Ok(UploadStep::Prepared(self.prepared_checkpoint(request, id)?))
                })
                .await
            }
            UploadIntent::Replace { item, .. } => Ok(UploadStep::Prepared(
                self.prepared_checkpoint(request, item.clone())?,
            )),
        }
    }

    async fn inspect_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        let saved = self.load_upload(request, checkpoint)?;
        self.upload_call(cancel, Duration::from_secs(125), async {
            match saved {
                SavedUpload::Prepared { id, .. } => {
                    self.inspect_prepared(request, &id, cancel).await
                }
                SavedUpload::Session { id, url, .. } => {
                    let result = self
                        .session_request(
                            Method::PUT,
                            self.session_url(&url)?,
                            format!("bytes */{}", request.size),
                            Vec::new(),
                            matches!(request.intent, UploadIntent::Replace { .. }),
                        )
                        .await;
                    match result {
                        Ok(response) if response.status() == StatusCode::PERMANENT_REDIRECT => {
                            let offset = Self::received_offset(&response, request.size)?;
                            self.session_checkpoint(request, id, url, offset)
                        }
                        Ok(response) => Ok(UploadStep::Complete(
                            self.receipt(request, &id, response).await?,
                        )),
                        Err(UploadError::SessionGone) => {
                            Ok(UploadStep::Prepared(self.prepared_checkpoint(request, id)?))
                        }
                        Err(error) => Err(error),
                    }
                }
            }
        })
        .await
    }

    async fn upload_part(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        let saved = self.load_upload(request, checkpoint)?;
        let SavedUpload::Session {
            id,
            url,
            offset: expected,
            length,
            ..
        } = saved
        else {
            return Err(UploadError::CheckpointInvalid);
        };
        if offset != expected || bytes.len() != length as usize {
            return Err(UploadError::Invalid);
        }
        let end = offset + bytes.len() as u64;
        self.upload_call(cancel, Duration::from_secs(125), async {
            let result = self
                .session_request(
                    Method::PUT,
                    self.session_url(&url)?,
                    format!("bytes {offset}-{}/{}", end - 1, request.size),
                    bytes,
                    matches!(request.intent, UploadIntent::Replace { .. }),
                )
                .await;
            match result {
                Ok(response) if response.status() == StatusCode::PERMANENT_REDIRECT => {
                    let next = Self::received_offset(&response, request.size)?;
                    if next != end {
                        return Err(UploadError::Uncertain);
                    }
                    self.session_checkpoint(request, id, url, next)
                }
                Ok(response) => Ok(UploadStep::Complete(
                    self.receipt(request, &id, response).await?,
                )),
                Err(UploadError::SessionGone) => {
                    Ok(UploadStep::Prepared(self.prepared_checkpoint(request, id)?))
                }
                Err(error) => Err(error),
            }
        })
        .await
    }

    async fn commit_upload(
        &self,
        request: &UploadRequest,
        _checkpoint: &SecretString,
        _cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        Err(UploadError::Unsupported(
            "Google uploads commit with their final byte range",
        ))
    }

    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
        cancel: &CancellationToken,
    ) -> Result<Reconciliation> {
        self.check_upload(request)?;
        let Some(checkpoint) = checkpoint else {
            // No mutation is permitted before a Prepared checkpoint is durable.
            return Ok(Reconciliation::Uncommitted);
        };
        let saved = self.load_upload(request, checkpoint)?;
        let id = saved.id().to_owned();
        self.upload_call(cancel, Duration::from_secs(15 * 60), async {
            let (file, etag) = match &request.intent {
                UploadIntent::Create { .. } => match self.file(&id).await {
                    Ok(file) => (file, None),
                    Err(ProviderError::NotFound)
                        if matches!(saved, SavedUpload::Prepared { .. }) =>
                    {
                        return Ok(Reconciliation::Uncommitted);
                    }
                    Err(ProviderError::NotFound) => return Err(UploadError::Uncertain),
                    Err(error) => return Err(error.into()),
                },
                UploadIntent::Replace { .. } => match self.file_with_strong_etag(&id).await {
                    Ok(result) => result,
                    Err(UploadError::Provider(ProviderError::NotFound)) => {
                        return Ok(Reconciliation::Conflict);
                    }
                    Err(error) => return Err(error),
                },
            };
            let node = match &request.intent {
                UploadIntent::Create { .. } => self.receipt_from_file(request, &id, file),
                UploadIntent::Replace { .. } => self.replacement_node(request, &id, file, etag),
            };
            let node = match node {
                Ok(node) => node,
                Err(_) => return Ok(Reconciliation::Conflict),
            };
            if node.size != request.size {
                return match &request.intent {
                    UploadIntent::Replace { expected_etag, .. }
                        if node.etag.as_deref() == Some(expected_etag) =>
                    {
                        Ok(Reconciliation::Uncommitted)
                    }
                    _ => Ok(Reconciliation::Conflict),
                };
            }
            let mut digest = Sha256::new();
            let mut offset = 0;
            while offset < node.size {
                let bytes = self
                    .read_range(&request.scope, &node, offset, 4 * 1024 * 1024, cancel)
                    .await?;
                if bytes.is_empty() {
                    return Err(UploadError::Uncertain);
                }
                offset += bytes.len() as u64;
                digest.update(bytes);
            }
            if hex::encode(digest.finalize()) == request.sha256 {
                Ok(Reconciliation::Committed(node))
            } else {
                match &request.intent {
                    UploadIntent::Replace { expected_etag, .. }
                        if node.etag.as_deref() == Some(expected_etag) =>
                    {
                        Ok(Reconciliation::Uncommitted)
                    }
                    _ => Ok(Reconciliation::Conflict),
                }
            }
        })
        .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cirrove_core::upload::UploadProvider;
    use serde_json::Value;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    enum ExpectedBody {
        Json(Value),
        Bytes(Vec<u8>),
    }
    struct Exchange {
        method: &'static str,
        path: &'static str,
        query: Vec<(&'static str, &'static str)>,
        headers: Vec<String>,
        authorized: bool,
        body: Option<ExpectedBody>,
        status: u16,
        response_headers: String,
        response_body: Vec<u8>,
    }
    impl Exchange {
        fn json(method: &'static str, path: &'static str, status: u16, body: Value) -> Self {
            Self {
                method,
                path,
                query: vec![],
                headers: vec![],
                authorized: true,
                body: None,
                status,
                response_headers: String::new(),
                response_body: serde_json::to_vec(&body).unwrap(),
            }
        }
    }

    fn scope() -> Scope {
        Scope {
            account: "account".into(),
            provider: PROVIDER_ID.into(),
            collection: "root-id".into(),
        }
    }
    fn request(bytes: &[u8]) -> UploadRequest {
        UploadRequest {
            scope: scope(),
            intent: UploadIntent::Create {
                parent: "root-id".into(),
                name: "report.txt".into(),
            },
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        }
    }
    fn named_file(id: &str, name: &str, version: &str, size: usize) -> Value {
        named_file_at(id, "root-id", name, version, size)
    }
    fn named_file_at(id: &str, parent: &str, name: &str, version: &str, size: usize) -> Value {
        json!({
            "id": id,
            "name": name,
            "mimeType": "application/octet-stream",
            "parents": [parent],
            "size": size.to_string(),
            "version": version,
            "capabilities": {"canDownload": true}
        })
    }
    fn mutation_request(parent: &str, name: &str, etag: &str) -> MutationRequest {
        MutationRequest {
            scope: scope(),
            intent: MutationIntent::Relocate {
                before: Node {
                    id: "generated-id".into(),
                    parent_id: Some("root-id".into()),
                    name: "report.txt".into(),
                    kind: NodeKind::File,
                    size: 6,
                    modified_unix: 0,
                    etag: Some(etag.into()),
                    content_version: Some("google-version:9".into()),
                    target: None,
                    package: false,
                },
                parent: parent.into(),
                name: name.into(),
            },
        }
    }
    fn removal_request(etag: &str) -> MutationRequest {
        let MutationIntent::Relocate { before, .. } =
            mutation_request("root-id", "unused.txt", etag).intent
        else {
            unreachable!()
        };
        MutationRequest {
            scope: scope(),
            intent: MutationIntent::RemoveFile { before },
        }
    }
    fn file(id: &str, size: usize) -> Value {
        named_file(id, "report.txt", "7", size)
    }
    fn metadata(id: &str) -> Value {
        json!({
            "id": id,
            "name": "report.txt",
            "parents": ["root-id"],
            "mimeType": "application/octet-stream"
        })
    }
    fn folder(id: &str) -> Value {
        json!({
            "id": id,
            "name": "Cirrove-Create-Validation",
            "mimeType": FOLDER_MIME,
            "parents": ["root-id"],
            "version": "3"
        })
    }

    async fn fixture(
        build: impl FnOnce(std::net::SocketAddr) -> Vec<Exchange>,
    ) -> (GoogleDrive, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let exchanges = build(address);
        let task = tokio::spawn(async move {
            for exchange in exchanges {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut bytes = [0; 4096];
                    let read = socket.read(&mut bytes).await.unwrap();
                    assert!(read > 0);
                    request.extend_from_slice(&bytes[..read]);
                    assert!(request.len() < 16 * 1024 * 1024);
                    if let Some(position) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        break position + 4;
                    }
                };
                let head = String::from_utf8(request[..header_end].to_vec()).unwrap();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() - header_end < content_length {
                    let mut bytes = [0; 4096];
                    let read = socket.read(&mut bytes).await.unwrap();
                    assert!(read > 0);
                    request.extend_from_slice(&bytes[..read]);
                }
                let mut words = head.lines().next().unwrap().split_ascii_whitespace();
                assert_eq!(words.next(), Some(exchange.method));
                let target = words.next().unwrap();
                let url = Url::parse(&format!("http://fixture{target}")).unwrap();
                assert_eq!(url.path(), exchange.path);
                let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
                for (name, value) in exchange.query {
                    assert_eq!(query.get(name).map(|value| value.as_ref()), Some(value));
                }
                let lower = head.to_ascii_lowercase();
                assert_eq!(
                    lower.contains("authorization: bearer synthetic-token"),
                    exchange.authorized
                );
                for header in exchange.headers {
                    assert!(
                        lower.contains(&header.to_ascii_lowercase()),
                        "missing {header}"
                    );
                }
                let request_body = &request[header_end..header_end + content_length];
                match exchange.body {
                    Some(ExpectedBody::Json(expected)) => {
                        assert_eq!(
                            serde_json::from_slice::<Value>(request_body).unwrap(),
                            expected
                        )
                    }
                    Some(ExpectedBody::Bytes(expected)) => assert_eq!(request_body, expected),
                    None => assert!(request_body.is_empty()),
                }
                let reply = format!(
                    "HTTP/1.1 {} Synthetic\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                    exchange.status,
                    exchange.response_body.len(),
                    exchange.response_headers
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
                socket.write_all(&exchange.response_body).await.unwrap();
            }
        });
        (
            GoogleDrive::build(
                "account".into(),
                "root-id".into(),
                Arc::new(cirrove_core::StaticToken(SecretString::from(
                    "synthetic-token",
                ))),
                Url::parse(&format!("http://{address}/drive/v3/")).unwrap(),
            )
            .unwrap(),
            task,
        )
    }

    #[tokio::test]
    async fn generated_identity_is_persisted_before_session_and_used_for_exact_create() {
        let bytes = b"abcdef";
        let generated = "generated-id";
        let (provider, server) = fixture(|address| {
            let mut generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            generate.query = vec![("count", "1"), ("space", "drive"), ("type", "files")];
            let missing = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            let mut start = Exchange::json("POST", "/upload/drive/v3/files", 200, json!({}));
            start.query = vec![("uploadType", "resumable"), ("fields", files::FIELDS)];
            start.headers = vec![
                "x-upload-content-type: application/octet-stream".into(),
                "x-upload-content-length: 6".into(),
            ];
            start.body = Some(ExpectedBody::Json(metadata(generated)));
            start.response_body.clear();
            start.response_headers = format!(
                "Location: http://{address}/upload/drive/v3/files?upload_id=PRIVATE-SESSION\r\n"
            );
            let mut upload = Exchange::json(
                "PUT",
                "/upload/drive/v3/files",
                201,
                file(generated, bytes.len()),
            );
            upload.query = vec![("upload_id", "PRIVATE-SESSION")];
            upload.authorized = false;
            upload.headers = vec!["content-range: bytes 0-5/6".into()];
            upload.body = Some(ExpectedBody::Bytes(bytes.to_vec()));
            vec![generate, missing, start, upload]
        })
        .await;
        let request = request(bytes);
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("generated ID was not prepared")
        };
        assert!(prepared.expose_secret().contains(generated));
        assert!(!format!("{:?}", UploadStep::Prepared(prepared.clone())).contains(generated));
        let UploadStep::Continue(progress) = provider
            .inspect_upload(&request, &prepared, &cancel)
            .await
            .unwrap()
        else {
            panic!("session did not request content")
        };
        assert_eq!((progress.offset, progress.length), (0, 6));
        let UploadStep::Complete(node) = provider
            .upload_part(&request, &progress.checkpoint, 0, bytes.to_vec(), &cancel)
            .await
            .unwrap()
        else {
            panic!("final range was not acknowledged")
        };
        assert_eq!(node.id, generated);
        assert_eq!(node.name, "report.txt");
        assert_eq!(node.parent_id.as_deref(), Some("root-id"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_validation_folder_is_prepared_before_create_and_inspected_by_exact_id() {
        let generated = "generated-folder-id";
        let (provider, server) = fixture(|_| {
            let mut generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            generate.query = vec![("count", "1"), ("space", "drive"), ("type", "files")];
            let mut create = Exchange::json("POST", "/drive/v3/files", 200, folder(generated));
            create.query = vec![("fields", files::FIELDS)];
            create.response_headers = "ETag: \"folder-3\"\r\n".into();
            create.body = Some(ExpectedBody::Json(json!({
                "id": generated,
                "name": "Cirrove-Create-Validation",
                "parents": ["root-id"],
                "mimeType": FOLDER_MIME
            })));
            let mut inspect = Exchange::json(
                "GET",
                "/drive/v3/files/generated-folder-id",
                200,
                folder(generated),
            );
            inspect.response_headers = "ETag: \"folder-3\"\r\n".into();
            vec![generate, create, inspect]
        })
        .await;
        let cancel = CancellationToken::new();
        let plan = provider
            .prepare_folder(&scope(), "root-id", "Cirrove-Create-Validation", &cancel)
            .await
            .unwrap();
        assert_eq!(plan.id(), generated);
        let created = provider
            .create_prepared_folder(&scope(), &plan, &cancel)
            .await
            .unwrap();
        assert_eq!(created.id, generated);
        assert_eq!(created.name, "Cirrove-Create-Validation");
        let inspected = provider
            .inspect_prepared_folder(&scope(), &plan, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(inspected.id, generated);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_folder_creation_uses_the_prepared_mutation_identity() {
        let generated = "generated-folder-id";
        let (provider, server) = fixture(|_| {
            let mut generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            generate.query = vec![("count", "1"), ("space", "drive"), ("type", "files")];
            let mut scan = Exchange::json("GET", "/drive/v3/files", 200, json!({"files":[]}));
            scan.query = vec![("q", "'root-id' in parents and trashed = false")];
            let mut create = Exchange::json("POST", "/drive/v3/files", 200, folder(generated));
            create.query = vec![("fields", files::FIELDS)];
            create.response_headers = "ETag: \"folder-3\"\r\n".into();
            create.body = Some(ExpectedBody::Json(json!({
                "id": generated,
                "name": "Cirrove-Create-Validation",
                "parents": ["root-id"],
                "mimeType": FOLDER_MIME
            })));
            let mut inspect = Exchange::json(
                "GET",
                "/drive/v3/files/generated-folder-id",
                200,
                folder(generated),
            );
            inspect.response_headers = "ETag: \"folder-3\"\r\n".into();
            vec![generate, scan, create, inspect]
        })
        .await;
        let adapter = provider.validation_mutations();
        let cancel = CancellationToken::new();
        let request = MutationRequest {
            scope: scope(),
            intent: MutationIntent::CreateFolder {
                parent: "root-id".into(),
                name: "Cirrove-Create-Validation".into(),
            },
        };
        let prepared = adapter
            .prepare_mutation(&request, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(prepared, generated);
        let MutationReceipt::Upsert(created) = adapter
            .mutate_prepared(&request, Some(&prepared), &cancel)
            .await
            .unwrap()
        else {
            panic!("expected folder receipt")
        };
        assert_eq!(created.id, generated);
        assert_eq!(created.etag.as_deref(), Some("\"folder-3\""));
        assert!(matches!(
            adapter
                .reconcile_prepared_mutation(&request, Some(&prepared), &cancel)
                .await
                .unwrap(),
            MutationReconciliation::Applied(MutationReceipt::Upsert(node)) if node.id == generated
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_folder_creation_refuses_an_occupied_destination_before_post() {
        let generated = "generated-folder-id";
        let (provider, server) = fixture(|_| {
            let mut generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            generate.query = vec![("count", "1"), ("space", "drive"), ("type", "files")];
            let mut occupied = folder("occupied-folder-id");
            occupied["name"] = json!("cirrove-create-validation");
            let mut scan =
                Exchange::json("GET", "/drive/v3/files", 200, json!({"files":[occupied]}));
            scan.query = vec![("q", "'root-id' in parents and trashed = false")];
            vec![generate, scan]
        })
        .await;
        let adapter = provider.validation_mutations();
        let cancel = CancellationToken::new();
        let request = MutationRequest {
            scope: scope(),
            intent: MutationIntent::CreateFolder {
                parent: "root-id".into(),
                name: "Cirrove-Create-Validation".into(),
            },
        };
        let prepared = adapter
            .prepare_mutation(&request, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            adapter
                .mutate_prepared(&request, Some(&prepared), &cancel)
                .await,
            Err(MutationError::Conflict)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn metadata_probe_updates_once_and_rejects_the_now_stale_etag() {
        let (provider, server) = fixture(|_| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "9", 6),
            );
            before.query = vec![("fields", files::FIELDS)];
            before.response_headers = "ETag: \"metadata-9\"\r\n".into();
            let mut update = Exchange::json(
                "PATCH",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "renamed.txt", "10", 6),
            );
            update.query = vec![("fields", files::FIELDS)];
            update.headers = vec!["if-match: \"metadata-9\"".into()];
            update.body = Some(ExpectedBody::Json(json!({"name":"renamed.txt"})));
            let mut stale = Exchange::json("PATCH", "/drive/v3/files/generated-id", 412, json!({}));
            stale.query = vec![("fields", files::FIELDS)];
            stale.headers = vec!["if-match: \"metadata-9\"".into()];
            stale.body = Some(ExpectedBody::Json(json!({"name":"stale.txt"})));
            let mut final_read = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "renamed.txt", "10", 6),
            );
            final_read.query = vec![("fields", files::FIELDS)];
            vec![before, update, stale, final_read]
        })
        .await;
        let plan = provider
            .prepare_metadata_precondition_probe(
                &scope(),
                "generated-id",
                "root-id",
                "report.txt",
                "renamed.txt",
                "stale.txt",
            )
            .unwrap();
        let result = provider
            .probe_metadata_precondition(&scope(), &plan, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.stale_rejected(), Some(true));
        let MetadataPreconditionProbe::Tested {
            before_version,
            updated_version,
            final_version,
            final_name,
            ..
        } = result
        else {
            panic!("strong ETag was not tested")
        };
        assert_eq!(before_version, "9");
        assert_eq!(updated_version, "10");
        assert_eq!(final_version, "10");
        assert_eq!(final_name, "renamed.txt");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn metadata_probe_reports_when_drive_accepts_the_stale_etag() {
        let (provider, server) = fixture(|_| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "9", 6),
            );
            before.query = vec![("fields", files::FIELDS)];
            before.response_headers = "ETag: \"metadata-9\"\r\n".into();
            let mut update = Exchange::json(
                "PATCH",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "renamed.txt", "10", 6),
            );
            update.query = vec![("fields", files::FIELDS)];
            update.headers = vec!["if-match: \"metadata-9\"".into()];
            update.body = Some(ExpectedBody::Json(json!({"name":"renamed.txt"})));
            let mut stale = Exchange::json(
                "PATCH",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "stale.txt", "11", 6),
            );
            stale.query = vec![("fields", files::FIELDS)];
            stale.headers = vec!["if-match: \"metadata-9\"".into()];
            stale.body = Some(ExpectedBody::Json(json!({"name":"stale.txt"})));
            let mut final_read = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "stale.txt", "11", 6),
            );
            final_read.query = vec![("fields", files::FIELDS)];
            vec![before, update, stale, final_read]
        })
        .await;
        let plan = provider
            .prepare_metadata_precondition_probe(
                &scope(),
                "generated-id",
                "root-id",
                "report.txt",
                "renamed.txt",
                "stale.txt",
            )
            .unwrap();
        let result = provider
            .probe_metadata_precondition(&scope(), &plan, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.stale_rejected(), Some(false));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn metadata_probe_does_not_mutate_without_a_strong_etag() {
        let (provider, server) = fixture(|_| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "9", 6),
            );
            before.query = vec![("fields", files::FIELDS)];
            vec![before]
        })
        .await;
        let plan = provider
            .prepare_metadata_precondition_probe(
                &scope(),
                "generated-id",
                "root-id",
                "report.txt",
                "renamed.txt",
                "stale.txt",
            )
            .unwrap();
        let result = provider
            .probe_metadata_precondition(&scope(), &plan, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.stale_rejected(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_mutation_moves_by_exact_identity_with_a_strong_precondition() {
        let (provider, server) = fixture(|_| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "9", 6),
            );
            before.query = vec![("fields", files::FIELDS)];
            before.response_headers = "ETag: \"metadata-9\"\r\n".into();
            let mut destination = Exchange::json(
                "GET",
                "/drive/v3/files",
                200,
                json!({"files":[],"incompleteSearch":false}),
            );
            destination.query = vec![
                ("q", "'destination-id' in parents and trashed = false"),
                ("spaces", "drive"),
                ("corpora", "user"),
                ("pageSize", "1000"),
                ("includeItemsFromAllDrives", "false"),
            ];
            let mut moved = Exchange::json(
                "PATCH",
                "/drive/v3/files/generated-id",
                200,
                named_file_at("generated-id", "destination-id", "moved.txt", "10", 6),
            );
            moved.query = vec![
                ("fields", files::FIELDS),
                ("addParents", "destination-id"),
                ("removeParents", "root-id"),
            ];
            moved.headers = vec!["if-match: \"metadata-9\"".into()];
            moved.body = Some(ExpectedBody::Json(json!({"name":"moved.txt"})));
            moved.response_headers = "ETag: \"metadata-10\"\r\n".into();
            vec![before, destination, moved]
        })
        .await;
        let request = mutation_request("destination-id", "moved.txt", "\"metadata-9\"");
        let MutationReceipt::Upsert(node) = provider
            .validation_mutations()
            .mutate(&request, &CancellationToken::new())
            .await
            .unwrap()
        else {
            panic!("move returned no item")
        };
        assert_eq!(node.id, "generated-id");
        assert_eq!(node.parent_id.as_deref(), Some("destination-id"));
        assert_eq!(node.name, "moved.txt");
        assert_eq!(node.etag.as_deref(), Some("\"metadata-10\""));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_mutation_refuses_an_occupied_destination_before_patch() {
        let (provider, server) = fixture(|_| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "9", 6),
            );
            before.query = vec![("fields", files::FIELDS)];
            before.response_headers = "ETag: \"metadata-9\"\r\n".into();
            let mut destination = Exchange::json(
                "GET",
                "/drive/v3/files",
                200,
                json!({
                    "files":[named_file_at("occupied-id","destination-id","MOVED.TXT","4",3)],
                    "incompleteSearch":false
                }),
            );
            destination.query = vec![("q", "'destination-id' in parents and trashed = false")];
            vec![before, destination]
        })
        .await;
        let request = mutation_request("destination-id", "moved.txt", "\"metadata-9\"");
        assert!(matches!(
            provider
                .validation_mutations()
                .mutate(&request, &CancellationToken::new())
                .await,
            Err(MutationError::Conflict)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_mutation_conflicts_before_destination_scan_when_the_etag_changed() {
        let (provider, server) = fixture(|_| {
            let mut current = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "10", 6),
            );
            current.query = vec![("fields", files::FIELDS)];
            current.response_headers = "ETag: \"metadata-10\"\r\n".into();
            vec![current]
        })
        .await;
        let request = mutation_request("destination-id", "moved.txt", "\"metadata-9\"");
        assert!(matches!(
            provider
                .validation_mutations()
                .mutate(&request, &CancellationToken::new())
                .await,
            Err(MutationError::Conflict)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_mutation_reconciles_a_lost_move_by_exact_identity() {
        let (provider, server) = fixture(|_| {
            let mut current = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file_at("generated-id", "destination-id", "moved.txt", "10", 6),
            );
            current.query = vec![("fields", files::FIELDS)];
            current.response_headers = "ETag: \"metadata-10\"\r\n".into();
            vec![current]
        })
        .await;
        let request = mutation_request("destination-id", "moved.txt", "\"metadata-9\"");
        let MutationReconciliation::Applied(MutationReceipt::Upsert(node)) = provider
            .validation_mutations()
            .reconcile_mutation(&request, &CancellationToken::new())
            .await
            .unwrap()
        else {
            panic!("lost move was not reconciled")
        };
        assert_eq!(node.parent_id.as_deref(), Some("destination-id"));
        assert_eq!(node.name, "moved.txt");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_mutation_reconciliation_retries_an_unchanged_source() {
        let (provider, server) = fixture(|_| {
            let mut current = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "9", 6),
            );
            current.query = vec![("fields", files::FIELDS)];
            current.response_headers = "ETag: \"metadata-9\"\r\n".into();
            vec![current]
        })
        .await;
        let request = mutation_request("destination-id", "moved.txt", "\"metadata-9\"");
        assert!(matches!(
            provider
                .validation_mutations()
                .reconcile_mutation(&request, &CancellationToken::new())
                .await,
            Ok(MutationReconciliation::Uncommitted)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_mutation_rejects_a_weak_etag_without_network() {
        let (provider, server) = fixture(|_| vec![]).await;
        for request in [
            mutation_request("destination-id", "moved.txt", "W/\"metadata-9\""),
            removal_request("W/\"metadata-9\""),
        ] {
            assert!(matches!(
                provider
                    .validation_mutations()
                    .mutate(&request, &CancellationToken::new())
                    .await,
                Err(MutationError::Invalid)
            ));
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_mutation_trashes_only_the_exact_conditional_file() {
        let (provider, server) = fixture(|_| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "9", 6),
            );
            before.query = vec![("fields", files::FIELDS)];
            before.response_headers = "ETag: \"metadata-9\"\r\n".into();
            let mut trashed_file = named_file("generated-id", "report.txt", "10", 6);
            trashed_file["trashed"] = true.into();
            let mut trash =
                Exchange::json("PATCH", "/drive/v3/files/generated-id", 200, trashed_file);
            trash.query = vec![("fields", files::FIELDS)];
            trash.headers = vec!["if-match: \"metadata-9\"".into()];
            trash.body = Some(ExpectedBody::Json(json!({"trashed":true})));
            vec![before, trash]
        })
        .await;
        let mutations = provider.validation_mutations();
        assert_eq!(
            mutations.deletion(),
            cirrove_core::mutation::DeletionSupport {
                recycle_bin: true,
                permanent: false
            }
        );
        assert!(matches!(
            mutations
                .mutate(
                    &removal_request("\"metadata-9\""),
                    &CancellationToken::new()
                )
                .await,
            Ok(MutationReceipt::Removed { item }) if item == "generated-id"
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_mutation_reconciles_an_exact_trashed_item() {
        let (provider, server) = fixture(|_| {
            let mut trashed_file = named_file("generated-id", "report.txt", "10", 6);
            trashed_file["trashed"] = true.into();
            let mut current =
                Exchange::json("GET", "/drive/v3/files/generated-id", 200, trashed_file);
            current.query = vec![("fields", files::FIELDS)];
            vec![current]
        })
        .await;
        assert!(matches!(
            provider
                .validation_mutations()
                .reconcile_mutation(
                    &removal_request("\"metadata-9\""),
                    &CancellationToken::new()
                )
                .await,
            Ok(MutationReconciliation::Applied(MutationReceipt::Removed { item }))
                if item == "generated-id"
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn validation_mutation_does_not_treat_a_missing_file_as_confirmed_removal() {
        let (provider, server) = fixture(|_| {
            let mut missing = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            missing.query = vec![("fields", files::FIELDS)];
            vec![missing]
        })
        .await;
        assert!(matches!(
            provider
                .validation_mutations()
                .reconcile_mutation(
                    &removal_request("\"metadata-9\""),
                    &CancellationToken::new()
                )
                .await,
            Ok(MutationReconciliation::Indeterminate)
        ));
        server.await.unwrap();
    }

    fn content_probe_exchanges(
        accepted: &[u8],
        stale: &[u8],
        stale_rejected: bool,
    ) -> Vec<Exchange> {
        let mut before = Exchange::json(
            "GET",
            "/drive/v3/files/generated-id",
            200,
            named_file("generated-id", "renamed.txt", "10", 6),
        );
        before.query = vec![("fields", files::FIELDS)];
        before.response_headers = "ETag: \"content-10\"\r\n".into();

        let mut update = Exchange::json(
            "PATCH",
            "/upload/drive/v3/files/generated-id",
            200,
            named_file("generated-id", "renamed.txt", "11", accepted.len()),
        );
        update.query = vec![("uploadType", "media"), ("fields", files::FIELDS)];
        update.headers = vec![
            "if-match: \"content-10\"".into(),
            "content-type: application/octet-stream".into(),
        ];
        update.body = Some(ExpectedBody::Bytes(accepted.to_vec()));

        let mut stale_update = if stale_rejected {
            Exchange::json(
                "PATCH",
                "/upload/drive/v3/files/generated-id",
                412,
                json!({}),
            )
        } else {
            Exchange::json(
                "PATCH",
                "/upload/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "renamed.txt", "12", stale.len()),
            )
        };
        stale_update.query = vec![("uploadType", "media"), ("fields", files::FIELDS)];
        stale_update.headers = vec![
            "if-match: \"content-10\"".into(),
            "content-type: application/octet-stream".into(),
        ];
        stale_update.body = Some(ExpectedBody::Bytes(stale.to_vec()));

        let (final_bytes, final_version) = if stale_rejected {
            (accepted, "11")
        } else {
            (stale, "12")
        };
        let final_metadata = || {
            let mut exchange = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file(
                    "generated-id",
                    "renamed.txt",
                    final_version,
                    final_bytes.len(),
                ),
            );
            exchange.query = vec![("fields", files::FIELDS)];
            exchange
        };
        let mut content = Exchange::json("GET", "/drive/v3/files/generated-id", 206, json!({}));
        content.query = vec![("alt", "media")];
        content.headers = vec![format!("range: bytes=0-{}", final_bytes.len() - 1)];
        content.response_headers = format!(
            "Content-Range: bytes 0-{}/{}\r\n",
            final_bytes.len() - 1,
            final_bytes.len()
        );
        content.response_body = final_bytes.to_vec();
        vec![
            before,
            update,
            stale_update,
            final_metadata(),
            final_metadata(),
            content,
            final_metadata(),
        ]
    }

    #[tokio::test]
    async fn content_probe_updates_once_and_rejects_the_now_stale_etag() {
        let accepted = b"accepted replacement";
        let stale = b"stale replacement must not win";
        let (provider, server) = fixture(|_| content_probe_exchanges(accepted, stale, true)).await;
        let plan = provider
            .prepare_content_precondition_probe(
                &scope(),
                "generated-id",
                "root-id",
                "renamed.txt",
                accepted,
                stale,
            )
            .unwrap();
        let result = provider
            .probe_content_precondition(
                &scope(),
                &plan,
                accepted.to_vec(),
                stale.to_vec(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.stale_rejected(), Some(true));
        let ContentPreconditionProbe::Tested {
            final_size,
            final_sha256,
            ..
        } = result
        else {
            panic!("strong ETag was not tested")
        };
        assert_eq!(final_size, accepted.len() as u64);
        assert_eq!(final_sha256, hex::encode(Sha256::digest(accepted)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn content_probe_reports_when_drive_accepts_the_stale_etag() {
        let accepted = b"accepted replacement";
        let stale = b"stale replacement wins";
        let (provider, server) = fixture(|_| content_probe_exchanges(accepted, stale, false)).await;
        let plan = provider
            .prepare_content_precondition_probe(
                &scope(),
                "generated-id",
                "root-id",
                "renamed.txt",
                accepted,
                stale,
            )
            .unwrap();
        let result = provider
            .probe_content_precondition(
                &scope(),
                &plan,
                accepted.to_vec(),
                stale.to_vec(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.stale_rejected(), Some(false));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn content_probe_does_not_mutate_without_a_strong_etag() {
        let accepted = b"accepted replacement";
        let stale = b"stale replacement";
        let (provider, server) = fixture(|_| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "renamed.txt", "10", 6),
            );
            before.query = vec![("fields", files::FIELDS)];
            vec![before]
        })
        .await;
        let plan = provider
            .prepare_content_precondition_probe(
                &scope(),
                "generated-id",
                "root-id",
                "renamed.txt",
                accepted,
                stale,
            )
            .unwrap();
        let result = provider
            .probe_content_precondition(
                &scope(),
                &plan,
                accepted.to_vec(),
                stale.to_vec(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.stale_rejected(), None);
        server.await.unwrap();
    }

    fn resumable_start(
        address: std::net::SocketAddr,
        session: &'static str,
        etag: &str,
        size: usize,
    ) -> Exchange {
        let mut start = Exchange::json(
            "PATCH",
            "/upload/drive/v3/files/generated-id",
            200,
            json!({}),
        );
        start.query = vec![("uploadType", "resumable"), ("fields", files::FIELDS)];
        start.headers = vec![
            format!("if-match: {etag}"),
            "x-upload-content-type: application/octet-stream".into(),
            format!("x-upload-content-length: {size}"),
        ];
        start.body = Some(ExpectedBody::Json(json!({})));
        start.response_body.clear();
        start.response_headers =
            format!("Location: http://{address}/upload/drive/v3/files?upload_id={session}\r\n");
        start
    }

    fn resumable_uploads(session: &'static str, bytes: &[u8], version: &str) -> Vec<Exchange> {
        let split = PART_SIZE as usize;
        let mut first = Exchange::json("PUT", "/upload/drive/v3/files", 308, json!({}));
        first.query = vec![("upload_id", session)];
        first.authorized = false;
        first.headers = vec![format!(
            "content-range: bytes 0-{}/{size}",
            split - 1,
            size = bytes.len()
        )];
        first.body = Some(ExpectedBody::Bytes(bytes[..split].to_vec()));
        first.response_body.clear();
        first.response_headers = format!("Range: bytes=0-{}\r\n", split - 1);

        let mut final_part = Exchange::json(
            "PUT",
            "/upload/drive/v3/files",
            200,
            named_file("generated-id", "renamed.txt", version, bytes.len()),
        );
        final_part.query = vec![("upload_id", session)];
        final_part.authorized = false;
        final_part.headers = vec![format!(
            "content-range: bytes {split}-{}/{size}",
            bytes.len() - 1,
            size = bytes.len()
        )];
        final_part.body = Some(ExpectedBody::Bytes(bytes[split..].to_vec()));
        vec![first, final_part]
    }

    fn resumable_readback(bytes: &[u8], version: &'static str) -> Vec<Exchange> {
        let metadata = || {
            let mut exchange = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "renamed.txt", version, bytes.len()),
            );
            exchange.query = vec![("fields", files::FIELDS)];
            exchange
        };
        let mut result = vec![metadata()];
        let mut offset = 0;
        while offset < bytes.len() {
            let end = (offset + 4 * 1024 * 1024).min(bytes.len());
            result.push(metadata());
            let mut content = Exchange::json("GET", "/drive/v3/files/generated-id", 206, json!({}));
            content.query = vec![("alt", "media")];
            content.headers = vec![format!("range: bytes={offset}-{}", end - 1)];
            content.response_headers = format!(
                "Content-Range: bytes {offset}-{}/{total}\r\n",
                end - 1,
                total = bytes.len()
            );
            content.response_body = bytes[offset..end].to_vec();
            result.push(content);
            result.push(metadata());
            offset = end;
        }
        result
    }

    fn resumable_probe_exchanges(
        address: std::net::SocketAddr,
        accepted: &[u8],
        stale: &[u8],
        stale_rejected: bool,
    ) -> Vec<Exchange> {
        let mut before = Exchange::json(
            "GET",
            "/drive/v3/files/generated-id",
            200,
            named_file("generated-id", "renamed.txt", "10", 6),
        );
        before.query = vec![("fields", files::FIELDS)];
        before.response_headers = "ETag: \"resumable-10\"\r\n".into();
        let mut result = vec![
            before,
            resumable_start(address, "CURRENT", "\"resumable-10\"", accepted.len()),
        ];
        result.extend(resumable_uploads("CURRENT", accepted, "11"));
        if stale_rejected {
            let mut stale_start = Exchange::json(
                "PATCH",
                "/upload/drive/v3/files/generated-id",
                412,
                json!({}),
            );
            stale_start.query = vec![("uploadType", "resumable"), ("fields", files::FIELDS)];
            stale_start.headers = vec![
                "if-match: \"resumable-10\"".into(),
                "x-upload-content-type: application/octet-stream".into(),
                format!("x-upload-content-length: {}", stale.len()),
            ];
            stale_start.body = Some(ExpectedBody::Json(json!({})));
            result.push(stale_start);
            result.extend(resumable_readback(accepted, "11"));
        } else {
            result.push(resumable_start(
                address,
                "STALE",
                "\"resumable-10\"",
                stale.len(),
            ));
            result.extend(resumable_uploads("STALE", stale, "12"));
            result.extend(resumable_readback(stale, "12"));
        }
        result
    }

    #[tokio::test]
    async fn resumable_probe_uploads_multiple_ranges_and_rejects_the_stale_etag() {
        let accepted = vec![0x52; PART_SIZE as usize + 29];
        let stale = vec![0x53; PART_SIZE as usize + 31];
        let (provider, server) =
            fixture(|address| resumable_probe_exchanges(address, &accepted, &stale, true)).await;
        let plan = provider
            .prepare_resumable_precondition_probe(
                &scope(),
                "generated-id",
                "root-id",
                "renamed.txt",
                &accepted,
                &stale,
            )
            .unwrap();
        let result = provider
            .probe_resumable_precondition(
                &scope(),
                &plan,
                accepted.clone(),
                stale,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.stale_rejected(), Some(true));
        let ResumablePreconditionProbe::Tested {
            final_size,
            final_sha256,
            ..
        } = result
        else {
            panic!("strong ETag was not tested")
        };
        assert_eq!(final_size, accepted.len() as u64);
        assert_eq!(final_sha256, hex::encode(Sha256::digest(&accepted)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resumable_probe_reports_when_drive_accepts_the_stale_etag() {
        let accepted = vec![0x52; PART_SIZE as usize + 29];
        let stale = vec![0x53; PART_SIZE as usize + 31];
        let (provider, server) =
            fixture(|address| resumable_probe_exchanges(address, &accepted, &stale, false)).await;
        let plan = provider
            .prepare_resumable_precondition_probe(
                &scope(),
                "generated-id",
                "root-id",
                "renamed.txt",
                &accepted,
                &stale,
            )
            .unwrap();
        let result = provider
            .probe_resumable_precondition(
                &scope(),
                &plan,
                accepted,
                stale,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.stale_rejected(), Some(false));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resumable_probe_accepts_a_session_but_rejects_stale_finalization() {
        let accepted = vec![0x52; PART_SIZE as usize + 29];
        let stale = vec![0x53; PART_SIZE as usize + 31];
        let (provider, server) = fixture(|address| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "renamed.txt", "10", 6),
            );
            before.query = vec![("fields", files::FIELDS)];
            before.response_headers = "ETag: \"resumable-10\"\r\n".into();
            let mut exchanges = vec![
                before,
                resumable_start(address, "CURRENT", "\"resumable-10\"", accepted.len()),
            ];
            exchanges.extend(resumable_uploads("CURRENT", &accepted, "11"));
            exchanges.push(resumable_start(
                address,
                "STALE",
                "\"resumable-10\"",
                stale.len(),
            ));
            let mut stale_uploads = resumable_uploads("STALE", &stale, "12");
            let finalization = stale_uploads.last_mut().unwrap();
            finalization.status = 412;
            finalization.response_body = serde_json::to_vec(&json!({})).unwrap();
            exchanges.extend(stale_uploads);
            exchanges.extend(resumable_readback(&accepted, "11"));
            exchanges
        })
        .await;
        let plan = provider
            .prepare_resumable_precondition_probe(
                &scope(),
                "generated-id",
                "root-id",
                "renamed.txt",
                &accepted,
                &stale,
            )
            .unwrap();
        let result = provider
            .probe_resumable_precondition(
                &scope(),
                &plan,
                accepted,
                stale,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.stale_rejected(), Some(true));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resumable_probe_does_not_start_a_session_without_a_strong_etag() {
        let accepted = vec![0x52; 5 * 1024 * 1024 + 1];
        let stale = vec![0x53; 5 * 1024 * 1024 + 2];
        let (provider, server) = fixture(|_| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "renamed.txt", "10", 6),
            );
            before.query = vec![("fields", files::FIELDS)];
            vec![before]
        })
        .await;
        let plan = provider
            .prepare_resumable_precondition_probe(
                &scope(),
                "generated-id",
                "root-id",
                "renamed.txt",
                &accepted,
                &stale,
            )
            .unwrap();
        let result = provider
            .probe_resumable_precondition(
                &scope(),
                &plan,
                accepted,
                stale,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.stale_rejected(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_empty_file_uses_the_prepared_identity_without_a_session() {
        let generated = "generated-id";
        let (provider, server) = fixture(|_| {
            let generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            let missing = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            let mut create = Exchange::json("POST", "/drive/v3/files", 200, file(generated, 0));
            create.query = vec![("fields", files::FIELDS)];
            create.body = Some(ExpectedBody::Json(metadata(generated)));
            vec![generate, missing, create]
        })
        .await;
        let request = request(b"");
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("missing prepared ID")
        };
        let UploadStep::Complete(node) = provider
            .inspect_upload(&request, &prepared, &cancel)
            .await
            .unwrap()
        else {
            panic!("empty file did not complete")
        };
        assert_eq!((node.id.as_str(), node.size), (generated, 0));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn replacement_uses_a_durable_target_and_conditional_resumable_session() {
        let bytes = b"abcdef";
        let (provider, server) = fixture(|address| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "10", 3),
            );
            before.query = vec![("fields", files::FIELDS)];
            before.response_headers = "ETag: \"old-10\"\r\n".into();
            let mut start = Exchange::json(
                "PATCH",
                "/upload/drive/v3/files/generated-id",
                200,
                json!({}),
            );
            start.query = vec![("uploadType", "resumable"), ("fields", files::FIELDS)];
            start.headers = vec![
                "if-match: \"old-10\"".into(),
                "x-upload-content-type: application/octet-stream".into(),
                "x-upload-content-length: 6".into(),
            ];
            start.body = Some(ExpectedBody::Json(json!({})));
            start.response_body.clear();
            start.response_headers = format!(
                "Location: http://{address}/upload/drive/v3/files?upload_id=REPLACEMENT\r\n"
            );
            let mut upload = Exchange::json(
                "PUT",
                "/upload/drive/v3/files",
                200,
                named_file("generated-id", "report.txt", "11", bytes.len()),
            );
            upload.query = vec![("upload_id", "REPLACEMENT")];
            upload.authorized = false;
            upload.headers = vec!["content-range: bytes 0-5/6".into()];
            upload.body = Some(ExpectedBody::Bytes(bytes.to_vec()));
            upload.response_headers = "ETag: \"new-11\"\r\n".into();
            vec![before, start, upload]
        })
        .await;
        let request = UploadRequest {
            scope: scope(),
            intent: UploadIntent::Replace {
                item: "generated-id".into(),
                expected_etag: "\"old-10\"".into(),
            },
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        };
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("replacement target was not persisted first")
        };
        assert!(prepared.expose_secret().contains("generated-id"));
        let UploadStep::Continue(progress) = provider
            .inspect_upload(&request, &prepared, &cancel)
            .await
            .unwrap()
        else {
            panic!("replacement session did not request content")
        };
        let UploadStep::Complete(node) = provider
            .upload_part(&request, &progress.checkpoint, 0, bytes.to_vec(), &cancel)
            .await
            .unwrap()
        else {
            panic!("replacement did not complete")
        };
        assert_eq!(node.id, "generated-id");
        assert_eq!(node.etag.as_deref(), Some("\"new-11\""));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn replacement_conflicts_before_starting_a_session_when_the_etag_changed() {
        let (provider, server) = fixture(|_| {
            let mut current = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "12", 3),
            );
            current.query = vec![("fields", files::FIELDS)];
            current.response_headers = "ETag: \"newer-12\"\r\n".into();
            vec![current]
        })
        .await;
        let request = UploadRequest {
            scope: scope(),
            intent: UploadIntent::Replace {
                item: "generated-id".into(),
                expected_etag: "\"old-10\"".into(),
            },
            size: 6,
            sha256: hex::encode(Sha256::digest(b"abcdef")),
        };
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("replacement target was not prepared")
        };
        assert!(matches!(
            provider.inspect_upload(&request, &prepared, &cancel).await,
            Err(UploadError::Conflict)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn replacement_rejects_weak_malformed_and_empty_requests_without_network() {
        let (provider, server) = fixture(|_| vec![]).await;
        for (etag, size) in [
            ("etag", 6),
            ("W/\"weak\"", 6),
            ("\"line\nfeed\"", 6),
            ("\"strong\"", 0),
        ] {
            let payload: &[u8] = if size == 0 { b"" } else { b"abcdef" };
            let request = UploadRequest {
                scope: scope(),
                intent: UploadIntent::Replace {
                    item: "generated-id".into(),
                    expected_etag: etag.into(),
                },
                size,
                sha256: hex::encode(Sha256::digest(payload)),
            };
            assert!(matches!(
                provider
                    .begin_upload(&request, &CancellationToken::new())
                    .await,
                Err(UploadError::Invalid)
            ));
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn replacement_maps_a_final_precondition_failure_to_conflict() {
        let bytes = b"abcdef";
        let (provider, server) = fixture(|address| {
            let mut before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                named_file("generated-id", "report.txt", "10", 3),
            );
            before.query = vec![("fields", files::FIELDS)];
            before.response_headers = "ETag: \"old-10\"\r\n".into();
            let mut start = resumable_start(address, "REPLACEMENT", "\"old-10\"", bytes.len());
            start.body = Some(ExpectedBody::Json(json!({})));
            let mut rejected = Exchange::json("PUT", "/upload/drive/v3/files", 412, json!({}));
            rejected.query = vec![("upload_id", "REPLACEMENT")];
            rejected.authorized = false;
            rejected.headers = vec!["content-range: bytes 0-5/6".into()];
            rejected.body = Some(ExpectedBody::Bytes(bytes.to_vec()));
            vec![before, start, rejected]
        })
        .await;
        let request = UploadRequest {
            scope: scope(),
            intent: UploadIntent::Replace {
                item: "generated-id".into(),
                expected_etag: "\"old-10\"".into(),
            },
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        };
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("replacement target was not prepared")
        };
        let UploadStep::Continue(progress) = provider
            .inspect_upload(&request, &prepared, &cancel)
            .await
            .unwrap()
        else {
            panic!("replacement session did not start")
        };
        assert!(matches!(
            provider
                .upload_part(&request, &progress.checkpoint, 0, bytes.to_vec(), &cancel)
                .await,
            Err(UploadError::Conflict)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn replacement_reconciliation_hashes_the_exact_item_after_a_lost_response() {
        let bytes = b"abcdef";
        let (provider, server) = fixture(|_| {
            let metadata = || {
                let mut exchange = Exchange::json(
                    "GET",
                    "/drive/v3/files/generated-id",
                    200,
                    named_file("generated-id", "report.txt", "11", bytes.len()),
                );
                exchange.query = vec![("fields", files::FIELDS)];
                exchange
            };
            let mut exact = metadata();
            exact.response_headers = "ETag: \"new-11\"\r\n".into();
            let mut content = Exchange::json("GET", "/drive/v3/files/generated-id", 206, json!({}));
            content.query = vec![("alt", "media")];
            content.headers = vec!["range: bytes=0-5".into()];
            content.response_headers = "Content-Range: bytes 0-5/6\r\n".into();
            content.response_body = bytes.to_vec();
            vec![exact, metadata(), content, metadata()]
        })
        .await;
        let request = UploadRequest {
            scope: scope(),
            intent: UploadIntent::Replace {
                item: "generated-id".into(),
                expected_etag: "\"old-10\"".into(),
            },
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        };
        let UploadStep::Prepared(prepared) = provider
            .begin_upload(&request, &CancellationToken::new())
            .await
            .unwrap()
        else {
            panic!("replacement target was not prepared")
        };
        let Reconciliation::Committed(node) = provider
            .reconcile_upload(&request, Some(&prepared), &CancellationToken::new())
            .await
            .unwrap()
        else {
            panic!("lost replacement success was not reconciled")
        };
        assert_eq!(node.id, "generated-id");
        assert_eq!(node.etag.as_deref(), Some("\"new-11\""));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_gone_session_returns_to_the_same_prepared_identity_without_its_old_url() {
        let generated = "generated-id";
        let bytes = b"abcdef";
        let (provider, server) = fixture(|address| {
            let mut generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            generate.query = vec![("count", "1")];
            let missing1 = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            let mut start1 = Exchange::json("POST", "/upload/drive/v3/files", 200, json!({}));
            start1.body = Some(ExpectedBody::Json(metadata(generated)));
            start1.response_body.clear();
            start1.response_headers = format!(
                "Location: http://{address}/upload/drive/v3/files?upload_id=PRIVATE-ONE\r\n"
            );
            let mut gone = Exchange::json("PUT", "/upload/drive/v3/files", 404, json!({}));
            gone.query = vec![("upload_id", "PRIVATE-ONE")];
            gone.authorized = false;
            gone.headers = vec!["content-range: bytes */6".into()];
            gone.body = Some(ExpectedBody::Bytes(vec![]));
            let missing2 = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            let mut start2 = Exchange::json("POST", "/upload/drive/v3/files", 200, json!({}));
            start2.body = Some(ExpectedBody::Json(metadata(generated)));
            start2.response_body.clear();
            start2.response_headers = format!(
                "Location: http://{address}/upload/drive/v3/files?upload_id=PRIVATE-TWO\r\n"
            );
            vec![generate, missing1, start1, gone, missing2, start2]
        })
        .await;
        let request = request(bytes);
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("missing prepared ID")
        };
        let UploadStep::Continue(session) = provider
            .inspect_upload(&request, &prepared, &cancel)
            .await
            .unwrap()
        else {
            panic!("missing first session")
        };
        let UploadStep::Prepared(restarted) = provider
            .inspect_upload(&request, &session.checkpoint, &cancel)
            .await
            .unwrap()
        else {
            panic!("gone session did not retain its identity")
        };
        assert!(restarted.expose_secret().contains(generated));
        assert!(!restarted.expose_secret().contains("PRIVATE-ONE"));
        let UploadStep::Continue(second) = provider
            .inspect_upload(&request, &restarted, &cancel)
            .await
            .unwrap()
        else {
            panic!("same identity did not start a replacement session")
        };
        assert!(second.checkpoint.expose_secret().contains("PRIVATE-TWO"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn every_non_throttle_session_4xx_restarts_with_the_same_identity() {
        for status in [400, 401, 403, 404, 408, 409, 410, 412, 416] {
            let (provider, server) = fixture(move |_| {
                let mut reply = Exchange::json("PUT", "/upload/drive/v3/files", status, json!({}));
                reply.query = vec![("upload_id", "PRIVATE-SESSION")];
                reply.authorized = false;
                reply.headers = vec!["content-range: bytes */6".into()];
                reply.body = Some(ExpectedBody::Bytes(vec![]));
                vec![reply]
            })
            .await;
            let request = request(b"abcdef");
            let session = SavedUpload::Session {
                version: 1,
                request: request.clone(),
                id: "generated-id".into(),
                url: format!(
                    "{}/upload/drive/v3/files?upload_id=PRIVATE-SESSION",
                    provider.endpoint.origin().ascii_serialization()
                ),
                offset: 0,
                length: 6,
            };
            let checkpoint = SecretString::from(serde_json::to_string(&session).unwrap());
            let UploadStep::Prepared(prepared) = provider
                .inspect_upload(&request, &checkpoint, &CancellationToken::new())
                .await
                .unwrap()
            else {
                panic!("session HTTP {status} did not restart")
            };
            assert!(prepared.expose_secret().contains("generated-id"));
            assert!(!prepared.expose_secret().contains("PRIVATE-SESSION"));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn session_rate_limiting_keeps_the_session_for_later_inspection() {
        let (provider, server) = fixture(|_| {
            let mut reply = Exchange::json("PUT", "/upload/drive/v3/files", 429, json!({}));
            reply.query = vec![("upload_id", "PRIVATE-SESSION")];
            reply.authorized = false;
            reply.headers = vec!["content-range: bytes */6".into()];
            reply.body = Some(ExpectedBody::Bytes(vec![]));
            reply.response_headers = "Retry-After: 1\r\n".into();
            vec![reply]
        })
        .await;
        let request = request(b"abcdef");
        let session = SavedUpload::Session {
            version: 1,
            request: request.clone(),
            id: "generated-id".into(),
            url: format!(
                "{}/upload/drive/v3/files?upload_id=PRIVATE-SESSION",
                provider.endpoint.origin().ascii_serialization()
            ),
            offset: 0,
            length: 6,
        };
        let checkpoint = SecretString::from(serde_json::to_string(&session).unwrap());
        assert!(matches!(
            provider
                .inspect_upload(&request, &checkpoint, &CancellationToken::new())
                .await,
            Err(UploadError::Provider(ProviderError::Throttled(_)))
        ));
        assert!(checkpoint.expose_secret().contains("PRIVATE-SESSION"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn status_uses_the_exact_server_offset_even_when_an_interrupted_chunk_is_partial() {
        let size = 20_000_000_u64;
        let (provider, server) = fixture(|_| {
            let mut status = Exchange::json("PUT", "/upload/drive/v3/files", 308, json!({}));
            status.query = vec![("upload_id", "PRIVATE-SESSION")];
            status.authorized = false;
            status.headers = vec![format!("content-range: bytes */{size}")];
            status.body = Some(ExpectedBody::Bytes(vec![]));
            status.response_headers = "Range: bytes=0-42\r\n".into();
            vec![status]
        })
        .await;
        let request = UploadRequest {
            scope: scope(),
            intent: UploadIntent::Create {
                parent: "root-id".into(),
                name: "report.txt".into(),
            },
            size,
            sha256: "0".repeat(64),
        };
        let session = SavedUpload::Session {
            version: 1,
            request: request.clone(),
            id: "generated-id".into(),
            url: format!(
                "{}/upload/drive/v3/files?upload_id=PRIVATE-SESSION",
                provider.endpoint.origin().ascii_serialization()
            ),
            offset: 0,
            length: PART_SIZE,
        };
        let checkpoint = SecretString::from(serde_json::to_string(&session).unwrap());
        let UploadStep::Continue(progress) = provider
            .inspect_upload(&request, &checkpoint, &CancellationToken::new())
            .await
            .unwrap()
        else {
            panic!("status did not return a range")
        };
        assert_eq!(progress.offset, 43);
        assert_eq!(progress.length, PART_SIZE);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_chunk_reply_cannot_advance_past_the_bytes_that_were_sent() {
        let size = ALIGNMENT + 10;
        let bytes = vec![b'x'; ALIGNMENT as usize];
        let (provider, server) = fixture(|_| {
            let mut reply = Exchange::json("PUT", "/upload/drive/v3/files", 308, json!({}));
            reply.query = vec![("upload_id", "PRIVATE-SESSION")];
            reply.authorized = false;
            reply.headers = vec![format!("content-range: bytes 0-{}/{size}", ALIGNMENT - 1)];
            reply.body = Some(ExpectedBody::Bytes(bytes.clone()));
            reply.response_headers = format!("Range: bytes=0-{ALIGNMENT}\r\n");
            vec![reply]
        })
        .await;
        let request = UploadRequest {
            scope: scope(),
            intent: UploadIntent::Create {
                parent: "root-id".into(),
                name: "report.txt".into(),
            },
            size,
            sha256: "0".repeat(64),
        };
        let session = SavedUpload::Session {
            version: 1,
            request: request.clone(),
            id: "generated-id".into(),
            url: format!(
                "{}/upload/drive/v3/files?upload_id=PRIVATE-SESSION",
                provider.endpoint.origin().ascii_serialization()
            ),
            offset: 0,
            length: ALIGNMENT as u32,
        };
        let checkpoint = SecretString::from(serde_json::to_string(&session).unwrap());
        assert!(matches!(
            provider
                .upload_part(&request, &checkpoint, 0, bytes, &CancellationToken::new())
                .await,
            Err(UploadError::Uncertain)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reconciliation_reads_back_the_exact_generated_identity_and_hash() {
        let generated = "generated-id";
        let bytes = b"abcdef";
        let (provider, server) = fixture(|_| {
            let mut generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            generate.query = vec![("count", "1")];
            let receipt = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                file(generated, bytes.len()),
            );
            let before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                file(generated, bytes.len()),
            );
            let mut content = Exchange::json("GET", "/drive/v3/files/generated-id", 206, json!({}));
            content.query = vec![("alt", "media")];
            content.headers = vec!["range: bytes=0-5".into()];
            content.response_headers = "Content-Range: bytes 0-5/6\r\n".into();
            content.response_body = bytes.to_vec();
            let after = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                file(generated, bytes.len()),
            );
            vec![generate, receipt, before, content, after]
        })
        .await;
        let request = request(bytes);
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("missing prepared ID")
        };
        let Reconciliation::Committed(node) = provider
            .reconcile_upload(&request, Some(&prepared), &cancel)
            .await
            .unwrap()
        else {
            panic!("exact content was not reconciled")
        };
        assert_eq!(node.id, generated);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_foreign_session_location_is_rejected_without_following_it() {
        let generated = "generated-id";
        let (provider, server) = fixture(|_| {
            let generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            let missing = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            let mut start = Exchange::json("POST", "/upload/drive/v3/files", 200, json!({}));
            start.body = Some(ExpectedBody::Json(metadata(generated)));
            start.response_body.clear();
            start.response_headers =
                "Location: https://evil.invalid/upload/drive/v3/files?private=session\r\n".into();
            vec![generate, missing, start]
        })
        .await;
        let request = request(b"abcdef");
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("missing prepared ID")
        };
        assert!(matches!(
            provider.inspect_upload(&request, &prepared, &cancel).await,
            Err(UploadError::Provider(ProviderError::Protocol(_)))
        ));
        server.await.unwrap();
    }
}
