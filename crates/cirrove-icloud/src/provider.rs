//! Read-only adapter. A folder is one staged metadata page; the parent stack is
//! a bounded continuation, never a path-derived item identity.

use crate::{
    DriveEntry, ICloudReadSession, IncompleteFolder, MAX_RANGE, ROOT_ID, SessionRejected, StaleRead,
};
use async_trait::async_trait;
use cirrove_auth::{CredentialVault, DesktopVault};
use cirrove_core::{
    CancellationToken, Change, ChangePage, Checkpoint, Cursor, DirectoryPage, FeedMode,
    MetadataProvider, Node, NodeKind, ProviderError, ReadProvider, Scope,
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OnceCell};

const PROVIDER_ID: &str = "icloud";
const COLLECTION: &str = "drive";
const MAX_CURSOR: usize = 128 * 1024;
const MAX_DEPTH: usize = 128;

/// An experimental iCloud adapter. Its exact-range guard is implemented, but
/// Apple's ETag revision behavior is not yet proven. The service can construct
/// it for a saved read-only account; the window flow still needs live validation.
pub struct ICloudDrive {
    scope: Scope,
    session: Mutex<SessionState>,
    index_mode: IndexMode,
    keyring_backed: bool,
    keyring_validated: OnceCell<()>,
}

