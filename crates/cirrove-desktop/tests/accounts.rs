#![allow(clippy::unwrap_used)]
use cirrove_desktop::{
    demo,
    model::{ConnectionState, Overview},
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
    snapshot.status = Err(());
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
    snapshot.settings = Err(());
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
