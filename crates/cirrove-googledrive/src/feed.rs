use super::*;
use async_trait::async_trait;
use cirrove_core::{
    CancellationToken, Change, ChangePage, Checkpoint, Cursor, MetadataProvider, Priority,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct FeedCursor {
    scope: Scope,
    phase: Phase,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum Phase {
    Listing { start: String, page: String },
    Changes { page: String },
}
impl FeedCursor {
    fn encode(scope: &Scope, phase: Phase) -> Result<Cursor, ProviderError> {
        serde_json::to_string(&Self {
            scope: scope.clone(),
            phase,
        })
        .map(Cursor)
        .map_err(|_| ProviderError::Protocol("Google cursor encoding"))
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Start {
    start_page_token: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Changes {
    #[serde(default)]
    changes: Vec<FileChange>,
    next_page_token: Option<String>,
    new_start_page_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileChange {
    file_id: String,
    #[serde(default)]
    removed: bool,
    file: Option<files::File>,
}

#[async_trait]
impl MetadataProvider for GoogleDrive {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }
    async fn changes(
        &self,
        scope: &Scope,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        self.check_scope(scope)?;
        let cursor: Option<FeedCursor> = cursor
            .map(|c| {
                if c.0.len() > 4 * MAX_TOKEN_BYTES {
                    return Err(ProviderError::Protocol("Google cursor exceeds limit"));
                }
                serde_json::from_str(&c.0)
                    .map_err(|_| ProviderError::Protocol("invalid Google feed cursor"))
            })
            .transpose()?;
        if cursor.as_ref().is_some_and(|c| c.scope != *scope) {
            return Err(ProviderError::Permission);
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let _permit = self.budget.acquire(Priority::Background, cancel).await?;
                match cursor.map(|c| c.phase) {
                    None => {
                        // Capture the change frontier BEFORE scanning. Catch-up joins
                        // this same staging transaction before Complete is returned.
                        let mut url = self.url(&["changes", "startPageToken"])?;
                        url.query_pairs_mut().append_pair("fields", "startPageToken");
                        if self.shared_drive {
                            url.query_pairs_mut().append_pair("driveId", &self.collection);
                            self.shared_drive_query(&mut url);
                        }
                        let start: Start = self.json(url).await?;
                        valid_token(&start.start_page_token)?;
                        self.baseline(scope, &start.start_page_token, None).await
                    }
                    Some(Phase::Listing { start, page }) => {
                        valid_token(&start)?; valid_token(&page)?;
                        self.baseline(scope, &start, Some(&page)).await
                    }
                    Some(Phase::Changes { page }) => {
                        valid_token(&page)?;
                        self.incremental(scope, &page).await
                    }
                }
            } => result,
        }
    }
}
impl GoogleDrive {
    async fn baseline(
        &self,
        scope: &Scope,
        start: &str,
        page: Option<&str>,
    ) -> Result<ChangePage, ProviderError> {
        let listing = self.list_files(None, page).await?;
        let mut changes = Vec::new();
        for file in listing.files {
            if file.trashed || !self.accepts_file(&file) {
                continue;
            }
            changes.push(Change::Upsert(file.node(&scope.collection)?));
        }
        if self.shared_drive
            && page.is_none()
            && !changes
                .iter()
                .any(|change| matches!(change, Change::Upsert(node) if node.id == self.collection))
        {
            let root = self.file(&self.collection).await?;
            if root.mime_type != files::FOLDER || !root.parents.is_empty() {
                return Err(ProviderError::Protocol("invalid shared-drive root"));
            }
            changes.push(Change::Upsert(root.node(&scope.collection)?));
        }
        let phase = match listing.next_page_token {
            Some(page) => Phase::Listing {
                start: start.into(),
                page,
            },
            None => Phase::Changes { page: start.into() },
        };
        Ok(ChangePage {
            changes,
            checkpoint: Checkpoint::Continue(FeedCursor::encode(scope, phase)?),
        })
    }
    async fn incremental(&self, scope: &Scope, page: &str) -> Result<ChangePage, ProviderError> {
        let mut url = self.url(&["changes"])?;
        url.query_pairs_mut().extend_pairs([
            ("pageToken", page),
            ("pageSize", "1000"),
            ("spaces", "drive"),
            ("includeRemoved", "true"),
            (
                "includeItemsFromAllDrives",
                if self.shared_drive { "true" } else { "false" },
            ),
            (
                "fields",
                &format!(
                    "nextPageToken,newStartPageToken,changes(fileId,removed,file({}))",
                    files::FIELDS
                ),
            ),
        ]);
        if self.shared_drive {
            url.query_pairs_mut()
                .append_pair("driveId", &self.collection);
            self.shared_drive_query(&mut url);
        }
        let response: Changes = self.json(url).await?;
        let mut changes = Vec::new();
        for change in response.changes {
            valid_id(&change.file_id)?;
            if change.removed {
                changes.push(Change::Delete { id: change.file_id });
                continue;
            }
            let file = change
                .file
                .ok_or(ProviderError::Protocol("Google change missing file"))?;
            if file.id != change.file_id {
                return Err(ProviderError::Protocol("Google change identity mismatch"));
            }
            if file.trashed || !self.accepts_file(&file) {
                changes.push(Change::Delete { id: change.file_id });
            } else {
                changes.push(Change::Upsert(file.node(&scope.collection)?));
            }
        }
        let checkpoint = match (response.next_page_token, response.new_start_page_token) {
            (Some(next), None) => {
                valid_token(&next)?;
                if next == page {
                    return Err(ProviderError::Protocol("repeated Google change token"));
                }
                Checkpoint::Continue(FeedCursor::encode(scope, Phase::Changes { page: next })?)
            }
            (None, Some(next)) => {
                valid_token(&next)?;
                Checkpoint::Complete(FeedCursor::encode(scope, Phase::Changes { page: next })?)
            }
            _ => {
                return Err(ProviderError::Protocol(
                    "Google changes missing an unambiguous checkpoint",
                ));
            }
        };
        Ok(ChangePage {
            changes,
            checkpoint,
        })
    }
}
