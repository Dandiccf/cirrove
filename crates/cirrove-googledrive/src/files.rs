use super::*;
use async_trait::async_trait;
use cirrove_core::{
    CancellationToken, CollectionInfo, Cursor, DirectoryPage, Node, NodeKind, Priority,
    ReadProvider, RemoteRef,
    reads::{ReadIdentity, ReadSession, ReadWindowSink},
};
use serde::Deserialize;

pub(super) const FIELDS: &str = "id,name,mimeType,parents,size,version,headRevisionId,modifiedTime,trashed,driveId,capabilities(canDownload),shortcutDetails(targetId,targetMimeType,targetResourceKey)";
const FOLDER: &str = "application/vnd.google-apps.folder";
const SHORTCUT: &str = "application/vnd.google-apps.shortcut";
const NATIVE_DOCUMENT: &str = "application/vnd.google-apps.document";
const NATIVE_SPREADSHEET: &str = "application/vnd.google-apps.spreadsheet";
const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const MAX_NATIVE_EXPORT_BYTES: usize = 10 * 1024 * 1024;
const EXPORT_DOCX_PREFIX: &str = "cirrove_export_docx_";
const EXPORT_XLSX_PREFIX: &str = "cirrove_export_xlsx_";
const NATIVE_LINK_PREFIX: &str = "cirrove_native_link_";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeKind {
    Document,
    Spreadsheet,
}

