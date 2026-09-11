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
            pin_budget: Default::default(),
            pins: Vec::new(),
            indexed_feeds: u64::from(a.enabled),
            indexed_items: 1234,
        })
        .collect();
    Ok(Snapshot {
        settings: Ok(settings),
        status: Ok(Status {
            protocol_version: 1,
            version: env!("CARGO_PKG_VERSION").into(),
            milestone: "readonly-preview".into(),
            indexed_feeds: 1,
            indexed_items: 1234,
            active_mounts: 1,
            accounts,
            allocator_trims: 0,
            free_arena_bytes: 0,
            retained_bytes: 0,
        }),
    })
}
pub fn overview() -> anyhow::Result<Overview> {
    Ok(Overview::from_snapshot(snapshot()?))
}
