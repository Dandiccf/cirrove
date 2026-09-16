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
            wastebasket: None,
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
            // Two, matching the two the activity reply below names: the count
            // and the list must agree or the demo shows a window that could
            // not exist.
            stuck_changes: if a.enabled { 2 } else { 0 },
            failed_uploads: 0,
            pin_budget: Default::default(),
            pins: Vec::new(),
            kept_generation: 0,
            // One running job on the connected account, so the demo shows the
            // progress row and its Stop button rather than only the still
            // states around it.
            jobs: if a.enabled {
                vec![cirrove_service::jobs::Job {
                    id: "demo-job".into(),
                    kind: cirrove_service::jobs::JobKind::KeepOffline,
                    name: "Projects/Photos".into(),
                    files_total: 340,
                    files_done: 112,
                    bytes_total: 4_294_967_296,
                    bytes_done: 1_395_864_371,
                    started_at: 0,
                    state: cirrove_service::jobs::JobState::Running,
                    issue: None,
                }]
            } else {
                Vec::new()
            },
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
                "work".to_owned(),
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
                        // A save still on its way, with how much of it has
                        // arrived: the state word alone cannot tell a transfer
                        // that is moving from one that is stuck.
                        cirrove_service::recent::LocalChange {
                            sequence: 4,
                            name: "Presentation.key".into(),
                            item: None,
                            state: "uploading".into(),
                            size: 148_000_000,
                            saved_at: Some(now.saturating_sub(70)),
                            transferred: 61_000_000,
                        },
                        cirrove_service::recent::LocalChange {
                            sequence: 3,
                            name: "Notes.txt".into(),
                            item: None,
                            state: "uploaded".into(),
                            size: 1_024,
                            saved_at: Some(now.saturating_sub(45 * 60)),
                            transferred: 0,
                        },
                        cirrove_service::recent::LocalChange {
                            sequence: 2,
                            name: "Budget.xlsx".into(),
                            item: None,
                            state: "failed".into(),
                            size: 8_192,
                            saved_at: Some(now.saturating_sub(30 * 3600)),
                            transferred: 0,
                        },
                        cirrove_service::recent::LocalChange {
                            sequence: 1,
                            name: "Old draft.md".into(),
                            item: None,
                            state: "uploaded".into(),
                            size: 512,
                            saved_at: None,
                            transferred: 0,
                        },
                    ],
                    // Two refused changes, one of each kind, so the demo shows
                    // the state the window has something to say about: a
                    // conflict the cloud decided about and a failure worth
                    // trying again. One of each is also what makes the Try
                    // again button sensitive and its sentence the interesting
                    // one.
                    stuck: vec![
                        cirrove_service::recent::StuckChange {
                            what: "delete folder".into(),
                            name: "Old invoices".into(),
                            path: Some("Accounts/2024/Old invoices".into()),
                            state: "conflict".into(),
                            instead: None,
                        },
                        cirrove_service::recent::StuckChange {
                            what: "create folder".into(),
                            name: "Q3 drafts".into(),
                            path: Some("Projects/Q3 drafts".into()),
                            state: "failed".into(),
                            instead: None,
                        },
                    ],
                    failed: vec![cirrove_service::recent::StuckChange {
                        what: "save".into(),
                        name: "Quarterly report.odt".into(),
                        path: Some("Projects/Quarterly report.odt".into()),
                        state: "failed".into(),
                        instead: Some("24,312 bytes, changed 2026-09-16".into()),
                    }],
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