impl NativeKind {
    fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            NATIVE_DOCUMENT => Some(Self::Document),
            NATIVE_SPREADSHEET => Some(Self::Spreadsheet),
            _ => None,
        }
    }

    fn package_extension(self) -> &'static str {
        match self {
            Self::Document => ".gdoc",
            Self::Spreadsheet => ".gsheet",
        }
    }

    fn export_mime(self) -> &'static str {
        match self {
            Self::Document => DOCX,
            Self::Spreadsheet => XLSX,
        }
    }

    fn export_name(self) -> &'static str {
        match self {
            Self::Document => "Document.docx",
            Self::Spreadsheet => "Spreadsheet.xlsx",
        }
    }

    fn version_prefix(self) -> &'static str {
        match self {
            Self::Document => "google-native-document:",
            Self::Spreadsheet => "google-native-spreadsheet:",
        }
    }

    fn export_version_prefix(self) -> &'static str {
        match self {
            Self::Document => "google-export-docx:",
            Self::Spreadsheet => "google-export-xlsx:",
        }
    }

    fn export_id_prefix(self) -> &'static str {
        match self {
            Self::Document => EXPORT_DOCX_PREFIX,
            Self::Spreadsheet => EXPORT_XLSX_PREFIX,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct File {
    pub id: String,
    pub(super) name: String,
    pub(super) mime_type: String,
    #[serde(default)]
    pub(super) parents: Vec<String>,
    pub(super) size: Option<String>,
    pub(super) version: Option<String>,
    pub(super) head_revision_id: Option<String>,
    modified_time: Option<String>,
    #[serde(default)]
    pub trashed: bool,
    pub drive_id: Option<String>,
    capabilities: Option<Capabilities>,
    shortcut_details: Option<Shortcut>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    can_download: Option<bool>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Shortcut {
    target_id: String,
    target_mime_type: String,
    target_resource_key: Option<String>,
}

impl File {
    pub(super) fn node(&self, collection: &str) -> Result<Node, ProviderError> {
        valid_id(&self.id)?;
        if self.trashed {
            return Err(ProviderError::NotFound);
        }
        if self.drive_id.is_some() {
            return Err(ProviderError::Permission);
        }
        if self.parents.len() > 1 {
            return Err(ProviderError::Protocol("multiple Google parents"));
        }
        for parent in &self.parents {
            valid_id(parent)?;
        }
        let version = self
            .version
            .as_deref()
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .ok_or(ProviderError::Protocol("missing Google file version"))?;
        let native = NativeKind::from_mime(&self.mime_type);
        let link = document_link(&self.id, &self.mime_type);
        let (kind, size, target) = if self.mime_type == FOLDER || native.is_some() {
            (NodeKind::Folder, 0, None)
        } else if self.mime_type == SHORTCUT {
            let shortcut = self
                .shortcut_details
                .as_ref()
                .ok_or(ProviderError::Protocol("missing shortcut target"))?;
            valid_id(&shortcut.target_id)?;
            // Resource-key links need a transport capability not carried by RemoteRef.
            // Keep them as browser links rather than dropping the key or persisting it.
            if shortcut.target_resource_key.is_some() {
                return self.browser_link_node(version, collection);
            }
            (
                NodeKind::Shortcut,
                0,
                Some(Box::new(RemoteRef {
                    collection: collection.into(),
                    item: shortcut.target_id.clone(),
                    kind: Some(if shortcut.target_mime_type == FOLDER {
                        NodeKind::Folder
                    } else {
                        NodeKind::File
                    }),
                })),
            )
        } else if let Some(ref bytes) = link {
            (NodeKind::File, bytes.len() as u64, None)
        } else {
            let size = self
                .size
                .as_deref()
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or(ProviderError::Protocol("missing Google file size"))?;
            (NodeKind::File, size, None)
        };
        let content_version = if let Some(native) = native {
            format!("{}{version}", native.version_prefix())
        } else if self.mime_type != FOLDER && self.mime_type != SHORTCUT && link.is_none() {
            let revision = self
                .head_revision_id
                .as_deref()
                .filter(|revision| !revision.is_empty() && revision.len() <= MAX_TOKEN_BYTES)
                .ok_or(ProviderError::Protocol(
                    "missing Google binary head revision",
                ))?;
            format!("google-revision:{revision}")
        } else {
            format!("google-version:{version}")
        };
        Ok(Node {
            id: self.id.clone(),
            parent_id: self.parents.first().cloned(),
            name: native.map_or_else(
                || projected_name(&self.name, &self.id, link.is_some()),
                |kind| {
                    projected_name_with_extension(&self.name, &self.id, kind.package_extension())
                },
            ),
            kind,
            size,
            modified_unix: self
                .modified_time
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map_or(0, |t| t.timestamp().max(0) as u64),
            etag: (link.is_none() && self.mime_type != SHORTCUT)
                .then(|| version_precondition(version)),
            content_version: Some(content_version),
            target,
            package: native.is_some(),
        })
    }
    fn browser_link_node(&self, version: &str, _collection: &str) -> Result<Node, ProviderError> {
        Ok(Node {
            id: self.id.clone(),
            parent_id: self.parents.first().cloned(),
            name: projected_name(&self.name, &self.id, true),
            kind: NodeKind::File,
            size: browser_link(&self.id).len() as u64,
            modified_unix: 0,
            etag: None,
            content_version: Some(format!("google-version:{version}")),
            target: None,
            package: false,
        })
    }
    fn local_content(&self) -> Option<Vec<u8>> {
        if self.mime_type == SHORTCUT
            && self
                .shortcut_details
                .as_ref()
                .is_some_and(|s| s.target_resource_key.is_some())
        {
            Some(browser_link(&self.id))
        } else {
            document_link(&self.id, &self.mime_type)
        }
    }
}
fn browser_link(id: &str) -> Vec<u8> {
    format!("[InternetShortcut]\nURL=https://drive.google.com/open?id={id}\n").into_bytes()
}
fn document_link(id: &str, mime: &str) -> Option<Vec<u8>> {
    (mime.starts_with("application/vnd.google-apps.")
        && mime != FOLDER
        && mime != SHORTCUT
        && NativeKind::from_mime(mime).is_none())
    .then(|| browser_link(id))
}
/// Stable, page-independent names. Drive permits duplicate sibling names and '/'.
/// The full ID disambiguates even duplicates split across pages; extensions remain
/// at the end. No enumeration-order winner or truncated-ID collision is possible.
pub(super) fn projected_name(name: &str, id: &str, link: bool) -> String {
    let escaped = name
        .replace('%', "%25")
        .replace('/', "%2F")
        .replace('\0', "%00");
    let (stem, extension) = if link {
        (escaped.as_str(), ".url".to_owned())
    } else if let Some((stem, ext)) = escaped.rsplit_once('.') {
        if !stem.is_empty() && ext.len() <= 20 {
            (stem, format!(".{ext}"))
        } else {
            (escaped.as_str(), String::new())
        }
    } else {
        (escaped.as_str(), String::new())
    };
    let suffix = format!(" [{id}]{extension}");
    let mut end = stem.len().min(255 - suffix.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &stem[..end])
}

fn projected_name_with_extension(name: &str, id: &str, extension: &str) -> String {
    let escaped = name
        .replace('%', "%25")
        .replace('/', "%2F")
        .replace('\0', "%00");
    let suffix = format!(" [{id}]{extension}");
    let mut end = escaped.len().min(255 - suffix.len());
    while !escaped.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &escaped[..end])
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileList {
    #[serde(default)]
    pub files: Vec<File>,
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub incomplete_search: bool,
}
#[derive(serde::Serialize, Deserialize)]
struct DirectoryCursor {
    scope: Scope,
    parent: String,
    token: String,
}
impl GoogleDrive {
    pub async fn root(&self, cancel: &CancellationToken) -> Result<Node, ProviderError> {
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let _permit = self.budget.acquire(Priority::Interactive, cancel).await?;
                let file = self.file("root").await?;
                if file.mime_type != FOLDER || !file.parents.is_empty() { return Err(ProviderError::Protocol("invalid Google root")); }
                file.node(&file.id)
            } => result,
        }
    }
    pub async fn collection(
        &self,
        cancel: &CancellationToken,
    ) -> Result<CollectionInfo, ProviderError> {
        let root = self.root(cancel).await?;
        Ok(CollectionInfo {
            id: root.id,
            name: "My Drive".into(),
            drive_type: "my_drive".into(),
            web_url: "https://drive.google.com/drive/my-drive".into(),
        })
    }
    pub(super) async fn file(&self, id: &str) -> Result<File, ProviderError> {
        valid_id(id)?;
        let mut url = self.url(&["files", id])?;
        url.query_pairs_mut().append_pair("fields", FIELDS);
        let file: File = self.json(url).await?;
        if id != "root" && file.id != id {
            return Err(ProviderError::Protocol("Google returned a different item"));
        }
        Ok(file)
    }
    pub(super) async fn list_files(
        &self,
        parent: Option<&str>,
        token: Option<&str>,
    ) -> Result<FileList, ProviderError> {
        let mut url = self.url(&["files"])?;
        let query = if let Some(parent) = parent {
            valid_id(parent)?;
            format!("'{parent}' in parents and trashed = false")
        } else {
            "trashed = false".into()
        };
        url.query_pairs_mut().extend_pairs([
            ("q", query.as_str()),
            ("spaces", "drive"),
            ("corpora", "user"),
            ("pageSize", "1000"),
            (
                "fields",
                &format!("nextPageToken,incompleteSearch,files({FIELDS})"),
            ),
            ("includeItemsFromAllDrives", "false"),
        ]);
        if let Some(token) = token {
            valid_token(token)?;
            url.query_pairs_mut().append_pair("pageToken", token);
        }
        let page: FileList = self.json(url).await?;
        if page.incomplete_search {
            return Err(ProviderError::Protocol(
                "Google directory search incomplete",
            ));
        }
        if let Some(next) = &page.next_page_token {
            valid_token(next)?;
            if Some(next.as_str()) == token {
                return Err(ProviderError::Protocol("repeated Google page token"));
            }
        }
        Ok(page)
    }

    #[cfg(feature = "test-support")]
    pub async fn native_export_preflight(
        &self,
        cancel: &CancellationToken,
    ) -> Result<NativeExportPreflight, ProviderError> {
        use sha2::{Digest, Sha256};

        #[derive(Deserialize)]
        struct Revision {
            id: String,
        }
        #[derive(Deserialize)]
        struct Revisions {
            #[serde(default)]
            revisions: Vec<Revision>,
        }

        let mut token = None;
        let mut scanned_files = 0usize;
        let mut observations = Vec::new();
        for _ in 0..16 {
            let page = tokio::select! { biased;
                _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                page = self.list_files(None, token.as_deref()) => page?,
            };
            for file in page.files {
                scanned_files = scanned_files
                    .checked_add(1)
                    .ok_or(ProviderError::Protocol("Google preflight count overflow"))?;
                let (kind, native_kind) = match file.mime_type.as_str() {
                    NATIVE_DOCUMENT
                        if !observations
                            .iter()
                            .any(|seen: &NativeExportObservation| seen.kind == "document") =>
                    {
                        ("document", NativeKind::Document)
                    }
                    NATIVE_SPREADSHEET
                        if !observations
                            .iter()
                            .any(|seen: &NativeExportObservation| seen.kind == "spreadsheet") =>
                    {
                        ("spreadsheet", NativeKind::Spreadsheet)
                    }
                    _ => continue,
                };
                let version = file
                    .version
                    .as_deref()
                    .filter(|value| {
                        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                    })
                    .ok_or(ProviderError::Protocol("missing Google file version"))?
                    .to_owned();
                let metadata_size = file.size.as_deref().and_then(|size| size.parse().ok());
                let head_revision_available = file
                    .head_revision_id
                    .as_deref()
                    .is_some_and(|revision| !revision.is_empty());

                let mut revisions_url = self.url(&["files", &file.id, "revisions"])?;
                revisions_url
                    .query_pairs_mut()
                    .extend_pairs([("pageSize", "1"), ("fields", "revisions(id)")]);
                let revisions: Revisions = self.json(revisions_url).await?;
                let listed_revision_available = revisions
                    .revisions
                    .first()
                    .is_some_and(|revision| !revision.id.is_empty());

                let parent = file.node(&self.collection)?;
                if native_parent_version(&parent, native_kind)? != version {
                    return Err(ProviderError::Protocol(
                        "Google native package lost its metadata version",
                    ));
                }
                let page = self.native_package_children(&parent, None, cancel).await?;
                let export_node = page
                    .nodes
                    .iter()
                    .find(|node| node.id.starts_with(native_kind.export_id_prefix()))
                    .ok_or(ProviderError::Protocol(
                        "Google native package omitted its export",
                    ))?;
                let identity = format!(
                    "{}:{}",
                    export_node.id,
                    export_node
                        .content_version
                        .as_deref()
                        .ok_or(ProviderError::Protocol("missing Google export version"))?
                );
                let export =
                    self.cached_native_export(&identity)
                        .await
                        .ok_or(ProviderError::Protocol(
                            "Google native package omitted staged bytes",
                        ))?;
                let version_stable = true;
                let export_bytes = export_node.size;
                observations.push(NativeExportObservation {
                    kind,
                    metadata_size,
                    export_bytes,
                    export_sha256: hex::encode(Sha256::digest(export.as_ref())),
                    metadata_matches_export: metadata_size == Some(export_bytes),
                    version_stable,
                    head_revision_available,
                    listed_revision_available,
                });
                if observations.len() == 2 {
                    return Ok(NativeExportPreflight {
                        scanned_files,
                        observations,
                    });
                }
            }
            token = page.next_page_token;
            if token.is_none() {
                return Ok(NativeExportPreflight {
                    scanned_files,
                    observations,
                });
            }
        }
        Err(ProviderError::Protocol(
            "Google native export preflight exceeded its page bound",
        ))
    }
}

