//! Synthetic owned-lifetime cookie persistence. Never contacts Apple or keyring.
#![allow(clippy::unwrap_used)]
use super::*;
use crate::sealed_session::OwnedSealedSession;
use crate::{CookieStore, HeaderValue, RecordingCookies, account_hash};
use base64::Engine as _;
use secrecy::ExposeSecret;
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
const APPLE: &str = "synthetic@example.invalid";
const HOST: &str = "p01-drivews.icloud.com";
const URL: &str = "https://p01-drivews.icloud.com/";

#[derive(Default)]
struct Keys {
    value: StdMutex<Option<SecretString>>,
    block: AtomicBool,
    entered: Notify,
    release: Notify,
}
#[async_trait]
impl CredentialVault for Keys {
    async fn load(&self, _: &str) -> anyhow::Result<Option<SecretString>> {
        if self.block.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(self.value.lock().unwrap().clone())
    }
    async fn save(&self, _: &str, value: SecretString) -> anyhow::Result<()> {
        *self.value.lock().unwrap() = Some(value);
        Ok(())
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        *self.value.lock().unwrap() = None;
        Ok(())
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    keys: Arc<Keys>,
    scope: Scope,
    credential: String,
    owner: Arc<std::fs::File>,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let keys = Arc::new(Keys::default());
        let scope = Scope {
            account: uuid::Uuid::new_v4().to_string(),
            provider: PROVIDER_ID.into(),
            collection: COLLECTION.into(),
        };
        let credential = uuid::Uuid::new_v4().to_string();
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some(account_hash(APPLE).unwrap());
        session.headers.session_token = "synthetic".into();
        session.drive_endpoint = Some(URL.parse().unwrap());
        session.docs_endpoint = Some("https://p01-docws.icloud.com/".parse().unwrap());
        let vault = SealedSessionVault::new(temp.path(), &scope.account).unwrap();
        vault
            .save_with(
                &credential,
                session.session_snapshot().unwrap(),
                keys.as_ref(),
            )
            .await
            .unwrap();
        let owner = Arc::new(
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(temp.path().join("owner"))
                .unwrap(),
        );
        owner.try_lock().unwrap();
        Self {
            temp,
            keys,
            scope,
            credential,
            owner,
        }
    }
    fn sealed_path(&self) -> std::path::PathBuf {
        self.temp
            .path()
            .join("accounts")
            .join(&self.scope.account)
            .join("icloud-session.sealed")
    }
    fn provider(&self, owned: bool) -> ICloudDrive {
        let vault = Arc::new(OwnedSealedSession::new(
            SealedSessionVault::new(self.temp.path(), &self.scope.account).unwrap(),
            self.credential.clone(),
            self.owner.clone(),
            self.keys.clone(),
        ));
        let mut drive = ICloudDrive::on_demand_from_vault(
            self.scope.clone(),
            APPLE.into(),
            self.credential.clone(),
            vault.clone(),
        )
        .unwrap();
        if owned {
            drive.writeback = Some(vault);
        }
        drive
    }
    async fn saved(&self) -> SecretString {
        SealedSessionVault::new(self.temp.path(), &self.scope.account)
            .unwrap()
            .load_with(&self.credential, self.keys.as_ref())
            .await
            .unwrap()
            .unwrap()
    }
}
async fn configure(drive: &ICloudDrive, address: std::net::SocketAddr) {
    let mut state = drive.session.lock().await;
    let session = ICloudDrive::active_session(&mut state).await.unwrap();
    session.http = reqwest::Client::builder()
        .no_proxy()
        .danger_accept_invalid_certs(true)
        .resolve(HOST, address)
        .cookie_provider(session.cookies.clone())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
}
async fn response(
    extra: &'static str,
    expect_cookie: Option<&'static str>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let decode = base64::engine::general_purpose::STANDARD;
    let cert = decode
        .decode(include_str!("../package_create/fixtures/server-cert.b64").trim())
        .unwrap();
    let key = decode
        .decode(include_str!("../package_create/fixtures/server-key.b64").trim())
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .unwrap(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(4), async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(socket).await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buf = [0;4096]; let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0 && request.len()+n <= 16384); request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|v| v==b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&request[..end]).unwrap().to_ascii_lowercase();
                    let length: usize = headers.lines().find_map(|v| v.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                    if request.len() >= end+4+length {
                        assert!(headers.starts_with("post /retrieveitemdetailsinfolders "));
                        if let Some(cookie) = expect_cookie { assert!(headers.contains(cookie), "restarted provider must send persisted synthetic cookie"); }
                        break;
                    }
                }
            }
            let body = serde_json::json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":0,"items":[]}]).to_string();
            stream.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",body.len()).as_bytes()).await.unwrap();
            let _ = stream.shutdown().await;
        }).await.expect("bounded synthetic TLS fixture");
    });
    (address, task)
}
async fn add_cookie(drive: &ICloudDrive, value: &str) {
    add_cookie_at(drive, value, URL).await;
}
async fn add_cookie_at(drive: &ICloudDrive, value: &str, source: &str) {
    let mut state = drive.session.lock().await;
    let session = ICloudDrive::active_session(&mut state).await.unwrap();
    let header = HeaderValue::from_str(value).unwrap();
    session
        .cookies
        .set_cookies(&mut [&header].into_iter(), &source.parse().unwrap());
}
#[tokio::test]
async fn owned_poll_cookie_survives_restart_with_original_expiry() {
    let f = Fixture::new().await;
    let drive = f.provider(true);
    let (address, server) = response(
        "Set-Cookie: synthetic=v2; Max-Age=60; Secure; Path=/\r\n",
        None,
    )
    .await;
    configure(&drive, address).await;
    drive
        .changes(&f.scope, None, &CancellationToken::new())
        .await
        .unwrap();
    server.await.unwrap();
    let saved = f.saved().await;
    let value: serde_json::Value = serde_json::from_str(saved.expose_secret()).unwrap();
    assert_eq!(
        value["cookies"].as_array().unwrap().len(),
        1,
        "successful owned poll must persist response cookie before restart"
    );
    let lifetime = &value["cookies"][0]["lifetime"];
    assert_eq!(
        lifetime["expires_at_unix"].as_i64().unwrap(),
        lifetime["received_at_unix"].as_i64().unwrap() + 60
    );
    drop(drive);
    let restarted = f.provider(true);
    let (address, server) = response("", Some("synthetic=v2")).await;
    configure(&restarted, address).await;
    restarted
        .changes(&f.scope, None, &CancellationToken::new())
        .await
        .unwrap();
    server.await.unwrap();
    assert!(
        f.saved().await.expose_secret() == saved.expose_secret(),
        "unchanged poll must preserve lifetime and sealed snapshot"
    );
    let expired = RecordingCookies::default();
    expired
        .restore_at(
            serde_json::from_value(value["cookies"].clone()).unwrap(),
            lifetime["received_at_unix"].as_i64().unwrap() + 60,
        )
        .unwrap();
    assert!(expired.cookies(&URL.parse().unwrap()).is_none());
}
#[tokio::test]
async fn owned_poll_without_response_cookie_and_public_constructor_do_not_write() {
    let f = Fixture::new().await;
    for owned in [true, false] {
        let before = std::fs::read(f.sealed_path()).unwrap();
        let drive = f.provider(owned);
        let (address, server) = response(
            if owned {
                ""
            } else {
                "Set-Cookie: synthetic=memory; Secure; Path=/\r\n"
            },
            None,
        )
        .await;
        configure(&drive, address).await;
        drive
            .changes(&f.scope, None, &CancellationToken::new())
            .await
            .unwrap();
        server.await.unwrap();
        assert!(std::fs::read(f.sealed_path()).unwrap() == before);
    }
}
#[tokio::test]
async fn owned_poll_tombstone_survives_and_preserves_other_path() {
    let f = Fixture::new().await;
    let drive = f.provider(true);
    add_cookie(&drive, "synthetic=old; Secure; Path=/").await;
    add_cookie(&drive, "other=retained; Secure; Path=/other").await;
    let (address, server) = response(
        "Set-Cookie: synthetic=deleted; Max-Age=0; Secure; Path=/\r\n",
        None,
    )
    .await;
    configure(&drive, address).await;
    drive
        .changes(&f.scope, None, &CancellationToken::new())
        .await
        .unwrap();
    server.await.unwrap();
    let restored = ICloudReadSession::from_session_snapshot(&f.saved().await, APPLE).unwrap();
    assert!(restored.cookies.cookies(&URL.parse().unwrap()).is_none());
    assert!(
        restored
            .cookies
            .cookies(&format!("{URL}other").parse().unwrap())
            .is_some()
    );
}
#[tokio::test]
async fn owned_poll_postpublication_failure_reconciles_only_own_attempt() {
    let f = Fixture::new().await;
    let drive = f.provider(true);
    add_cookie(&drive, "synthetic=attempt; Secure; Path=/").await;
    let writer = drive.writeback.as_ref().unwrap();
    writer.fail_next_publication_sync();
    assert!(matches!(
        drive.persist_poll_cookies(&CancellationToken::new()).await,
        Err(ProviderError::Unavailable)
    ));
    // Publication happened, but revision must remain pending until its own
    // exact readback and directory sync succeeds on the next local flush.
    drive
        .persist_poll_cookies(&CancellationToken::new())
        .await
        .unwrap();
    let value = f.saved().await;
    assert!(value.expose_secret().contains("synthetic=attempt"));
    let stable = std::fs::read(f.sealed_path()).unwrap();
    drive
        .persist_poll_cookies(&CancellationToken::new())
        .await
        .unwrap();
    assert!(std::fs::read(f.sealed_path()).unwrap() == stable);
}
#[tokio::test]
async fn owned_poll_binding_refuses_foreign_snapshot_key_or_missing_directory() {
    for kind in 0..3 {
        let f = Fixture::new().await;
        let drive = f.provider(true);
        add_cookie(&drive, "synthetic=pending; Secure; Path=/").await;
        if kind == 0 {
            let vault = SealedSessionVault::new(f.temp.path(), &f.scope.account).unwrap();
            vault
                .save_with(
                    &f.credential,
                    SecretString::from("foreign synthetic snapshot"),
                    f.keys.as_ref(),
                )
                .await
                .unwrap();
        } else if kind == 1 {
            *f.keys.value.lock().unwrap() = Some(SecretString::from("foreign key"));
        } else {
            std::fs::rename(
                f.sealed_path().parent().unwrap(),
                f.temp.path().join("moved-account"),
            )
            .unwrap();
        }
        let before = std::fs::read(f.sealed_path()).ok();
        assert!(matches!(
            drive.persist_poll_cookies(&CancellationToken::new()).await,
            Err(ProviderError::Unavailable)
        ));
        assert!(std::fs::read(f.sealed_path()).ok() == before);
    }
}
#[tokio::test]
async fn owned_poll_blocked_commit_acknowledges_only_captured_revision_and_cancel_keeps_pending() {
    let f = Fixture::new().await;
    let drive = Arc::new(f.provider(true));
    add_cookie(&drive, "synthetic=one; Secure; Path=/").await;
    f.keys.block.store(true, Ordering::SeqCst);
    let task = {
        let drive = drive.clone();
        tokio::spawn(async move { drive.persist_poll_cookies(&CancellationToken::new()).await })
    };
    tokio::time::timeout(Duration::from_secs(2), f.keys.entered.notified())
        .await
        .unwrap();
    add_cookie(&drive, "synthetic=two; Secure; Path=/").await;
    f.keys.release.notify_one();
    task.await.unwrap().unwrap();
    let first = f.saved().await;
    assert!(!first.expose_secret().contains("synthetic=two"));
    let cancelled = CancellationToken::new();
    f.keys.block.store(true, Ordering::SeqCst);
    let pending = {
        let drive = drive.clone();
        let cancelled = cancelled.clone();
        tokio::spawn(async move { drive.persist_poll_cookies(&cancelled).await })
    };
    tokio::time::timeout(Duration::from_secs(2), f.keys.entered.notified())
        .await
        .unwrap();
    cancelled.cancel();
    assert!(matches!(
        pending.await.unwrap(),
        Err(ProviderError::Cancelled)
    ));
    assert!(f.saved().await.expose_secret() == first.expose_secret());
    drive
        .persist_poll_cookies(&CancellationToken::new())
        .await
        .unwrap();
    assert!(f.saved().await.expose_secret().contains("synthetic=two"));
}
#[tokio::test]
async fn owned_poll_legacy_snapshot_never_migrates_or_creates_a_sealed_file() {
    let f = Fixture::new().await;
    let snapshot = f.saved().await;
    *f.keys.value.lock().unwrap() = Some(snapshot);
    let before = std::fs::read(f.sealed_path()).unwrap();
    let drive = f.provider(true);
    add_cookie(&drive, "synthetic=legacy-memory; Secure; Path=/").await;
    drive
        .persist_poll_cookies(&CancellationToken::new())
        .await
        .unwrap();
    assert!(std::fs::read(f.sealed_path()).unwrap() == before);
    assert!(
        !f.keys
            .value
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .expose_secret()
            .starts_with("icloud-seal-v1:")
    );
}

