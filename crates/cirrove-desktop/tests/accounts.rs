#![allow(clippy::unwrap_used)]
use cirrove_desktop::{
    demo,
    model::{ConnectionState, Overview, ServiceFailure, SettingsFailure},
};
use cirrove_service::accounts::{Settings, set_enabled_by_id};

#[test]
fn desired_state_is_not_mount_acknowledgement_and_lost_service_is_not_ready() {
    let mut snapshot = demo::snapshot().unwrap();
    snapshot.settings.as_mut().unwrap().accounts[0].enabled = false;
    let overview = Overview::from_snapshot(snapshot);
    assert_eq!(overview.accounts[0].state, ConnectionState::Unmounting);
    assert!(overview.accounts[0].mounted);
    let mut snapshot = demo::snapshot().unwrap();
    snapshot.status = Err(ServiceFailure::Io(std::io::ErrorKind::NotFound));
    let overview = Overview::from_snapshot(snapshot);
    assert_eq!(overview.accounts.len(), 2);
    assert!(overview.accounts.iter().all(|a| !a.mounted
        && !a.controls_available
        && a.state == ConnectionState::ServiceUnavailable));
    let mut snapshot = demo::snapshot().unwrap();
    snapshot.settings.as_mut().unwrap().accounts[1].enabled = true;
    assert_eq!(
        Overview::from_snapshot(snapshot).accounts[1].state,
        ConnectionState::Mounting
    );
}

#[test]
fn old_services_and_reused_names_cannot_confirm_another_account() {
    let mut snapshot = demo::snapshot().unwrap();
    snapshot.status.as_mut().unwrap().protocol_version = 0;
    for account in &mut snapshot.status.as_mut().unwrap().accounts {
        account.account_id.clear();
    }
    let overview = Overview::from_snapshot(snapshot);
    assert_eq!(
        overview.service_error,
        Some(ServiceFailure::Incompatible {
            expected: cirrove_service::STATUS_PROTOCOL_VERSION,
            actual: 0
        })
    );
    assert!(
        overview
            .accounts
            .iter()
            .all(|a| !a.controls_available && a.state == ConnectionState::IncompatibleService)
    );
    let mut snapshot = demo::snapshot().unwrap();
    snapshot.settings.as_mut().unwrap().accounts[0].id =
        "00000000-0000-4000-8000-000000000099".into();
    let overview = Overview::from_snapshot(snapshot);
    assert!(!overview.accounts[0].mounted);
    assert_eq!(
        overview.accounts[0].state,
        ConnectionState::WaitingForService
    );
    let mut snapshot = demo::snapshot().unwrap();
    snapshot.settings.as_mut().unwrap().accounts[0].mount_path = "/different/location".into();
    assert!(!Overview::from_snapshot(snapshot).accounts[0].mounted);
    let mut snapshot = demo::snapshot().unwrap();
    snapshot.settings.as_mut().unwrap().accounts[0].drive.id = "different-library".into();
    assert!(!Overview::from_snapshot(snapshot).accounts[0].mounted);
}

#[tokio::test]
async fn unreadable_invalid_and_recovered_settings_keep_distinct_causes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("accounts.json");
    let socket = temp.path().join("missing.sock");
    std::fs::create_dir(&path).unwrap();
    let read = || cirrove_desktop::model::snapshot(temp.path().into(), socket.clone());
    let view = Overview::from_snapshot(read().await);
    assert_eq!(
        view.settings_error,
        Some(SettingsFailure::Read(std::io::ErrorKind::IsADirectory))
    );
    assert_eq!(
        view.service_error,
        Some(ServiceFailure::Io(std::io::ErrorKind::NotFound))
    );
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(
        &path,
        br#"{"version":"PRIVATE INVALID CONTENT","accounts":[]}"#,
    )
    .unwrap();
    let view = Overview::from_snapshot(read().await);
    assert!(matches!(
        view.settings_error,
        Some(SettingsFailure::InvalidFormat { line: 1, .. })
    ));
    assert!(!format!("{view:?}").contains("PRIVATE"));
    std::fs::write(&path, br#"{"version":999,"accounts":[]}"#).unwrap();
    assert_eq!(
        Overview::from_snapshot(read().await).settings_error,
        Some(SettingsFailure::InvalidConfiguration)
    );
    std::fs::write(
        &path,
        serde_json::to_vec(&demo::snapshot().unwrap().settings.unwrap()).unwrap(),
    )
    .unwrap();
    let view = Overview::from_snapshot(read().await);
    assert!(view.settings_error.is_none() && view.settings_available);
    assert_eq!(view.accounts.len(), 2);
    assert!(
        view.accounts
            .iter()
            .all(|a| !a.controls_available && !a.mounted)
    );
}