struct NativeExport {
    node: Node,
    bytes: Arc<[u8]>,
}

fn derived_representation_id(prefix: &str, source_id: &str) -> String {
    use sha2::{Digest, Sha256};

    format!(
        "{prefix}{}",
        hex::encode(Sha256::digest(source_id.as_bytes()))
    )
}

fn native_parent_version(parent: &Node, kind: NativeKind) -> Result<&str, ProviderError> {
    if parent.kind != NodeKind::Folder || !parent.package {
        return Err(ProviderError::Protocol("invalid Google native package"));
    }
    parent
        .content_version
        .as_deref()
        .and_then(|version| version.strip_prefix(kind.version_prefix()))
        .filter(|version| !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or(ProviderError::Protocol(
            "invalid Google native package version",
        ))
}

fn native_kind_from_parent(parent: &Node) -> Option<NativeKind> {
    let version = parent.content_version.as_deref()?;
    if version.starts_with(NativeKind::Document.version_prefix()) {
        Some(NativeKind::Document)
    } else if version.starts_with(NativeKind::Spreadsheet.version_prefix()) {
        Some(NativeKind::Spreadsheet)
    } else {
        None
    }
}

impl GoogleDrive {
    async fn cached_native_export(&self, identity: &str) -> Option<Arc<[u8]>> {
        let cache = self.native_exports.lock().await;
        cache
            .iter()
            .find(|entry| entry.identity == identity)
            .map(|entry| entry.bytes.clone())
    }

    async fn remember_native_export(&self, identity: String, bytes: Arc<[u8]>) {
        let mut cache = self.native_exports.lock().await;
        cache.retain(|entry| entry.identity != identity);
        cache.push_front(super::NativeExportCacheEntry { identity, bytes });
        while cache.len() > 2 {
            cache.pop_back();
        }
    }

    async fn cached_native_materialization(
        &self,
        source_id: &str,
        kind: NativeKind,
        expected_version: &str,
        modified_unix: u64,
    ) -> Option<NativeExport> {
        let id = derived_representation_id(kind.export_id_prefix(), source_id);
        let prefix = format!("{id}:{}{expected_version}:", kind.export_version_prefix());
        let cache = self.native_exports.lock().await;
        let entry = cache
            .iter()
            .find(|entry| entry.identity.starts_with(&prefix))?;
        let digest = entry.identity.strip_prefix(&prefix)?;
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        Some(NativeExport {
            node: Node {
                id,
                parent_id: Some(source_id.to_owned()),
                name: kind.export_name().into(),
                kind: NodeKind::File,
                size: entry.bytes.len() as u64,
                modified_unix,
                etag: None,
                content_version: Some(format!(
                    "{}{expected_version}:{digest}",
                    kind.export_version_prefix()
                )),
                target: None,
                package: false,
            },
            bytes: entry.bytes.clone(),
        })
    }

    async fn materialize_native_export(
        &self,
        source_id: &str,
        kind: NativeKind,
        expected_version: &str,
        modified_unix: u64,
    ) -> Result<NativeExport, ProviderError> {
        use sha2::{Digest, Sha256};

        valid_id(source_id)?;
        let before = self.file(source_id).await?;
        if NativeKind::from_mime(&before.mime_type) != Some(kind)
            || before.version.as_deref() != Some(expected_version)
        {
            return Err(ProviderError::VersionChanged);
        }
        let mut export_url = self.url(&["files", source_id, "export"])?;
        export_url
            .query_pairs_mut()
            .append_pair("mimeType", kind.export_mime());
        let bytes: Arc<[u8]> = super::transport::body(
            self.response(export_url, None).await?,
            MAX_NATIVE_EXPORT_BYTES,
        )
        .await?
        .into();
        let after = self.file(source_id).await?;
        if NativeKind::from_mime(&after.mime_type) != Some(kind)
            || after.version.as_deref() != Some(expected_version)
        {
            return Err(ProviderError::VersionChanged);
        }
        let digest = hex::encode(Sha256::digest(bytes.as_ref()));
        let id = derived_representation_id(kind.export_id_prefix(), source_id);
        let content_version = format!(
            "{}{expected_version}:{digest}",
            kind.export_version_prefix()
        );
        let node = Node {
            id: id.clone(),
            parent_id: Some(source_id.to_owned()),
            name: kind.export_name().into(),
            kind: NodeKind::File,
            size: bytes.len() as u64,
            modified_unix,
            etag: None,
            content_version: Some(content_version.clone()),
            target: None,
            package: false,
        };
        self.remember_native_export(format!("{id}:{content_version}"), bytes.clone())
            .await;
        Ok(NativeExport { node, bytes })
    }

    async fn native_package_children(
        &self,
        parent: &Node,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        if cursor.is_some() {
            return Err(ProviderError::Protocol(
                "Google native package has no continuation",
            ));
        }
        let kind = native_kind_from_parent(parent).ok_or(ProviderError::Protocol(
            "unknown Google native package kind",
        ))?;
        let version = native_parent_version(parent, kind)?;
        let export = if let Some(export) = self
            .cached_native_materialization(&parent.id, kind, version, parent.modified_unix)
            .await
        {
            export
        } else {
            tokio::select! { biased;
                _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                result = async {
                    let _permit = self.budget.acquire(Priority::Content, cancel).await?;
                    self.materialize_native_export(&parent.id, kind, version, parent.modified_unix).await
                } => result?,
            }
        };
        let link_id = derived_representation_id(NATIVE_LINK_PREFIX, &parent.id);
        let link = browser_link(&parent.id);
        let link_node = Node {
            id: link_id,
            parent_id: Some(parent.id.clone()),
            name: "Open in Google.url".into(),
            kind: NodeKind::File,
            size: link.len() as u64,
            modified_unix: parent.modified_unix,
            etag: None,
            content_version: Some("google-native-link:v1".into()),
            target: None,
            package: false,
        };
        Ok(DirectoryPage {
            nodes: vec![export.node, link_node],
            next: None,
        })
    }

    fn native_representation<'a>(
        &self,
        node: &'a Node,
    ) -> Result<Option<(NativeKind, &'a str, &'a str)>, ProviderError> {
        let Some(source_id) = node.parent_id.as_deref() else {
            return Ok(None);
        };
        let kind = if node.id.starts_with(EXPORT_DOCX_PREFIX) {
            NativeKind::Document
        } else if node.id.starts_with(EXPORT_XLSX_PREFIX) {
            NativeKind::Spreadsheet
        } else {
            return Ok(None);
        };
        valid_id(source_id)?;
        if node.id != derived_representation_id(kind.export_id_prefix(), source_id) {
            return Err(ProviderError::Permission);
        }
        let content_version = node
            .content_version
            .as_deref()
            .ok_or(ProviderError::Protocol("missing Google export version"))?;
        let remainder = content_version
            .strip_prefix(kind.export_version_prefix())
            .ok_or(ProviderError::Protocol("invalid Google export version"))?;
        let (version, digest) = remainder
            .split_once(':')
            .ok_or(ProviderError::Protocol("invalid Google export version"))?;
        if version.is_empty()
            || !version.bytes().all(|byte| byte.is_ascii_digit())
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ProviderError::Protocol("invalid Google export version"));
        }
        Ok(Some((kind, version, content_version)))
    }
}

