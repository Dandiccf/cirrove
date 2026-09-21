//! Bounded GET-only preflight for native Google Docs and Sheets exports.
#![allow(clippy::unwrap_used)]

use cirrove_service::accounts::{Settings, google_provider};

#[tokio::test]
#[ignore = "requires an explicitly selected live Google account; GET-only"]
async fn report_native_doc_and_sheet_export_shape_without_item_identity() {
    let state = std::path::PathBuf::from(std::env::var_os("CIRROVE_GOOGLE_LIVE_STATE").unwrap());
    let label = std::env::var("CIRROVE_GOOGLE_LIVE_LABEL").unwrap();
    let account = Settings::load(&state)
        .unwrap()
        .accounts
        .into_iter()
        .find(|account| account.label == label)
        .unwrap();
    let provider = google_provider(&account).unwrap();
    let report = provider
        .native_export_preflight(&cirrove_core::CancellationToken::new())
        .await
        .unwrap();
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