#[tokio::test]
async fn owned_poll_cookie_record_limit_refuses_without_durable_changes() {
    let f = Fixture::new().await;
    let drive = f.provider(true);
    let before = std::fs::read(f.sealed_path()).unwrap();
    for index in 0..=crate::MAX_COOKIE_RECORDS {
        add_cookie(&drive, &format!("distinct{index}=bounded; Secure; Path=/")).await;
    }
    assert!(matches!(
        drive.persist_poll_cookies(&CancellationToken::new()).await,
        Err(ProviderError::Unavailable)
    ));
    assert!(std::fs::read(f.sealed_path()).unwrap() == before);
}

#[tokio::test]
async fn owned_poll_repeated_cookie_rotation_remains_bounded_and_survives_restart() {
    let f = Fixture::new().await;
    let drive = f.provider(true);
    for index in 0..=crate::MAX_COOKIE_RECORDS {
        add_cookie_at(
            &drive,
            &format!("synthetic=rotation{index}; Secure; Path=/"),
            "https://p01-drivews.icloud.com/retrieveItemDetailsInFolders",
        )
        .await;
    }
    let (address, server) = response(
        "Set-Cookie: synthetic=latest; Max-Age=60; Secure; Path=/\r\n",
        None,
    )
    .await;
    configure(&drive, address).await;
    let result = drive
        .changes(&f.scope, None, &CancellationToken::new())
        .await;
    server.await.unwrap();
    assert!(
        result.is_ok(),
        "repeated same-scope cookie rotations must not stop normal owned polls"
    );
    let saved = f.saved().await;
    let value: serde_json::Value = serde_json::from_str(saved.expose_secret()).unwrap();
    assert_eq!(value["cookies"].as_array().unwrap().len(), 1);
    drop(drive);
    let restarted = f.provider(true);
    let (address, server) = response(
        "Set-Cookie: synthetic=deleted; Max-Age=0; Secure; Path=/\r\n",
        Some("synthetic=latest"),
    )
    .await;
    configure(&restarted, address).await;
    restarted
        .changes(&f.scope, None, &CancellationToken::new())
        .await
        .unwrap();
    server.await.unwrap();
    let restored = ICloudReadSession::from_session_snapshot(&f.saved().await, APPLE).unwrap();
    assert!(restored.cookies.cookies(&URL.parse().unwrap()).is_none());
    let tombstone: serde_json::Value =
        serde_json::from_str(f.saved().await.expose_secret()).unwrap();
    assert_eq!(tombstone["cookies"].as_array().unwrap().len(), 1);
}