struct GoogleReadSession {
    drive: GoogleDrive,
    identity: ReadIdentity,
    node: Node,
}

#[async_trait]
impl ReadSession for GoogleReadSession {
    fn identity(&self) -> &ReadIdentity {
        &self.identity
    }

    fn window_limit(&self) -> u32 {
        8 * 1024 * 1024
    }

    async fn read_window(
        &self,
        offset: u64,
        length: u32,
        sink: &mut dyn ReadWindowSink,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        if length > self.window_limit() {
            return Err(ProviderError::Protocol("invalid Google read window"));
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let _permit = self.drive.budget.acquire(Priority::Content, cancel).await?;
                let before = self.drive.file(&self.node.id).await?;
                let current = before.node(&self.identity.scope.collection)?;
                if current.kind != NodeKind::File
                    || current.content_revision() != self.node.content_revision()
                    || current.size != self.node.size
                {
                    return Err(ProviderError::VersionChanged);
                }
                if offset >= self.node.size || length == 0 {
                    return Ok(());
                }
                let count = (self.node.size - offset).min(u64::from(length));
                if let Some(content) = before.local_content() {
                    for chunk in content[offset as usize..(offset + count) as usize].chunks(64 * 1024) {
                        sink.write_chunk(chunk).await?;
                    }
                    return Ok(());
                }
                if !before.capabilities.as_ref().is_some_and(|c| c.can_download == Some(true)) {
                    return Err(ProviderError::Permission);
                }
                let end = offset + count - 1;
                let mut url = self.drive.url(&["files", &self.node.id])?;
                url.query_pairs_mut().append_pair("alt", "media");
                let mut response = self.drive.response(url, Some((offset, end))).await?;
                if response.headers().get("content-encoding").is_some_and(|v| v != "identity") {
                    return Err(ProviderError::Protocol("encoded Google range"));
                }
                if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                    let expected = format!("bytes {offset}-{end}/{}", self.node.size);
                    if response.headers().get("content-range").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
                        return Err(ProviderError::Protocol("incorrect Google content range"));
                    }
                } else if offset != 0 || count != self.node.size {
                    return Err(ProviderError::Protocol("Google ignored content range"));
                }
                if response.content_length().is_some_and(|length| length != count) {
                    return Err(ProviderError::Protocol("short Google content range"));
                }
                let mut received = 0_u64;
                while let Some(bytes) = response.chunk().await.map_err(|_| ProviderError::Unavailable)? {
                    if bytes.len() as u64 > count.saturating_sub(received) {
                        return Err(ProviderError::Protocol("long Google content range"));
                    }
                    for chunk in bytes.chunks(64 * 1024) {
                        sink.write_chunk(chunk).await?;
                    }
                    received += bytes.len() as u64;
                }
                if received != count {
                    return Err(ProviderError::Protocol("short Google content range"));
                }
                let after = self
                    .drive
                    .file(&self.node.id)
                    .await?
                    .node(&self.identity.scope.collection)?;
                if after.kind != NodeKind::File
                    || after.content_revision() != self.node.content_revision()
                    || after.size != self.node.size
                {
                    return Err(ProviderError::VersionChanged);
                }
                Ok(())
            } => result,
        }
    }

    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        self.drive
            .read_range(&self.identity.scope, &self.node, offset, length, cancel)
            .await
    }
}

