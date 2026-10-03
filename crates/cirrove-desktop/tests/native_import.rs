#![allow(clippy::unwrap_used)]
use cirrove_auth::{AccessMode, AppRegistration};
use cirrove_desktop::{
    demo,
    model::{ConnectionState, Overview, native_import_fields_valid},
};
use std::path::Path;

fn card() -> cirrove_desktop::model::AccountCard {
    let mut sample = demo::snapshot().unwrap();
    let account = &mut sample.settings.as_mut().unwrap().accounts[0];
    account.access = AccessMode::ReadWrite;
    account.enabled = true;
    account.registration = AppRegistration::ICloud;
    let status = &mut sample.status.as_mut().unwrap().accounts[0];
    status.provider = "icloud".into();
    status.mounted = true;
    status.enabled = true;
    status.state = "ready".into();
    Overview::from_snapshot(sample).accounts.remove(0)
}
#[test]
fn native_import_requires_current_service_capability_and_exact_writable_icloud_target() {
    let selected = card();
    assert!(selected.can_import_native_package());
    for stale in [
        "provider",
        "grant",
        "mount",
        "enabled",
        "service",
        "capability",
        "state",
        "id",
        "label",
        "user",
        "collection",
        "root",
        "path",
    ] {
        let mut c = selected.clone();
        match stale {
            "provider" => c.provider_id = "googledrive",
            "grant" => c.writable = false,
            "mount" => c.mounted = false,
            "enabled" => c.enabled = false,
            "service" => c.controls_available = false,
            "capability" => c.supports_native_import = false,
            "state" => c.state = ConnectionState::SignInRequired,
            "id" => c.id = "other".into(),
            "label" => c.label = "other".into(),
            "user" => c.username = "other".into(),
            "collection" => c.collection_id = "other".into(),
            "root" => c.root_id = "other".into(),
            _ => c.mount_path = "/elsewhere".into(),
        }
        assert!(!c.same_native_import_target(&selected), "{stale}");
    }
    for capability in [
        "import-native-package",
        "import-native-package-account-binding",
    ] {
        for version in [None, Some(2)] {
            let mut sample = demo::snapshot().unwrap();
            sample.capabilities.capabilities.remove(capability);
            if let Some(version) = version {
                sample
                    .capabilities
                    .capabilities
                    .insert(capability.into(), version);
            }
            assert!(!Overview::from_snapshot(sample).accounts[0].supports_native_import);
        }
    }
}
#[test]
fn native_import_form_preserves_unusual_names_but_refuses_escape_or_other_formats() {
    let path = Path::new("/var/tmp/My 'quoted' archive.zip");
    assert!(native_import_fields_valid(
        path,
        "Äpfel & Birnen.pages",
        "Reports/2026",
        "A new 'name'.pages"
    ));
    for suffix in ["pages", "numbers", "key"] {
        let name = format!("A.{suffix}");
        assert!(native_import_fields_valid(path, &name, "", &name));
        for other in ["pages", "numbers", "key"] {
            let target = format!("B.{other}");
            assert_eq!(
                native_import_fields_valid(path, &name, "", &target),
                suffix == other
            );
        }
    }
    for parent in [
        "/absolute",
        "..",
        "a/../b",
        "a//b",
        "./a",
        "a/",
        "a\nb",
        "a\\b",
    ] {
        assert!(
            !native_import_fields_valid(path, "A.pages", parent, "A.pages"),
            "{parent}"
        );
    }
    for name in [
        "../A.pages",
        "A.pages/b",
        "A.numbers",
        "A.pages\n",
        "",
        "A:bad.pages",
    ] {
        assert!(!native_import_fields_valid(path, name, "", "A.pages"));
        assert!(!native_import_fields_valid(path, "A.pages", "", name));
    }
    assert!(!native_import_fields_valid(
        Path::new("relative.zip"),
        "A.pages",
        "",
        "A.pages"
    ));
    assert!(!native_import_fields_valid(
        Path::new("/var/tmp/../A.zip"),
        "A.pages",
        "",
        "A.pages"
    ));
}

