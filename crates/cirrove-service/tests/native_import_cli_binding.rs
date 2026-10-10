//! Actual CLI wire contract; synthetic Unix replies never open an archive/provider.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind};
use cirrove_service::{
    ImportNativePackageRequest, Status,
    jobs::{Job, JobKind, JobState, NativeImportProgress},
    manager::AccountStatus,
};
use serde_json::json;
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
};
const SELECTED: &str = "11111111-1111-4111-8111-111111111111";
const REASSIGNED: &str = "22222222-2222-4222-8222-222222222222";
const LABEL: &str = "selected-native-account";
const REFUSAL: &str = "synthetic import refused before archive capture";
#[derive(Default)]
struct Seen {
    verbs: Vec<String>,
    imports: Vec<ImportNativePackageRequest>,
    serialized_imports: Vec<serde_json::Value>,
}
#[derive(Clone, Copy)]
enum Reply {
    Refusal,
    LegacyRefusal,
    Observer(&'static str),
}
// The pre-layout daemon contract: unknown fields are rejected before a job.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyImportRequest {
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_account_id: Option<String>,
    archive: std::path::PathBuf,
    expected_root: String,
    parent: String,
    name: String,
}
fn job(state: JobState) -> Job {
    Job {
        id: "synthetic-owned-import-job".into(),
        name: "Copy.numbers".into(),
        kind: JobKind::ImportNativePackage,
        state,
        native_import: Some(NativeImportProgress {
            operation: uuid::Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap(),
            remote: (state == JobState::Succeeded).then(|| Node {
                id: "FILE::com.apple.CloudDocs::synthetic-copy".into(),
                parent_id: Some("FOLDER::com.apple.CloudDocs::owned".into()),
                name: "Copy.numbers".into(),
                kind: NodeKind::Folder,
                size: 7,
                modified_unix: 0,
                etag: Some("synthetic-v1".into()),
                content_version: None,
                target: None,
                package: true,
            }),
        }),
        ..Default::default()
    }
}
async fn scenario(
    binding: Option<&str>,
    capability: Option<u32>,
    reply: Reply,
) -> (std::process::Output, Seen) {
    scenario_with_layout(binding, capability, reply, false).await
}
async fn scenario_with_layout(
    binding: Option<&str>,
    capability: Option<u32>,
    reply: Reply,
    flat: bool,
) -> (std::process::Output, Seen) {
    scenario_format(binding, capability, reply, flat, false).await
}
async fn scenario_format(
    binding: Option<&str>,
    capability: Option<u32>,
    reply: Reply,
    flat: bool,
    pages: bool,
) -> (std::process::Output, Seen) {
    let temp = tempfile::Builder::new()
        .prefix("cirrove-import-binding-")
        .tempdir_in("/var/tmp")
        .unwrap()
        .keep();
    std::fs::set_permissions(temp.as_path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = temp.as_path().join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let observed = seen.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let (verb, body) = line
                .trim_end()
                .split_once(' ')
                .unwrap_or((line.trim_end(), ""));
            observed.lock().unwrap().verbs.push(verb.into());
            let response = match verb {
                "capabilities" => {
                    let mut caps = json!({"import-native-package":1});
                    if let Some(version) = capability {
                        caps["import-native-package-account-binding"] = version.into();
                    }
                    json!({"capabilities":caps})
                }
                "import-native-package" => {
                    let mut recorded = observed.lock().unwrap();
                    recorded
                        .serialized_imports
                        .push(serde_json::from_str(body).unwrap());
                    recorded.imports.push(serde_json::from_str(body).unwrap());
                    drop(recorded);
                    match reply {
                        Reply::Refusal => json!({"job":null,"refusal":REFUSAL}),
                        Reply::LegacyRefusal => {
                            let legacy = serde_json::from_str::<LegacyImportRequest>(body);
                            assert!(
                                legacy.is_err(),
                                "old daemon must reject explicit flat request before starting a job"
                            );
                            json!({"job":null,"refusal":"legacy source layout unsupported"})
                        }
                        Reply::Observer(_) => json!({"job":job(JobState::Running),"refusal":null}),
                    }
                }
                "status" => {
                    let Reply::Observer(account) = reply else {
                        panic!("refusal must never start observation")
                    };
                    serde_json::to_value(Status {
                        protocol_version: cirrove_service::STATUS_PROTOCOL_VERSION,
                        version: "synthetic".into(),
                        milestone: "synthetic".into(),
                        indexed_feeds: 0,
                        indexed_items: 0,
                        active_mounts: 1,
                        accounts: vec![AccountStatus {
                            account_id: account.into(),
                            label: LABEL.into(),
                            jobs: vec![job(JobState::Succeeded)],
                            ..Default::default()
                        }],
                        allocator_trims: 0,
                        free_arena_bytes: 0,
                        retained_bytes: 0,
                        restart_required: false,
                    })
                    .unwrap()
                }
                _ => panic!("unexpected synthetic CLI verb"),
            };
            let mut stream = stream.into_inner();
            stream
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    let archive = temp.as_path().join("never-opened-source.numbers");
    assert!(!archive.exists());
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_cirrove"));
    command
        .args(["import-native-package", "--label", LABEL, "--archive"])
        .arg(&archive)
        .args([
            "--parent",
            "Owned",
            "--name",
            if pages { "Copy.pages" } else { "Copy.numbers" },
            "--socket",
        ])
        .arg(&socket)
        .env("RUST_LOG", "off")
        .kill_on_drop(true);
    if flat {
        command.args([
            "--source-layout",
            if pages { "flat-pages" } else { "flat-numbers" },
        ]);
    } else {
        command.args(["--source-root", "Source.numbers"]);
    }
    if let Some(account) = binding {
        command.args(["--account-id", account]);
    }
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .unwrap()
        .unwrap();
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    assert!(
        !archive.exists(),
        "wire fixtures must not generate/capture source bytes"
    );
    let seen = Arc::try_unwrap(seen).ok().unwrap().into_inner().unwrap();
    (output, seen)
}
fn exact_import(seen: &Seen, binding: Option<&str>) {
    assert_eq!(
        seen.imports.len(),
        1,
        "actual CLI did not submit exactly one synthetic import request"
    );
    let request = &seen.imports[0];
    assert_eq!(request.expected_account_id.as_deref(), binding);
    match binding {
        Some(id) => assert_eq!(seen.serialized_imports[0]["expected_account_id"], id),
        None => assert!(
            seen.serialized_imports[0]
                .get("expected_account_id")
                .is_none()
        ),
    }
    assert_eq!(request.label, LABEL);
    assert_eq!(request.expected_root.as_deref(), Some("Source.numbers"));
    assert_eq!(
        request.source_layout,
        cirrove_service::native_import::PackageSourceLayout::Wrapped
    );
    assert!(seen.serialized_imports[0].get("source_layout").is_none());
    assert_eq!(request.parent, "Owned");
    assert_eq!(request.name, "Copy.numbers");
    assert!(request.archive.is_absolute());
    assert_eq!(
        request.archive.file_name().unwrap(),
        "never-opened-source.numbers"
    );
}
#[tokio::test]
async fn cli_native_import_transmits_selected_account_uuid() {
    let (output, seen) = scenario(Some(SELECTED), Some(1), Reply::Refusal).await;
    assert!(!output.status.success());
    exact_import(&seen, Some(SELECTED));
    assert_eq!(seen.verbs, vec!["capabilities", "import-native-package"]);
    assert!(String::from_utf8(output.stderr).unwrap().contains(REFUSAL));
}
#[tokio::test]
async fn cli_native_import_preserves_legacy_label_lookup_without_binding_capability() {
    let (output, seen) = scenario(None, None, Reply::Refusal).await;
    assert!(!output.status.success());
    exact_import(&seen, None);
    assert_eq!(seen.verbs, vec!["capabilities", "import-native-package"]);
    assert!(String::from_utf8(output.stderr).unwrap().contains(REFUSAL));
}
#[tokio::test]
async fn cli_native_import_bound_account_requires_exact_capability_version_before_submission() {
    for version in [None, Some(0), Some(2)] {
        let (output, seen) = scenario(Some(SELECTED), version, Reply::Refusal).await;
        assert!(!output.status.success());
        assert_eq!(
            seen.verbs,
            vec!["capabilities"],
            "bound invocation did not check the daemon contract before refusing"
        );
        assert!(
            seen.imports.is_empty(),
            "unsupported account binding submitted an archive request"
        );
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("account binding")
        );
    }
}
#[tokio::test]
async fn cli_native_import_bound_observer_refuses_same_label_reassigned_account() {
    for account in [SELECTED, REASSIGNED] {
        let (output, seen) = scenario(Some(SELECTED), Some(1), Reply::Observer(account)).await;
        exact_import(&seen, Some(SELECTED));
        assert_eq!(
            seen.verbs,
            vec!["capabilities", "import-native-package", "status"]
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        if account == SELECTED {
            assert!(
                output.status.success(),
                "matching selected-account observation refused"
            );
            assert!(stdout.contains("Verified native document import"));
        } else {
            assert!(
                !output.status.success(),
                "same label admitted a foreign account's synthetic receipt"
            );
            assert!(!stdout.contains("Verified native document import"));
            assert!(
                String::from_utf8(output.stderr)
                    .unwrap()
                    .contains("import result unavailable")
            );
        }
    }
}

#[tokio::test]
async fn cli_native_import_invalid_selected_uuid_refuses_before_any_socket_request() {
    let (output, seen) = scenario(Some("not-an-account-uuid"), Some(1), Reply::Refusal).await;
    assert!(!output.status.success());
    assert!(seen.verbs.is_empty());
    assert!(seen.imports.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("invalid value")
    );
}

#[tokio::test]
async fn cli_explicit_flat_numbers_dispatches_null_root_once_without_opening_archive() {
    let (output, seen) = scenario_with_layout(Some(SELECTED), Some(1), Reply::Refusal, true).await;
    assert!(!output.status.success());
    assert_eq!(seen.verbs, vec!["capabilities", "import-native-package"]);
    assert_eq!(seen.imports.len(), 1);
    let request = &seen.imports[0];
    assert_eq!(request.expected_account_id.as_deref(), Some(SELECTED));
    assert_eq!(
        request.source_layout,
        cirrove_service::native_import::PackageSourceLayout::FlatNumbers
    );
    assert!(request.expected_root.is_none());
    assert_eq!(request.name, "Copy.numbers");
    assert_eq!(seen.serialized_imports[0]["source_layout"], "flat_numbers");
    assert!(seen.serialized_imports[0]["expected_root"].is_null());
    assert!(String::from_utf8(output.stderr).unwrap().contains(REFUSAL));
    for capability in [None, Some(2)] {
        let (output, seen) =
            scenario_with_layout(Some(SELECTED), capability, Reply::Refusal, true).await;
        assert!(!output.status.success());
        assert_eq!(seen.verbs, vec!["capabilities"]);
        assert!(
            seen.imports.is_empty(),
            "flat source choice cannot bypass account-binding capability"
        );
    }
}

#[tokio::test]
async fn cli_explicit_flat_numbers_old_daemon_refusal_does_not_observe_or_retry() {
    let legacy = json!({"label":LABEL,"expected_account_id":SELECTED,"archive":"/var/tmp/never-opened.numbers","expected_root":"Source.numbers","parent":"Owned","name":"Copy.numbers"});
    let decoded: LegacyImportRequest = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        legacy,
        "old wrapped wire remains valid"
    );
    let (output, seen) =
        scenario_with_layout(Some(SELECTED), Some(1), Reply::LegacyRefusal, true).await;
    assert!(!output.status.success());
    assert_eq!(seen.verbs, vec!["capabilities", "import-native-package"]);
    assert_eq!(seen.imports.len(), 1);
    assert_eq!(
        seen.imports[0].expected_account_id.as_deref(),
        Some(SELECTED)
    );
    assert_eq!(seen.serialized_imports[0]["source_layout"], "flat_numbers");
    assert!(seen.serialized_imports[0]["expected_root"].is_null());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("legacy source layout unsupported")
    );
}

