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

/// An upgrade replaces the daemon's binary and leaves the old process running,
/// so the person keeps using the version they just replaced with nothing
/// saying so. Measured on Fedora 44: after `dnf upgrade` the pid was unchanged
/// and `/proc/<pid>/exe` read `/usr/bin/cirroved (deleted)`. The window is
/// where that has to surface, because the tray is one of the stale processes.
#[test]
fn a_daemon_running_a_replaced_binary_asks_for_a_restart_in_the_window() {
    let snapshot = demo::snapshot().unwrap();
    assert!(
        !Overview::from_snapshot(snapshot).restart_required,
        "nothing should ask for a restart when no upgrade has happened"
    );

    let mut snapshot = demo::snapshot().unwrap();
    snapshot.status.as_mut().unwrap().restart_required = true;
    assert!(
        Overview::from_snapshot(snapshot).restart_required,
        "a daemon whose binary was replaced must reach the window"
    );
}

/// An older daemon has no such field and deserializes it as false, which is
/// the only answer it can give. Taking that as "no upgrade pending" from a
/// daemon we cannot otherwise talk to would be reading meaning into a default.
#[test]
fn an_incompatible_daemon_is_never_read_as_saying_no_restart_is_needed() {
    let mut snapshot = demo::snapshot().unwrap();
    let status = snapshot.status.as_mut().unwrap();
    status.restart_required = true;
    status.protocol_version = cirrove_service::STATUS_PROTOCOL_VERSION + 1;
    let overview = Overview::from_snapshot(snapshot);
    assert!(
        !overview.restart_required,
        "a daemon we cannot read must not drive this banner"
    );
    assert!(
        overview.service_error.is_some(),
        "and it must still be reported as incompatible, which is the louder problem"
    );
}

/// The activity list had names and states but never times, so after a restart
/// -- when the remote ring is empty by design -- it was a column of saves
/// against nothing, with no way to tell this morning's from last week's.
#[test]
fn how_long_ago_is_coarse_and_never_reads_the_future_as_the_past() {
    use cirrove_desktop::model::how_long_ago;
    let now = 1_000_000_000;
    assert_eq!(how_long_ago(now, now).as_deref(), Some("just now"));
    assert_eq!(how_long_ago(now, now - 59).as_deref(), Some("just now"));
    assert_eq!(how_long_ago(now, now - 60).as_deref(), Some("1 minute ago"));
    assert_eq!(
        how_long_ago(now, now - 25 * 60).as_deref(),
        Some("25 minutes ago")
    );
    assert_eq!(how_long_ago(now, now - 3600).as_deref(), Some("1 hour ago"));
    assert_eq!(
        how_long_ago(now, now - 5 * 3600).as_deref(),
        Some("5 hours ago")
    );
    assert_eq!(
        how_long_ago(now, now - 26 * 3600).as_deref(),
        Some("yesterday")
    );
    assert_eq!(
        how_long_ago(now, now - 3 * 86400).as_deref(),
        Some("3 days ago")
    );
    assert_eq!(
        how_long_ago(now, now - 30 * 86400).as_deref(),
        Some("more than a week ago")
    );
    // A clock that went backwards, or a record written a moment into the
    // future, must not come out as a very long time ago.
    assert_eq!(how_long_ago(now, now + 10), None);
}

/// A count says something is wrong; it does not say what. Fourteen folder
/// removals went missing on a live drive and the only trace was the number 14,
/// which nobody could act on. The window now names them.
#[test]
fn refused_changes_are_named_and_not_only_counted() {
    let snapshot = demo::snapshot().unwrap();
    let card = &Overview::from_snapshot(snapshot).accounts[0];
    assert_eq!(
        card.refused_paths,
        vec!["Accounts/2024/Old invoices".to_owned()],
        "the refused change should arrive with the path the daemon resolved"
    );

    // A daemon too old to name them leaves the list empty, and the count must
    // still stand on its own rather than the row going blank.
    let mut snapshot = demo::snapshot().unwrap();
    for (_, reply) in snapshot.activity.iter_mut() {
        reply.stuck.clear();
    }
    let card = &Overview::from_snapshot(snapshot).accounts[0];
    assert!(card.refused_paths.is_empty());
}