#[test]
fn native_import_progress_never_claims_offline_pinning() {
    use cirrove_desktop::model::RunningJob;
    use cirrove_service::jobs::{Job, JobKind, JobState};
    for state in [JobState::Running, JobState::Stopped, JobState::Succeeded] {
        let job = Job {
            kind: JobKind::ImportNativePackage,
            state,
            files_total: 1,
            ..Default::default()
        };
        let view = RunningJob::from_job(&job);
        assert!(view.native_import);
        assert!(!view.detail.contains("kept offline"));
        if state == JobState::Succeeded {
            assert_eq!(view.detail, "Document imported and available in Files.");
        }
        if state == JobState::Stopped {
            assert!(view.detail.contains("may still finish uploading"));
        }
    }
}

#[test]
fn saved_import_discovery_watch_capabilities_and_target_binding_are_independent() {
    let selected = card();
    assert!(selected.can_list_native_imports());
    assert!(selected.can_watch_native_import());
    let mut readonly = selected.clone();
    readonly.writable = false;
    assert!(readonly.can_list_native_imports());
    assert!(!readonly.can_watch_native_import());
    let mut unmounted = selected.clone();
    unmounted.mounted = false;
    assert!(unmounted.can_list_native_imports());
    assert!(!unmounted.can_watch_native_import());
    for state in [
        ConnectionState::WaitingForService,
        ConnectionState::ServiceUnavailable,
        ConnectionState::IncompatibleService,
    ] {
        let mut stale = selected.clone();
        stale.state = state;
        assert!(!stale.can_list_native_imports());
    }
    let mut mismatched = demo::snapshot().unwrap();
    mismatched.settings.as_mut().unwrap().accounts[0].registration =
        cirrove_auth::AppRegistration::ICloud;
    let status = &mut mismatched.status.as_mut().unwrap().accounts[0];
    status.provider = "icloud".into();
    status.drive_id = "not-the-selected-collection".into();
    let stale = Overview::from_snapshot(mismatched).accounts.remove(0);
    assert_eq!(stale.state, ConnectionState::WaitingForService);
    assert!(!stale.can_list_native_imports());
    for field in [
        "id",
        "label",
        "collection",
        "root",
        "user",
        "tenant",
        "path",
        "provider",
        "access",
    ] {
        let mut changed = selected.clone();
        match field {
            "id" => changed.id = "other".into(),
            "label" => changed.label = "other".into(),
            "collection" => changed.collection_id = "other".into(),
            "root" => changed.root_id = "other".into(),
            "user" => changed.username = "other".into(),
            "tenant" => changed.tenant = "other".into(),
            "path" => changed.mount_path = "/other".into(),
            "provider" => changed.provider_id = "googledrive",
            _ => changed.writable = false,
        }
        assert!(!changed.same_native_import_connection(&selected), "{field}");
    }
    for capability in ["list-native-imports", "watch-native-import"] {
        for version in [None, Some(2)] {
            let mut sample = demo::snapshot().unwrap();
            sample.capabilities.capabilities.remove(capability);
            if let Some(version) = version {
                sample
                    .capabilities
                    .capabilities
                    .insert(capability.into(), version);
            }
            let card = Overview::from_snapshot(sample).accounts.remove(0);
            if capability == "list-native-imports" {
                assert!(!card.supports_native_import_listing);
            } else {
                assert!(!card.supports_native_import_watch);
            }
        }
    }
}
#[test]
fn native_import_job_projection_preserves_exact_durable_operation_across_progress_states() {
    use cirrove_service::jobs::{Job, JobKind, JobState, NativeImportProgress};
    let progress = NativeImportProgress {
        operation: "00000000-0000-4000-8000-000000000042".parse().unwrap(),
        remote: None,
    };
    for state in [
        JobState::Running,
        JobState::Stopped,
        JobState::Failed,
        JobState::Succeeded,
    ] {
        let job = Job {
            kind: JobKind::ImportNativePackage,
            state,
            native_import: Some(progress.clone()),
            ..Default::default()
        };
        assert_eq!(
            cirrove_desktop::model::RunningJob::from_job(&job).import_progress,
            Some(progress.clone())
        );
    }
    let ordinary = Job {
        kind: JobKind::KeepOffline,
        native_import: Some(progress),
        ..Default::default()
    };
    assert!(
        cirrove_desktop::model::RunningJob::from_job(&ordinary)
            .import_progress
            .is_none()
    );
}