#[tokio::test]
async fn cli_explicit_flat_pages_dispatches_null_root_once_and_never_retries_refusal() {
    for reply in [Reply::Refusal, Reply::LegacyRefusal] {
        let (output, seen) = scenario_format(Some(SELECTED), Some(1), reply, true, true).await;
        assert!(!output.status.success());
        assert_eq!(seen.verbs, vec!["capabilities", "import-native-package"]);
        assert_eq!(seen.imports.len(), 1);
        assert_eq!(
            seen.imports[0].expected_account_id.as_deref(),
            Some(SELECTED)
        );
        assert_eq!(
            seen.imports[0].source_layout,
            cirrove_service::native_import::PackageSourceLayout::FlatPages
        );
        assert!(seen.imports[0].expected_root.is_none());
        assert_eq!(seen.imports[0].name, "Copy.pages");
        assert_eq!(seen.serialized_imports[0]["source_layout"], "flat_pages");
        assert!(seen.serialized_imports[0]["expected_root"].is_null());
        let expected = if matches!(reply, Reply::LegacyRefusal) {
            "legacy source layout unsupported"
        } else {
            REFUSAL
        };
        assert!(String::from_utf8(output.stderr).unwrap().contains(expected));
    }
    for capability in [None, Some(2)] {
        let (output, seen) =
            scenario_format(Some(SELECTED), capability, Reply::Refusal, true, true).await;
        assert!(!output.status.success());
        assert_eq!(seen.verbs, vec!["capabilities"]);
        assert!(seen.imports.is_empty());
    }
}
