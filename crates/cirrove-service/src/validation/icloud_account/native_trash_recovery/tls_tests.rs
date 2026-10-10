//! An actual worker child loses the genuine TLS Trash receipt before local ACK.
//! All retained files belong to this synthetic fixture; no host keyring is used.
use super::*;
use crate::{
    accounts::Account, engine::Engine, icloud_writes::ICloudWriteProvider,
    journal::RecoveryJournal, manager::WriteContext, mutations::MutationWorker,
    native_import::ImportParent, native_trash::NativeTrashAdmission,
};
use anyhow::{Context, Result};
use base64::Engine as _;
use cirrove_auth::{AccessMode, AppRegistration, CredentialVault, Identity};
use cirrove_core::{
    ChangePage, CollectionInfo, Cursor, DirectoryPage, MetadataProvider, Node, NodeKind,
    ProviderError, ReadProvider,
};
use cirrove_icloud::{ROOT_ID, SealedNativeTrashCheckpointVault};
use secrecy::ExposeSecret;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    net::SocketAddr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};

const ORIGIN: &str = "https://fixture.icloud-content.com";
const ITEM: &str = "FILE::com.apple.CloudDocs::native";
const CHILD_ENV: &str = "CIRROVE_SYNTHETIC_NATIVE_TRASH_CHILD";
const EXACT_TEST: &str = "validation::icloud_account::native_trash_recovery::tls_tests::native_trash_actual_worker_child_exit86_recovers_without_second_trash";
const CERT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../cirrove-icloud/src/package_create/fixtures/server-cert.b64"
));
const KEY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../cirrove-icloud/src/package_create/fixtures/server-key.b64"
));