#[async_trait]
impl ReadProvider for GoogleDrive {
    async fn open_read_session(
        &self,
        scope: &Scope,
        node: &Node,
        _cancel: &CancellationToken,
    ) -> Result<Option<Arc<dyn ReadSession>>, ProviderError> {
        self.check_scope(scope)?;
        if node.id.starts_with(EXPORT_DOCX_PREFIX)
            || node.id.starts_with(EXPORT_XLSX_PREFIX)
            || node.id.starts_with(NATIVE_LINK_PREFIX)
        {
            return Ok(None);
        }
        valid_id(&node.id)?;
        Ok(Some(Arc::new(GoogleReadSession {
            drive: self.clone(),
            identity: ReadIdentity::new(scope, node)?,
            node: node.clone(),
        })))
    }

    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        self.check_scope(scope)?;
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let _permit = self.budget.acquire(Priority::Interactive, cancel).await?;
                self.file(id).await?.node(&scope.collection)
            } => result,
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
        valid_id(parent)?;
        let cursor: Option<DirectoryCursor> = cursor
            .map(|c| {
                if c.0.len() > 2 * MAX_TOKEN_BYTES {
                    return Err(ProviderError::Protocol("invalid directory cursor"));
                }
                serde_json::from_str(&c.0)
                    .map_err(|_| ProviderError::Protocol("invalid directory cursor"))
            })
            .transpose()?;
        if cursor
            .as_ref()
            .is_some_and(|c| c.scope != *scope || c.parent != parent)
        {
            return Err(ProviderError::Permission);
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let _permit = self.budget.acquire(Priority::Interactive, cancel).await?;
                let page = self.list_files(Some(parent), cursor.as_ref().map(|c| c.token.as_str())).await?;
                let mut nodes = Vec::new();
                for file in page.files {
                    if file.drive_id.is_some() || file.trashed { continue; }
                    if file.parents.first().map(String::as_str) != Some(parent) { return Err(ProviderError::Protocol("Google returned a different parent")); }
                    nodes.push(file.node(&scope.collection)?);
                }
                let next = page.next_page_token.map(|token| serde_json::to_string(&DirectoryCursor { scope: scope.clone(), parent: parent.into(), token })
                    .map(Cursor).map_err(|_| ProviderError::Protocol("directory cursor encoding"))).transpose()?;
                Ok(DirectoryPage { nodes, next })
            } => result,
        }
    }
    async fn children_for_node(
        &self,
        scope: &Scope,
        parent: &Node,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        self.check_scope(scope)?;
        if parent.package && native_kind_from_parent(parent).is_some() {
            if parent.id == scope.collection {
                return Err(ProviderError::Protocol(
                    "Google root cannot be a native package",
                ));
            }
            return self.native_package_children(parent, cursor, cancel).await;
        }
        self.children(scope, &parent.id, cursor, cancel).await
    }
    async fn staged_content(
        &self,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Result<Option<Arc<[u8]>>, ProviderError> {
        self.check_scope(scope)?;
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let Some((_kind, _version, content_version)) = self.native_representation(node)? else {
            return Ok(None);
        };
        let identity = format!("{}:{content_version}", node.id);
        let bytes = self
            .cached_native_export(&identity)
            .await
            .ok_or(ProviderError::VersionChanged)?;
        if bytes.len() as u64 != node.size {
            return Err(ProviderError::VersionChanged);
        }
        Ok(Some(bytes))
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
        if node.kind != NodeKind::File || length > 8 * 1024 * 1024 {
            return Err(ProviderError::Protocol("invalid Google read range"));
        }
        if node.id.starts_with(NATIVE_LINK_PREFIX) {
            let source_id = node
                .parent_id
                .as_deref()
                .ok_or(ProviderError::Protocol("missing Google native link source"))?;
            valid_id(source_id)?;
            if node.id != derived_representation_id(NATIVE_LINK_PREFIX, source_id)
                || node.content_version.as_deref() != Some("google-native-link:v1")
            {
                return Err(ProviderError::Permission);
            }
            let bytes = browser_link(source_id);
            if node.size != bytes.len() as u64 {
                return Err(ProviderError::VersionChanged);
            }
            if offset >= node.size || length == 0 {
                return Ok(Vec::new());
            }
            let count = (node.size - offset).min(u64::from(length));
            return Ok(bytes[offset as usize..(offset + count) as usize].to_vec());
        }
        if let Some((kind, version, content_version)) = self.native_representation(node)? {
            let source_id = node
                .parent_id
                .as_deref()
                .ok_or(ProviderError::Protocol("missing Google export source"))?;
            let identity = format!("{}:{content_version}", node.id);
            let bytes = if let Some(bytes) = self.cached_native_export(&identity).await {
                bytes
            } else {
                let export = tokio::select! { biased;
                    _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                    result = async {
                        let _permit = self.budget.acquire(Priority::Content, cancel).await?;
                        self.materialize_native_export(source_id, kind, version, node.modified_unix).await
                    } => result?,
                };
                if export.node.id != node.id
                    || export.node.size != node.size
                    || export.node.content_version != node.content_version
                {
                    return Err(ProviderError::VersionChanged);
                }
                export.bytes
            };
            if bytes.len() as u64 != node.size {
                return Err(ProviderError::VersionChanged);
            }
            if offset >= node.size || length == 0 {
                return Ok(Vec::new());
            }
            let count = (node.size - offset).min(u64::from(length));
            return Ok(bytes[offset as usize..(offset + count) as usize].to_vec());
        }
        valid_id(&node.id)?;
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let _permit = self.budget.acquire(Priority::Content, cancel).await?;
                let before = self.file(&node.id).await?;
                let current = before.node(&scope.collection)?;
                if current.kind != NodeKind::File || current.content_revision() != node.content_revision() || current.size != node.size {
                    return Err(ProviderError::VersionChanged);
                }
                if offset >= node.size || length == 0 { return Ok(Vec::new()); }
                let count = (node.size - offset).min(u64::from(length));
                if let Some(content) = before.local_content() { return Ok(content[offset as usize..(offset + count) as usize].to_vec()); }
                if !before.capabilities.as_ref().is_some_and(|c| c.can_download == Some(true)) { return Err(ProviderError::Permission); }
                let end = offset + count - 1;
                let mut url = self.url(&["files", &node.id])?;
                url.query_pairs_mut().append_pair("alt", "media");
                let response = self.response(url, Some((offset,end))).await?;
                if response.headers().get("content-encoding").is_some_and(|v| v != "identity") {
                    return Err(ProviderError::Protocol("encoded Google range"));
                }
                if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                    let expected = format!("bytes {offset}-{end}/{}", node.size);
                    if response.headers().get("content-range").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
                        return Err(ProviderError::Protocol("incorrect Google content range"));
                    }
                } else if offset != 0 || count != node.size { return Err(ProviderError::Protocol("Google ignored content range")); }
                let bytes = super::transport::body(response, count as usize).await?;
                if bytes.len() != count as usize { return Err(ProviderError::Protocol("short Google content range")); }
                let after = self.file(&node.id).await?.node(&scope.collection)?;
                if after.kind != NodeKind::File || after.content_revision() != node.content_revision() || after.size != node.size {
                    return Err(ProviderError::VersionChanged);
                }
                Ok(bytes)
            } => result,
        }
    }
}
