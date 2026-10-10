#![allow(clippy::unwrap_used)]
use cirrove_auth::{AccessMode, AppRegistration};
use cirrove_desktop::{demo, model::Overview};

#[test]
fn irreversible_deletion_is_separate_from_ordinary_provider_writes() {
    for provider in ["onedrive", "googledrive", "icloud"] {
        let mut sample = demo::snapshot().unwrap();
        let account = &mut sample.settings.as_mut().unwrap().accounts[0];
        account.access = AccessMode::ReadWrite;
        account.enabled = true;
        match provider {
            "icloud" => account.registration = AppRegistration::ICloud,
            "googledrive" => {
                account.registration = AppRegistration::Google {
                    client_id: "synthetic.apps.googleusercontent.com".into(),
                }
            }
            _ => (),
        }
        let status = &mut sample.status.as_mut().unwrap().accounts[0];
        status.provider = provider.into();
        status.enabled = true;
        status.mounted = true;
        let mut card = Overview::from_snapshot(sample).accounts.remove(0);
        assert_eq!(card.supports_permanent_delete, provider != "icloud");
        // Future ordinary iCloud write support must not imply permanent deletion.
        card.supports_writes = true;
        assert_eq!(card.can_delete_permanently(), provider != "icloud");
        if provider != "icloud" {
            for stale in [
                "grant",
                "mount",
                "enabled",
                "service",
                "provider",
                "capability",
            ] {
                let mut changed = card.clone();
                match stale {
                    "grant" => changed.writable = false,
                    "mount" => changed.mounted = false,
                    "enabled" => changed.enabled = false,
                    "service" => changed.controls_available = false,
                    "provider" => changed.provider_id = "icloud",
                    _ => changed.supports_permanent_delete = false,
                }
                assert!(!changed.can_delete_permanently(), "{stale}");
            }
        }
    }
}

#[test]
fn foreign_daemon_provider_identity_cannot_authorize_deletion() {
    for configured in ["onedrive", "icloud"] {
        let mut sample = demo::snapshot().unwrap();
        let account = &mut sample.settings.as_mut().unwrap().accounts[0];
        account.access = AccessMode::ReadWrite;
        if configured == "icloud" {
            account.registration = AppRegistration::ICloud;
        }
        let status = &mut sample.status.as_mut().unwrap().accounts[0];
        status.provider = if configured == "icloud" {
            "onedrive"
        } else {
            "icloud"
        }
        .into();
        status.mounted = true;
        let card = Overview::from_snapshot(sample).accounts.remove(0);
        assert!(!card.mounted);
        assert!(!card.can_delete_permanently());
    }
}