fn original() -> Node {
    Node {
        id: ITEM.into(),
        parent_id: Some(ROOT_ID.into()),
        name: "Target.numbers".into(),
        kind: NodeKind::Folder,
        package: true,
        size: 17,
        modified_unix: 0,
        etag: Some("original-v1".into()),
        content_version: None,
        target: None,
    }
}
fn parent() -> Node {
    Node {
        id: ROOT_ID.into(),
        parent_id: None,
        name: String::new(),
        kind: NodeKind::Folder,
        package: false,
        size: 0,
        modified_unix: 0,
        etag: Some("root-v1".into()),
        content_version: None,
        target: None,
    }
}
fn archive() -> Vec<u8> {
    crate::native_import::synthetic_package_archive("Target.numbers/Document", b"synthetic content")
}
fn private_dir(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    std::fs::create_dir(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}
fn put(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
/// Only short wrapping keys go here. Values and the genuine encrypted checkpoint
/// survive the child process; there is no mock checkpoint or host-keyring access.
struct FixtureKeys(PathBuf);
#[async_trait::async_trait]
impl CredentialVault for FixtureKeys {
    async fn load(&self, key: &str) -> Result<Option<SecretString>> {
        let path = self.0.join(hex::encode(Sha256::digest(key.as_bytes())));
        match std::fs::read_to_string(path) {
            Ok(value) => Ok(Some(SecretString::from(value))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    async fn save(&self, key: &str, value: SecretString) -> Result<()> {
        anyhow::ensure!(
            value.expose_secret().starts_with("icloud-seal-v1:")
                && value.expose_secret().len() < 256,
            "fixture received checkpoint instead of wrapping key"
        );
        put(
            &self.0.join(hex::encode(Sha256::digest(key.as_bytes()))),
            value.expose_secret().as_bytes(),
        )
    }
    async fn remove(&self, _: &str) -> Result<()> {
        anyhow::bail!("fixture keys remain retained")
    }
}
struct Metadata;
#[async_trait::async_trait]
impl MetadataProvider for Metadata {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        Err(ProviderError::Unavailable)
    }
}
#[async_trait::async_trait]
impl ReadProvider for Metadata {
    async fn node(
        &self,
        _: &Scope,
        item: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        if item == ROOT_ID {
            Ok(parent())
        } else if item == ITEM {
            Ok(original())
        } else {
            Err(ProviderError::NotFound)
        }
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        Err(ProviderError::Unavailable)
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        Err(ProviderError::Unavailable)
    }
}
#[derive(Default)]
struct Seen {
    trashed: bool,
    trash_calls: usize,
    archive_reads: usize,
}
struct Server {
    address: SocketAddr,
    state: Arc<Mutex<Seen>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn client(address: SocketAddr) -> Result<reqwest::Client> {
    let cert = base64::engine::general_purpose::STANDARD.decode(CERT.trim())?;
    Ok(reqwest::Client::builder()
        .no_proxy()
        .resolve("fixture.icloud-content.com", address)
        .tls_certs_only([reqwest::Certificate::from_der(&cert)?])
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()?)
}
impl Server {
    async fn start() -> Result<Self> {
        let decoder = base64::engine::general_purpose::STANDARD;
        let cert = decoder.decode(CERT.trim())?;
        let key = decoder.decode(KEY.trim())?;
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )?;
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let state = Arc::new(Mutex::new(Seen::default()));
        let seen = state.clone();
        let task = tokio::spawn(async move {
            let serve = async {
                loop {
                    let (socket, _) = listener.accept().await?;
                    let mut stream = acceptor.accept(socket).await?;
                    let mut raw = Vec::new();
                    let (path, body) = loop {
                        let mut buf = [0; 4096];
                        let n = stream.read(&mut buf).await?;
                        anyhow::ensure!(
                            n > 0 && raw.len() + n < 128 * 1024,
                            "fixture HTTP request bound"
                        );
                        raw.extend_from_slice(&buf[..n]);
                        let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let head = std::str::from_utf8(&raw[..end])?;
                        let size = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .map(|size| size.parse::<usize>())
                            .transpose()?
                            .unwrap_or(0);
                        if raw.len() >= end + 4 + size {
                            let path = head
                                .lines()
                                .next()
                                .context("fixture HTTP header")?
                                .split_whitespace()
                                .nth(1)
                                .context("fixture HTTP path")?
                                .to_owned();
                            break (path, raw[end + 4..end + 4 + size].to_vec());
                        }
                    };
                    let reply = {
                        let mut s = seen.lock().map_err(|_| anyhow::anyhow!("fixture lock"))?;
                        match path.split('?').next().context("fixture route")? {
                            "/retrieveItemDetails" => json!({"items":[{"drivewsid":ITEM,"docwsid":"native","zone":"com.apple.CloudDocs","type":"FILE","name":"Target","extension":"numbers","parentId":if s.trashed {"TRASH_ROOT"}else{ROOT_ID},"restorePath":if s.trashed {json!(["root"])}else{json!(null)},"etag":if s.trashed {"trash-v2"}else{"original-v1"},"size":17}]}).to_string().into_bytes(),
                            "/ws/com.apple.CloudDocs/download/by_id" => json!({"package_token":{"url":format!("{ORIGIN}/archive")}}).to_string().into_bytes(),
                            "/archive" => { s.archive_reads += 1; archive() },
                            "/moveItemsToTrash" => {
                                let value: serde_json::Value = serde_json::from_slice(&body)?;
                                anyhow::ensure!(value["items"][0]["drivewsid"] == ITEM && value["items"][0]["etag"] == "original-v1", "fixture conditional Trash changed");
                                s.trash_calls += 1;
                                anyhow::ensure!(s.trash_calls == 1, "fixture received repeated Trash");
                                s.trashed = true;
                                json!({"items":[{"status":"OK"}]}).to_string().into_bytes()
                            }
                            _ => anyhow::bail!("unexpected synthetic native Trash route"),
                        }
                    };
                    stream.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.len()).as_bytes()).await?;
                    stream.write_all(&reply).await?;
                    stream.shutdown().await?;
                }
                #[allow(unreachable_code)]
                Ok::<(), anyhow::Error>(())
            };
            if serve.await.is_err() {
                panic!("synthetic native Trash TLS server failed");
            }
        });
        Ok(Self {
            address,
            state,
            task,
        })
    }
}
#[derive(Serialize, Deserialize)]
struct Input {
    root: PathBuf,
    state: PathBuf,
    directory: PathBuf,
    keys: PathBuf,
    account: Account,
    operation: Uuid,
    run: Uuid,
    address: SocketAddr,
}
fn scope(account: &Account) -> Scope {
    Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    }
}
fn request(account: &Account) -> MutationRequest {
    MutationRequest {
        scope: scope(account),
        intent: MutationIntent::TrashNativeDocument { before: original() },
    }
}
fn vault(input: &Input) -> Result<Arc<SealedNativeTrashCheckpointVault>> {
    Ok(Arc::new(
        SealedNativeTrashCheckpointVault::with_test_key_vault(
            &input.state,
            &input.account.id,
            Arc::new(FixtureKeys(input.keys.clone())),
        )?,
    ))
}
fn router(input: &Input, context: &WriteContext) -> Result<Arc<dyn MutationProvider>> {
    Ok(Arc::new(
        ICloudWriteProvider::new(&input.account, context)?
            .synthetic_native_transport(client(input.address)?)
            .synthetic_native_trash_checkpoint(vault(input)?),
    ))
}
async fn child(input: Input) -> Result<()> {
    let engine = Engine::new(
        input.account.clone(),
        Arc::new(Metadata),
        input.state.clone(),
    )
    .await?;
    let context = WriteContext::open(&engine, &input.state).await?;
    let guard = Arc::new(Guard::new(
        router(&input, &context)?,
        request(&input.account),
        context.journal(),
        vec![],
        vec![],
        input.directory.clone(),
        input.run,
        Mode::Lose,
    )?);
    let worker = MutationWorker::new(context.journal(), guard, CancellationToken::new());
    tokio::time::timeout(Duration::from_secs(20), worker.run_once()).await??;
    anyhow::bail!("worker returned before the genuine receipt exit86 cut")
}
fn raw_tuple(journal: &Path, operation: Uuid) -> Result<(i64, String, String, i64, bool)> {
    let db = rusqlite::Connection::open_with_flags(
        journal.join("uploads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    Ok(db.query_row("SELECT m.sequence,m.state,m.body,q.sequence,q.complete FROM mutations m JOIN write_queue q ON q.id=m.id WHERE m.id=?1", [operation.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_trash_actual_worker_child_exit86_recovers_without_second_trash() -> Result<()> {
    if let Some(path) = std::env::var_os(CHILD_ENV) {
        return child(serde_json::from_slice(&std::fs::read(path)?)?).await;
    }
    let root = tempfile::Builder::new()
        .prefix("cirrove-native-trash-worker-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?
        .keep();
    let state = private_dir(&root, "state")?;
    let directory = private_dir(&root, "evidence")?;
    let keys = private_dir(&root, "keys")?;
    let mount = private_dir(&root, "mount")?;
    let source = root.join("original.numbers");
    let source_bytes = archive();
    put(&source, &source_bytes)?;
    let account = Account {
        id: Uuid::new_v4().to_string(),
        label: "synthetic native Trash ACK cut".into(),
        registration: AppRegistration::ICloud,
        identity: Identity {
            tenant_id: "fixture".into(),
            subject: "fixture".into(),
            username: "fixture@example.com".into(),
            graph_user_id: "fixture".into(),
            display_name: "fixture".into(),
        },
        credential_id: Uuid::new_v4().to_string(),
        access: AccessMode::ReadWrite,
        drive: CollectionInfo {
            id: "drive".into(),
            name: "Fixture".into(),
            drive_type: "icloud_drive".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: ROOT_ID.into(),
        mount_path: mount,
        enabled: false,
        poll_seconds: 3600,
        cache_bytes: 8 * 1024 * 1024,
    };
    let engine = Engine::new(account.clone(), Arc::new(Metadata), state.clone()).await?;
    let context = WriteContext::open(&engine, &state).await?;
    let journal = context.journal();
    let mut local = journal
        .lock()
        .map_err(|_| anyhow::anyhow!("fixture journal lock"))?;
    let frontier = local.namespace_publication(0)?.through;
    let operation = local.enqueue_native_trash_selection(
        NativeTrashAdmission {
            parent: ImportParent {
                scope: scope(&account),
                route: vec![parent()],
                parent: parent(),
                children: vec![original()],
                frontier,
            },
            target: original(),
        },
        &CancellationToken::new(),
    )?;
    drop(local);
    drop(journal);
    drop(context);
    drop(engine);
    let server = Server::start().await?;
    let input = Input {
        root: root.clone(),
        state,
        directory,
        keys,
        account,
        operation,
        run: Uuid::new_v4(),
        address: server.address,
    };
    let input_path = root.join("child-input.json");
    put(&input_path, &serde_json::to_vec(&input)?)?;
    let child_input = input_path.clone();
    let (pid, code) = tokio::task::spawn_blocking(move || -> Result<(u32, Option<i32>)> {
        let mut child = Command::new(std::env::current_exe()?)
            .args(["--exact", EXACT_TEST, "--nocapture"])
            .env(CHILD_ENV, child_input)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let pid = child.id();
        let until = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok((pid, status.code()));
            }
            if Instant::now() >= until {
                child.kill()?;
                child.wait()?;
                anyhow::bail!("owned worker child did not reach exit86 within bound");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })
    .await??;
    assert_eq!(
        code,
        Some(86),
        "the worker child must exit at the actual receipt boundary"
    );
    assert!(!Path::new("/proc").join(pid.to_string()).exists());
    assert_eq!(
        server
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("fixture lock"))?
            .trash_calls,
        1
    );
    let marker: LossMarker = serde_json::from_slice(&std::fs::read(
        input.directory.join("lost-native-trash-confirmation.json"),
    )?)?;
    assert_eq!(marker.pid, pid);
    assert_eq!(marker.run, input.run);
    assert_eq!(marker.operation, operation);
    assert!(marker.request == request(&input.account));
    assert_eq!(marker.prepared_item, ITEM);
    assert!(marker.receipt == MutationReceipt::Removed { item: ITEM.into() });
    assert!(!marker.acknowledgement_returned);
    let journal = input
        .state
        .join("accounts")
        .join(&input.account.id)
        .join("journal");
    let read = RecoveryJournal::open(&journal, &input.account.id)?;
    let (before, absent) = read.native_trash_status(operation)?;
    assert!(!absent);
    assert_eq!(before.state, MutationState::Applying);
    assert!(before.attempt.is_some());
    assert_eq!(before.prepared_item.as_deref(), Some(ITEM));
    assert!(before.receipt.is_none() && before.verified_content.is_none());
    assert!(before.base.is_none() && before.working_file.is_none());
    assert!(read.list(0, 2)?.is_empty());
    let tuple = raw_tuple(&journal, operation)?;
    assert_eq!(tuple.0 as u64, before.sequence);
    assert_eq!(tuple.1, "applying");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&tuple.2)?,
        serde_json::to_value(&before)?
    );
    assert_eq!(tuple.3, tuple.0);
    assert!(!tuple.4);
    drop(read);
    let key = format!("icloud-native-trash/{}/{}", input.account.id, operation);
    let saved = vault(&input)?
        .load(&key)
        .await?
        .context("genuine sealed checkpoint")?;
    let checkpoint: serde_json::Value = serde_json::from_str(saved.expose_secret())?;
    assert_eq!(checkpoint["phase"], "may_have_sent");
    assert_eq!(checkpoint["operation"], json!(operation));
    assert_eq!(
        checkpoint["request"],
        serde_json::to_value(request(&input.account))?
    );
    assert_eq!(checkpoint["semantic"]["version"], 2);
    let cipher_path = input
        .state
        .join("accounts")
        .join(&input.account.id)
        .join("native-trash-checkpoints")
        .join(operation.to_string())
        .join("checkpoint.sealed");
    let ciphertext = std::fs::read(&cipher_path)?;
    assert!(ciphertext.starts_with(b"ICLDS1"));
    assert!(
        !ciphertext
            .windows(b"may_have_sent".len())
            .any(|w| w == b"may_have_sent")
    );
    let engine = Engine::new(
        input.account.clone(),
        Arc::new(Metadata),
        input.state.clone(),
    )
    .await?;
    let context = WriteContext::open(&engine, &input.state).await?;
    let mut expected = before;
    expected.state = MutationState::VerifyRequired;
    expected.attempt = None;
    let reopened = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("fixture journal lock"))?
        .native_trash_record(operation)?;
    assert_eq!(
        serde_json::to_value(&reopened)?,
        serde_json::to_value(&expected)?
    );
    let guard = Arc::new(Guard::new(
        router(&input, &context)?,
        request(&input.account),
        context.journal(),
        vec![],
        vec![],
        input.directory.clone(),
        input.run,
        Mode::Recover {
            operation,
            receipt: marker.receipt,
        },
    )?);
    let worker = MutationWorker::new(context.journal(), guard.clone(), CancellationToken::new());
    let result = tokio::time::timeout(Duration::from_secs(20), worker.run_once())
        .await??
        .context("recovery selected original operation")?;
    assert_eq!(result.id, operation);
    assert_eq!(result.state, MutationState::Applied);
    assert!(result.issue.is_none());
    let counts = guard.counts.value();
    assert_eq!(counts["mutations"], 0);
    assert_eq!(counts["preparations"], 0);
    assert_eq!(counts["inspections"], 1);
    assert_eq!(counts["reconciliations"], 1);
    assert_eq!(counts["refused_mutations"], 0);
    assert_eq!(counts["refused_uploads"], 0);
    let after = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("fixture journal lock"))?
        .native_trash_record(operation)?;
    assert_eq!(after.id, operation);
    assert_eq!(after.state, MutationState::Applied);
    assert!(after.request == reopened.request);
    assert!(after.attempt.is_none());
    assert_eq!(after.prepared_item.as_deref(), Some(ITEM));
    assert!(after.receipt == Some(MutationReceipt::Removed { item: ITEM.into() }));
    assert!(raw_tuple(&journal, operation)?.4);
    assert_eq!(
        server
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("fixture lock"))?
            .trash_calls,
        1
    );
    assert!(
        server
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("fixture lock"))?
            .archive_reads
            >= 4
    );
    assert_eq!(std::fs::read(&source)?, source_bytes);
    assert_eq!(std::fs::read(&input_path)?, serde_json::to_vec(&input)?);
    assert_eq!(std::fs::read(&cipher_path)?, ciphertext);
    assert_eq!(
        vault(&input)?
            .load(&key)
            .await?
            .context("retained checkpoint")?
            .expose_secret(),
        saved.expose_secret()
    );
    drop(worker);
    drop(guard);
    drop(context);
    drop(engine);
    let read = RecoveryJournal::open(&journal, &input.account.id)?;
    assert_eq!(
        read.native_trash_status(operation)?.0.state,
        MutationState::Applied
    );
    assert!(read.list(0, 2)?.is_empty());
    Ok(())
}