enum SessionState {
    Ready(Box<ICloudReadSession>),
    Keyring {
        apple_id: String,
        credential_id: String,
        vault: Arc<dyn CredentialVault>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IndexMode {
    FullSnapshot,
    OnDemand,
}

impl ICloudDrive {
    /// Restore only a Cirrove-owned keyring value. The caller owns vault access
    /// and must never log or serialize the supplied secret outside that vault.
    pub fn from_session_snapshot(
        scope: Scope,
        apple_id: &str,
        snapshot: &SecretString,
    ) -> Result<Self, ProviderError> {
        Self::new(scope, apple_id, snapshot, IndexMode::FullSnapshot)
    }

    /// Seed the root immediately and fetch complete folders only when opened.
    /// This bounds initial work for large accounts while the full snapshot
    /// protocol remains available to the offline metadata probe.
    pub fn on_demand_from_session_snapshot(
        scope: Scope,
        apple_id: &str,
        snapshot: &SecretString,
    ) -> Result<Self, ProviderError> {
        Self::new(scope, apple_id, snapshot, IndexMode::OnDemand)
    }

    /// An authenticated foreground probe can mount without persisting Apple
    /// session material. The session disappears when that process exits.
    pub fn on_demand_from_live_session(
        scope: Scope,
        session: ICloudReadSession,
    ) -> Result<Self, ProviderError> {
        if scope.provider != PROVIDER_ID
            || scope.collection != COLLECTION
            || scope.account.is_empty()
        {
            return Err(ProviderError::Permission);
        }
        Ok(Self {
            scope,
            session: Mutex::new(SessionState::Ready(Box::new(session))),
            index_mode: IndexMode::OnDemand,
            keyring_backed: false,
            keyring_validated: OnceCell::new(),
        })
    }

    /// Defer the Secret Service request until a directory or file is opened.
    /// Constructing a provider remains synchronous for the service manager;
    /// neither secrets nor network requests enter its settings lock.
    pub fn on_demand_from_keyring(
        scope: Scope,
        apple_id: String,
        credential_id: String,
    ) -> Result<Self, ProviderError> {
        Self::on_demand_from_vault(scope, apple_id, credential_id, Arc::new(DesktopVault))
    }

    fn on_demand_from_vault(
        scope: Scope,
        apple_id: String,
        credential_id: String,
        vault: Arc<dyn CredentialVault>,
    ) -> Result<Self, ProviderError> {
        if scope.provider != PROVIDER_ID
            || scope.collection != COLLECTION
            || scope.account.is_empty()
            || apple_id.trim().is_empty()
            || uuid::Uuid::parse_str(&credential_id).is_err()
        {
            return Err(ProviderError::Permission);
        }
        Ok(Self {
            scope,
            session: Mutex::new(SessionState::Keyring {
                apple_id,
                credential_id,
                vault,
            }),
            index_mode: IndexMode::OnDemand,
            keyring_backed: true,
            keyring_validated: OnceCell::new(),
        })
    }

    fn new(
        scope: Scope,
        apple_id: &str,
        snapshot: &SecretString,
        index_mode: IndexMode,
    ) -> Result<Self, ProviderError> {
        if scope.provider != PROVIDER_ID
            || scope.collection != COLLECTION
            || scope.account.is_empty()
        {
            return Err(ProviderError::Permission);
        }
        let session = ICloudReadSession::from_session_snapshot(snapshot, apple_id)
            .map_err(|_| ProviderError::Authentication)?;
        Ok(Self {
            scope,
            session: Mutex::new(SessionState::Ready(Box::new(session))),
            index_mode,
            keyring_backed: false,
            keyring_validated: OnceCell::new(),
        })
    }

    async fn active_session(
        state: &mut SessionState,
    ) -> Result<&mut ICloudReadSession, ProviderError> {
        if let SessionState::Keyring {
            apple_id,
            credential_id,
            vault,
        } = state
        {
            let saved = vault
                .load(credential_id)
                .await
                .map_err(|_| ProviderError::Authentication)?
                .ok_or(ProviderError::Authentication)?;
            let restored = ICloudReadSession::from_session_snapshot(&saved, apple_id)
                .map_err(|_| ProviderError::Authentication)?;
            *state = SessionState::Ready(Box::new(restored));
        }
        match state {
            SessionState::Ready(session) => Ok(session),
            SessionState::Keyring { .. } => Err(ProviderError::Authentication),
        }
    }

    fn check_scope(&self, scope: &Scope) -> Result<(), ProviderError> {
        if *scope == self.scope {
            Ok(())
        } else {
            Err(ProviderError::Permission)
        }
    }

    async fn list_folder(
        &self,
        folder: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<DriveEntry>, ProviderError> {
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let mut state = self.session.lock().await;
                let session = Self::active_session(&mut state).await?;
                session.list_folder(folder).await.map_err(|error| map_read_error(&error))
            } => result,
        }
    }
}

fn map_read_error(error: &anyhow::Error) -> ProviderError {
    if error.downcast_ref::<SessionRejected>().is_some() {
        ProviderError::Authentication
    } else if error.downcast_ref::<IncompleteFolder>().is_some() {
        ProviderError::Protocol("incomplete iCloud folder listing")
    } else if error.downcast_ref::<StaleRead>().is_some() {
        ProviderError::VersionChanged
    } else {
        ProviderError::Unavailable
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Frame {
    folder: String,
    next_folder: usize,
    fingerprint: String,
}

#[derive(Serialize, Deserialize)]
struct Walk {
    scope: Scope,
    frames: Vec<Frame>,
}

impl Walk {
    fn new(scope: &Scope, items: &[DriveEntry]) -> Result<Self, ProviderError> {
        Ok(Self {
            scope: scope.clone(),
            frames: vec![Frame {
                folder: ROOT_ID.into(),
                next_folder: 0,
                fingerprint: fingerprint(items)?,
            }],
        })
    }

    fn decode(cursor: &Cursor, scope: &Scope) -> Result<Self, ProviderError> {
        if cursor.0.len() > MAX_CURSOR {
            return Err(ProviderError::Protocol("iCloud continuation exceeds limit"));
        }
        let walk: Self = serde_json::from_str(&cursor.0)
            .map_err(|_| ProviderError::Protocol("invalid iCloud continuation"))?;
        if walk.scope != *scope
            || walk.frames.is_empty()
            || walk.frames.len() > MAX_DEPTH
            || walk.frames[0].folder != ROOT_ID
        {
            return Err(ProviderError::Protocol("invalid iCloud continuation scope"));
        }
        let mut seen = HashSet::new();
        for frame in &walk.frames {
            if !frame.folder.starts_with("FOLDER::")
                || frame.folder.len() > 512
                || frame.fingerprint.len() != 64
                || !seen.insert(&frame.folder)
            {
                return Err(ProviderError::Protocol("invalid iCloud continuation frame"));
            }
        }
        Ok(walk)
    }

    fn encode(&self) -> Result<Cursor, ProviderError> {
        let value = serde_json::to_string(self)
            .map_err(|_| ProviderError::Protocol("iCloud continuation encoding"))?;
        if value.len() > MAX_CURSOR {
            return Err(ProviderError::Protocol("iCloud continuation exceeds limit"));
        }
        Ok(Cursor(value))
    }

    fn next_folder(&mut self, items: &[DriveEntry]) -> Result<Option<String>, ProviderError> {
        let frame = self
            .frames
            .last_mut()
            .ok_or(ProviderError::Protocol("empty iCloud continuation"))?;
        if fingerprint(items)? != frame.fingerprint {
            return Err(ProviderError::CursorExpired);
        }
        let mut folders: Vec<_> = items.iter().filter(|item| item.is_folder()).collect();
        folders.sort_unstable_by(|left, right| left.drivewsid.cmp(&right.drivewsid));
        if frame.next_folder > folders.len() {
            return Err(ProviderError::Protocol("invalid iCloud folder position"));
        }
        let next = folders
            .get(frame.next_folder)
            .map(|item| item.drivewsid.clone());
        if next.is_some() {
            frame.next_folder += 1;
        }
        Ok(next)
    }

    fn descend(&mut self, folder: String, items: &[DriveEntry]) -> Result<(), ProviderError> {
        if self.frames.len() >= MAX_DEPTH || self.frames.iter().any(|frame| frame.folder == folder)
        {
            return Err(ProviderError::Protocol(
                "iCloud folder ancestry is cyclic or too deep",
            ));
        }
        self.frames.push(Frame {
            folder,
            next_folder: 0,
            fingerprint: fingerprint(items)?,
        });
        Ok(())
    }
}

fn fingerprint(items: &[DriveEntry]) -> Result<String, ProviderError> {
    let mut ordered: Vec<_> = items.iter().collect();
    ordered.sort_unstable_by(|left, right| left.drivewsid.cmp(&right.drivewsid));
    let value = serde_json::to_vec(&ordered)
        .map_err(|_| ProviderError::Protocol("iCloud listing fingerprint"))?;
    Ok(hex::encode(Sha256::digest(value)))
}

fn directory_nodes(parent: &str, items: Vec<DriveEntry>) -> Result<Vec<Node>, ProviderError> {
    let mut nodes = Vec::with_capacity(items.len());
    let mut seen = HashSet::new();
    for item in items {
        if !seen.insert(item.drivewsid.clone()) || item.drivewsid == ROOT_ID {
            return Err(ProviderError::Protocol("duplicate iCloud item identity"));
        }
        let kind = match item.kind.as_str() {
            "FOLDER" | "APP_CONTAINER" | "APP_LIBRARY" => NodeKind::Folder,
            "FILE" => NodeKind::File,
            _ => return Err(ProviderError::Protocol("unsupported iCloud item kind")),
        };
        let name = item.display_name();
        if name.is_empty() || name.contains('/') {
            return Err(ProviderError::Protocol("invalid iCloud item name"));
        }
        nodes.push(Node {
            id: item.drivewsid,
            parent_id: Some(parent.into()),
            name,
            kind,
            size: item.size,
            modified_unix: 0,
            etag: (!item.etag.is_empty()).then_some(item.etag),
            content_version: None,
            target: None,
            package: false,
        });
    }
    Ok(nodes)
}

fn nodes(parent: &str, items: Vec<DriveEntry>) -> Result<Vec<Change>, ProviderError> {
    Ok(directory_nodes(parent, items)?
        .into_iter()
        .map(Change::Upsert)
        .collect())
}

fn root_node() -> Node {
    Node {
        id: ROOT_ID.into(),
        parent_id: None,
        name: "iCloud Drive".into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    }
}

fn root() -> Change {
    Change::Upsert(root_node())
}

fn on_demand_page(cursor: Option<&Cursor>) -> Result<ChangePage, ProviderError> {
    const READY: &str = "on-demand-root-ready";
    let changes = match cursor {
        None => vec![root()],
        Some(value) if value.0 == READY => Vec::new(),
        Some(_) => return Err(ProviderError::CursorExpired),
    };
    Ok(ChangePage {
        changes,
        checkpoint: Checkpoint::Complete(Cursor(READY.into())),
    })
}

#[async_trait]
impl MetadataProvider for ICloudDrive {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn feed_mode(&self) -> FeedMode {
        match self.index_mode {
            IndexMode::FullSnapshot => FeedMode::FullSnapshot,
            IndexMode::OnDemand => FeedMode::Incremental,
        }
    }

    async fn changes(
        &self,
        scope: &Scope,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        self.check_scope(scope)?;
        if self.index_mode == IndexMode::OnDemand {
            if self.keyring_backed {
                self.keyring_validated
                    .get_or_try_init(|| async {
                        self.list_folder(ROOT_ID, cancel).await.map(|_| ())
                    })
                    .await?;
            }
            return on_demand_page(cursor);
        }
        if let Some(cursor) = cursor {
            let mut walk = Walk::decode(cursor, scope)?;
            let parent = walk
                .frames
                .last()
                .ok_or(ProviderError::Protocol("empty iCloud continuation"))?
                .folder
                .clone();
            let entries = self.list_folder(&parent, cancel).await?;
            if let Some(folder) = walk.next_folder(&entries)? {
                let children = self.list_folder(&folder, cancel).await?;
                walk.descend(folder.clone(), &children)?;
                let changes = nodes(&folder, children)?;
                return Ok(ChangePage {
                    changes,
                    checkpoint: Checkpoint::Continue(walk.encode()?),
                });
            }
            walk.frames.pop();
            let checkpoint = if walk.frames.is_empty() {
                Checkpoint::Complete(Cursor("snapshot-complete".into()))
            } else {
                Checkpoint::Continue(walk.encode()?)
            };
            return Ok(ChangePage {
                changes: Vec::new(),
                checkpoint,
            });
        }
        let entries = self.list_folder(ROOT_ID, cancel).await?;
        let walk = Walk::new(scope, &entries)?;
        let mut changes = vec![root()];
        changes.extend(nodes(ROOT_ID, entries)?);
        Ok(ChangePage {
            changes,
            checkpoint: Checkpoint::Continue(walk.encode()?),
        })
    }
}

#[async_trait]
impl ReadProvider for ICloudDrive {
    fn directory_fetch_timeout(&self, _parent: Option<&Node>) -> Duration {
        // A live folder listing has already needed more than the shared 60 s
        // default. The transport itself caps each request at 90 s.
        Duration::from_secs(100)
    }

    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        _cancel: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        self.check_scope(scope)?;
        // Apple rejected the individual-item endpoint in the live probe. The
        // service already owns nodes it learned from a completed folder page;
        // a cold unknown ID must not be guessed from a path.
        if id == ROOT_ID {
            Ok(root_node())
        } else {
            Err(ProviderError::Unavailable)
        }
    }

    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        self.check_scope(scope)?;
        if cursor.is_some() || !parent.starts_with("FOLDER::") {
            return Err(ProviderError::Protocol("invalid iCloud directory request"));
        }
        let nodes = directory_nodes(parent, self.list_folder(parent, cancel).await?)?;
        Ok(DirectoryPage { nodes, next: None })
    }

    async fn read_range(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        self.check_scope(scope)?;
        if node.kind != NodeKind::File || !node.id.starts_with("FILE::") {
            return Err(ProviderError::Protocol("invalid iCloud file request"));
        }
        let parent = node
            .parent_id
            .as_deref()
            .filter(|id| id.starts_with("FOLDER::"))
            .ok_or(ProviderError::Protocol("iCloud file has no known parent"))?;
        let etag = node
            .etag
            .as_deref()
            .filter(|etag| !etag.is_empty())
            .ok_or(ProviderError::Protocol("iCloud file has no revision"))?;
        if length == 0 {
            return Ok(Vec::new());
        }
        if length > MAX_RANGE {
            return Err(ProviderError::Protocol("iCloud range exceeds limit"));
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let mut state = self.session.lock().await;
                let session = Self::active_session(&mut state).await?;
                session.read_range_in_folder_for_revision(parent, &node.id, offset, length, Some((etag, node.size))).await.map_err(|error| map_read_error(&error))
            } => result,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn listing_fixture(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 4096];
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0 && request.len() + count <= 16 * 1024);
                request.extend_from_slice(&chunk[..count]);
                let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let header_end = header_end + 4;
                let headers = std::str::from_utf8(&request[..header_end]).unwrap();
                assert!(headers.starts_with("POST /retrieveItemDetailsInFolders HTTP/1.1"));
                let content_length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.parse().ok())
                    })
                    .unwrap();
                if request.len() - header_end < content_length {
                    continue;
                }
                let payload: serde_json::Value =
                    serde_json::from_slice(&request[header_end..header_end + content_length])
                        .unwrap();
                assert_eq!(payload[0]["drivewsid"], ROOT_ID);
                assert_eq!(payload[0]["partialData"], false);
                assert_eq!(payload[0]["includeHierarchy"], false);
                break;
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        });
        format!("http://{address}/")
    }

    #[tokio::test]
    async fn complete_apple_folder_becomes_one_provider_directory_page() {
        let endpoint = listing_fixture(
            r#"[{"drivewsid":"FOLDER::com.apple.CloudDocs::root","name":"root","type":"FOLDER","numberOfItems":2,"items":[{"drivewsid":"FOLDER::zone::opaque-a","name":"Projects","type":"FOLDER","etag":"folder-revision"},{"drivewsid":"FILE::zone::opaque-b","name":"Report","extension":"txt","type":"FILE","size":3,"etag":"file-revision"}]}]"#,
        )
        .await;
        let mut session = ICloudReadSession::new().unwrap();
        session.drive_endpoint = Some(url::Url::parse(&endpoint).unwrap());
        let scope = Scope {
            account: "synthetic-account".into(),
            provider: PROVIDER_ID.into(),
            collection: COLLECTION.into(),
        };
        let drive = ICloudDrive::on_demand_from_live_session(scope.clone(), session).unwrap();
        let page = drive
            .children(&scope, ROOT_ID, None, &CancellationToken::new())
            .await
            .unwrap();
        assert!(page.next.is_none());
        assert_eq!(page.nodes.len(), 2);
        assert_eq!(page.nodes[0].id, "FOLDER::zone::opaque-a");
        assert_eq!(page.nodes[0].parent_id.as_deref(), Some(ROOT_ID));
        assert_eq!(page.nodes[0].kind, NodeKind::Folder);
        assert_eq!(page.nodes[1].name, "Report.txt");
        assert_eq!(page.nodes[1].etag.as_deref(), Some("file-revision"));
        assert_eq!(page.nodes[1].size, 3);
    }

    #[tokio::test]
    async fn incomplete_apple_folder_is_not_published_as_a_complete_page() {
        let endpoint = listing_fixture(
            r#"[{"drivewsid":"FOLDER::com.apple.CloudDocs::root","name":"root","type":"FOLDER","numberOfItems":2,"items":[{"drivewsid":"FILE::zone::only-one","name":"One","type":"FILE","size":1,"etag":"revision"}]}]"#,
        )
        .await;
        let mut session = ICloudReadSession::new().unwrap();
        session.drive_endpoint = Some(url::Url::parse(&endpoint).unwrap());
        let scope = Scope {
            account: "synthetic-account".into(),
            provider: PROVIDER_ID.into(),
            collection: COLLECTION.into(),
        };
        let drive = ICloudDrive::on_demand_from_live_session(scope.clone(), session).unwrap();
        assert!(matches!(
            drive
                .children(&scope, ROOT_ID, None, &CancellationToken::new())
                .await,
            Err(ProviderError::Protocol("incomplete iCloud folder listing"))
        ));
    }

    #[test]
    fn rejected_apple_session_requires_reauthentication() {
        for status in [
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::FORBIDDEN,
        ] {
            let error = crate::drive_request_failure(status, "iCloud Drive listing");
            assert!(matches!(
                map_read_error(&error),
                ProviderError::Authentication
            ));
        }
        let error = crate::drive_request_failure(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "iCloud Drive listing",
        );
        assert!(matches!(map_read_error(&error), ProviderError::Unavailable));
    }

    struct MissingVault;

    #[async_trait]
    impl CredentialVault for MissingVault {
        async fn load(&self, _key: &str) -> anyhow::Result<Option<SecretString>> {
            Ok(None)
        }
        async fn save(&self, _key: &str, _value: SecretString) -> anyhow::Result<()> {
            panic!("read-only feed must not save credentials")
        }
        async fn remove(&self, _key: &str) -> anyhow::Result<()> {
            panic!("read-only feed must not remove credentials")
        }
    }

    #[tokio::test]
    async fn keyring_feed_does_not_publish_root_without_a_session() {
        let scope = Scope {
            account: "account".into(),
            provider: PROVIDER_ID.into(),
            collection: COLLECTION.into(),
        };
        let drive = ICloudDrive::on_demand_from_vault(
            scope.clone(),
            "person@example.com".into(),
            uuid::Uuid::new_v4().to_string(),
            Arc::new(MissingVault),
        )
        .unwrap();
        assert!(matches!(
            drive.changes(&scope, None, &CancellationToken::new()).await,
            Err(ProviderError::Authentication)
        ));
    }

    fn entry(id: &str, kind: &str) -> DriveEntry {
        DriveEntry {
            drivewsid: id.into(),
            docwsid: String::new(),
            item_id: String::new(),
            zone: String::new(),
            name: id.into(),
            extension: String::new(),
            parent_id: String::new(),
            etag: "v1".into(),
            kind: kind.into(),
            size: 1,
            items: Vec::new(),
            number_of_items: None,
        }
    }

    #[test]
    fn snapshot_cursor_resumes_folder_order_and_rejects_changed_listing() {
        let scope = Scope {
            account: "account".into(),
            provider: PROVIDER_ID.into(),
            collection: COLLECTION.into(),
        };
        let listing = vec![
            entry("FOLDER::zone::one", "FOLDER"),
            entry("FILE::zone::two", "FILE"),
        ];
        let mut walk = Walk::new(&scope, &listing).unwrap();
        assert_eq!(
            walk.next_folder(&listing).unwrap().as_deref(),
            Some("FOLDER::zone::one")
        );
        let mut resumed = Walk::decode(&walk.encode().unwrap(), &scope).unwrap();
        assert!(resumed.next_folder(&listing).unwrap().is_none());
        let changed = vec![entry("FOLDER::zone::other", "FOLDER")];
        assert!(matches!(
            resumed.next_folder(&changed),
            Err(ProviderError::CursorExpired)
        ));
    }

    #[test]
    fn snapshot_cursor_survives_reordered_unchanged_listing() {
        let scope = Scope {
            account: "account".into(),
            provider: PROVIDER_ID.into(),
            collection: COLLECTION.into(),
        };
        let first = vec![
            entry("FOLDER::zone::two", "FOLDER"),
            entry("FILE::zone::note", "FILE"),
            entry("FOLDER::zone::one", "FOLDER"),
        ];
        let mut reordered = first.clone();
        reordered.reverse();
        let mut walk = Walk::new(&scope, &first).unwrap();
        assert_eq!(
            walk.next_folder(&first).unwrap().as_deref(),
            Some("FOLDER::zone::one")
        );
        let mut resumed = Walk::decode(&walk.encode().unwrap(), &scope).unwrap();
        assert_eq!(
            resumed.next_folder(&reordered).unwrap().as_deref(),
            Some("FOLDER::zone::two")
        );
        assert!(resumed.next_folder(&first).unwrap().is_none());
    }

    #[test]
    fn nodes_keep_opaque_identity_and_refuse_duplicate_ids() {
        let item = entry("FILE::zone::opaque", "FILE");
        let changes = nodes(ROOT_ID, vec![item.clone()]).unwrap();
        let Change::Upsert(node) = &changes[0] else {
            panic!("expected item")
        };
        assert_eq!(node.id, item.drivewsid);
        assert_eq!(node.parent_id.as_deref(), Some(ROOT_ID));
        assert!(nodes(ROOT_ID, vec![item.clone(), item]).is_err());
    }

    #[test]
    fn on_demand_feed_seeds_root_once_and_rejects_other_cursors() {
        let first = on_demand_page(None).unwrap();
        assert_eq!(first.changes, vec![root()]);
        let cursor = first.checkpoint.cursor().clone();
        let repeat = on_demand_page(Some(&cursor)).unwrap();
        assert!(repeat.changes.is_empty());
        assert!(repeat.checkpoint.complete());
        assert!(matches!(
            on_demand_page(Some(&Cursor("unrelated".into()))),
            Err(ProviderError::CursorExpired)
        ));
    }
}
