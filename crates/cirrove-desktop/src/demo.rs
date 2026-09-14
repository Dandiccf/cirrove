//! Explicit synthetic presentation data. No credentials, mounts or provider calls.
use crate::model::{Overview, Snapshot};
use cirrove_service::{Status, accounts::Settings, manager::AccountStatus};

pub fn snapshot() -> anyhow::Result<Snapshot> {
    let settings: Settings = serde_json::from_str(include_str!("../fixtures/accounts.json"))?;
    settings.validate()?;
    let accounts = settings
        .accounts
        .iter()
        .map(|a| AccountStatus {
            account_id: a.id.clone(),
            drive_id: a.drive.id.clone(),
            root_id: a.root_id.clone(),
            enabled: a.enabled,
            label: a.label.clone(),
            account: a.identity.username.clone(),
            tenant: a.identity.tenant_id.clone(),
            drive: a.drive.name.clone(),
            mount_path: a.mount_path.clone(),
            mounted: a.enabled,
            state: if a.enabled { "ready" } else { "disabled" }.into(),
            feeds: if a.enabled {
                vec![cirrove_service::engine::FeedHealth {
                    collection: a.drive.id.clone(),
                    state: "ready".into(),
                    last_success: None,
                    retry_at: None,
                    message: None,
                    notifications: Default::default(),
                }]
            } else {
                vec![]
            },
            directory_freshness: Default::default(),
            // The demo snapshot invents no counters. A fabricated read path would
            // look exactly like a measured one in the UI.
            read_path: None,
            save_refusal: None,
            stuck_changes: 0,
            failed_uploads: 0,
            pin_budget: Default::default(),
            pins: Vec::new(),
            indexed_feeds: u64::from(a.enabled),
            indexed_items: 1234,
        })
        .collect();
    Ok(Snapshot {
        // The demo showed an empty activity list, which is the one thing the
        // window does that a demo screenshot could not show. Times are
        // relative to now, so it reads the same whenever it is opened, and the
        // last entry deliberately has none: a save from a journal written
        // before saves carried a time must render without one.
        activity: {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            vec![(
                "work-onedrive".to_owned(),
                cirrove_service::RecentReply {
                    remote: vec![cirrove_service::recent::RemoteChange {
                        at_unix: now.saturating_sub(90),
                        id: "demo-1".into(),
                        parent_id: None,
                        name: "Quarterly report.docx".into(),
                        kind: "file".into(),
                        size: 24_576,
                        removed: false,
                    }],
                    local: vec![
                        cirrove_service::recent::LocalChange {
                            sequence: 3,
                            name: "Notes.txt".into(),
                            item: None,
                            state: "uploaded".into(),
                            size: 1_024,
                            saved_at: Some(now.saturating_sub(45 * 60)),
                        },
                        cirrove_service::recent::LocalChange {
                            sequence: 2,
                            name: "Budget.xlsx".into(),
                            item: None,
                            state: "failed".into(),
                            size: 8_192,
                            saved_at: Some(now.saturating_sub(30 * 3600)),
                        },
                        cirrove_service::recent::LocalChange {
                            sequence: 1,
                            name: "Old draft.md".into(),
                            item: None,
                            state: "uploaded".into(),
                            size: 512,
                            saved_at: None,
                        },
                    ],
                    refusal: None,
                },
            )]
        },
        settings: Ok(settings),
        status: Ok(Status {
            protocol_version: 1,
            version: env!("CARGO_PKG_VERSION").into(),
            milestone: "writable-preview".into(),
            indexed_feeds: 1,
            indexed_items: 1234,
            active_mounts: 1,
            accounts,
            restart_required: false,
            allocator_trims: 0,
            free_arena_bytes: 0,
            retained_bytes: 0,
        }),
    })
}
pub fn overview() -> anyhow::Result<Overview> {
    Ok(Overview::from_snapshot(snapshot()?))
}