#[test]
fn owned_poll_coalescing_keeps_exact_source_domain_path_and_security_classes() {
    let store = RecordingCookies::default();
    let cases = [
        (URL, "same=host; Secure; Path=/"),
        (
            "https://p02-drivews.icloud.com/",
            "same=otherhost; Secure; Path=/",
        ),
        (URL, "same=domain; Domain=icloud.com; Secure; Path=/"),
        (URL, "same=path; Secure; Path=/other"),
        (URL, "same=http; Secure; HttpOnly; Path=/"),
        (URL, "same=site; Secure; SameSite=Strict; Path=/"),
        (
            "https://p01-drivews.icloud.com/elsewhere",
            "same=source; Secure; Path=/",
        ),
        (
            URL,
            "same=ignored; Domain=unrelated.invalid; Secure; Path=/",
        ),
    ];
    for (source, cookie) in cases {
        let header = HeaderValue::from_static(cookie);
        store.set_cookies(&mut [&header].into_iter(), &source.parse().unwrap());
    }
    let before = store.records().unwrap();
    let header = HeaderValue::from_static("same=newhost; Secure; Path=/");
    store.set_cookies(&mut [&header].into_iter(), &URL.parse().unwrap());
    let (records, revision) = store.records_with_revision().unwrap();
    assert_eq!(records.len(), cases.len());
    assert_eq!(revision, cases.len() as u64 + 1);
    let retained = serde_json::to_value(&records[..records.len() - 1]).unwrap();
    assert!(retained == serde_json::to_value(&before[1..]).unwrap());
    let replay = RecordingCookies::default();
    replay.restore(records).unwrap();
    for url in [
        URL,
        "https://p02-drivews.icloud.com/",
        "https://p01-drivews.icloud.com/other",
    ] {
        let pairs = |store: &RecordingCookies| {
            let mut pairs = store
                .cookies(&url.parse().unwrap())
                .map(|header| {
                    header
                        .to_str()
                        .unwrap()
                        .split("; ")
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            pairs.sort();
            pairs
        };
        assert!(pairs(&store) == pairs(&replay));
    }
}

#[tokio::test]
async fn owned_poll_retains_owner_until_last_provider_reference() {
    let f = Fixture::new().await;
    let drive = Arc::new(f.provider(true));
    let other = drive.clone();
    let weak = Arc::downgrade(&f.owner);
    drop(drive);
    let Fixture { owner, .. } = f;
    drop(owner);
    assert!(weak.upgrade().is_some());
    drop(other);
    assert!(weak.upgrade().is_none());
}
#[test]
fn owned_poll_concurrent_cookie_replay_matches_live_jar_and_revision() {
    let cookies = Arc::new(RecordingCookies::default());
    std::thread::scope(|scope| {
        for value in ["same=one; Secure; Path=/", "same=two; Secure; Path=/"] {
            let cookies = cookies.clone();
            scope.spawn(move || {
                let header = HeaderValue::from_static(value);
                cookies.set_cookies(&mut [&header].into_iter(), &URL.parse().unwrap());
            });
        }
    });
    let (records, revision) = cookies.records_with_revision().unwrap();
    assert_eq!(revision, 2);
    let restored = RecordingCookies::default();
    restored.restore(records).unwrap();
    assert!(cookies.cookies(&URL.parse().unwrap()) == restored.cookies(&URL.parse().unwrap()));
}