#[tokio::test]
async fn malformed_and_stalled_status_responses_are_distinct_and_do_not_echo_bodies() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
    };
    let temp = tempfile::tempdir().unwrap();
    for (name, stalled, expected) in [
        ("malformed", false, ServiceFailure::InvalidResponse),
        ("stalled", true, ServiceFailure::TimedOut),
    ] {
        let socket = temp.path().join(name);
        let server = UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let (mut client, _) = server.accept().await.unwrap();
            let mut command = [0; 7];
            client.read_exact(&mut command).await.unwrap();
            if stalled {
                std::future::pending::<()>().await;
            } else {
                client
                    .write_all(br#"{"protocol_version":"PRIVATE SIGNED URL"}"#)
                    .await
                    .unwrap();
            }
        });
        let view = Overview::from_snapshot(
            cirrove_desktop::model::snapshot(temp.path().into(), socket).await,
        );
        assert_eq!(view.service_error, Some(expected));
        assert!(!format!("{view:?}").contains("PRIVATE"));
        task.abort();
        let _ = task.await;
    }
    // Version mismatch remains visible even when no accounts have been saved.
    let mut sample = demo::snapshot().unwrap();
    sample.settings.as_mut().unwrap().accounts.clear();
    sample.status.as_mut().unwrap().protocol_version = 999;
    let view = Overview::from_snapshot(sample);
    assert!(view.accounts.is_empty());
    assert_eq!(
        view.service_error,
        Some(ServiceFailure::Incompatible {
            expected: cirrove_service::STATUS_PROTOCOL_VERSION,
            actual: 999
        })
    );
}

#[test]
fn library_errors_are_visible_without_rendering_raw_provider_messages() {
    let mut snapshot = demo::snapshot().unwrap();
    let account = &mut snapshot.status.as_mut().unwrap().accounts[0];
    account.feeds[0].state = "throttled".into();
    account.feeds[0].message = Some("PRIVATE SIGNED URL".into());
    let overview = Overview::from_snapshot(snapshot);
    assert_eq!(overview.accounts[0].state, ConnectionState::Limited);
    assert!(!format!("{overview:?}").contains("PRIVATE"));
    let mut snapshot = demo::snapshot().unwrap();
    snapshot.status.as_mut().unwrap().accounts[0].feeds[0].state = "sign_in_required".into();
    assert_eq!(
        Overview::from_snapshot(snapshot).accounts[0].state,
        ConnectionState::SignInRequired
    );
    let mut snapshot = demo::snapshot().unwrap();
    snapshot.settings = Err(SettingsFailure::InvalidConfiguration);
    let overview = Overview::from_snapshot(snapshot);
    assert!(!overview.settings_available);
    assert!(overview.accounts.is_empty());
}

#[test]
fn uuid_mount_action_survives_label_change_and_refuses_removed_identity_or_busy_account() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("state");
    cirrove_service::private_dir(&directory).unwrap();
    let state = directory.as_path();
    let mut settings = demo::snapshot().unwrap().settings.unwrap();
    let original = settings.accounts[0].id.clone();
    let save = |settings: &Settings| {
        std::fs::write(
            state.join("accounts.json"),
            serde_json::to_vec(settings).unwrap(),
        )
        .unwrap()
    };
    settings.accounts[0].label = "renamed".into();
    settings.accounts[1].label = "work".into();
    save(&settings);
    set_enabled_by_id(state, &original, false).unwrap();
    let after = Settings::load(state).unwrap();
    assert!(!after.accounts[0].enabled);
    assert!(!after.accounts[1].enabled);
    let busy = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.join(format!("operation-{original}.lock")))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&busy).unwrap();
    assert!(set_enabled_by_id(state, &original, true).is_err());
    drop(busy);
    let lock = cirrove_service::accounts::config_lock(state).unwrap();
    assert!(set_enabled_by_id(state, &original, true).is_err());
    drop(lock);
    settings.accounts.remove(0);
    save(&settings);
    assert!(set_enabled_by_id(state, &original, true).is_err());
    assert!(!Settings::load(state).unwrap().accounts[0].enabled);
}

#[tokio::test]
async fn snapshot_reads_local_socket_and_settings_without_a_cloud_provider() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
    };
    let temp = tempfile::tempdir().unwrap();
    let sample = demo::snapshot().unwrap();
    std::fs::write(
        temp.path().join("accounts.json"),
        serde_json::to_vec(&sample.settings.unwrap()).unwrap(),
    )
    .unwrap();
    let socket = temp.path().join("status.sock");
    let server = UnixListener::bind(&socket).unwrap();
    let task = tokio::spawn(async move {
        let (mut client, _) = server.accept().await.unwrap();
        let mut request = [0; 7];
        client.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"status\n");
        client
            .write_all(&serde_json::to_vec(&sample.status.unwrap()).unwrap())
            .await
            .unwrap();
    });
    let view = Overview::from_snapshot(
        cirrove_desktop::model::snapshot(temp.path().into(), socket.clone()).await,
    );
    task.await.unwrap();
    assert_eq!(view.accounts[0].state, ConnectionState::Connected);
    let view =
        Overview::from_snapshot(cirrove_desktop::model::snapshot(temp.path().into(), socket).await);
    assert!(!view.service_reachable);
    assert_eq!(view.accounts.len(), 2);
}
