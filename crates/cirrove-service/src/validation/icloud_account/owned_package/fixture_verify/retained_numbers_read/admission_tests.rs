//! Actual admission entrypoint controls. All synthetic fixture bytes are retained.
use super::*;
use std::collections::BTreeMap;

type Snapshot = BTreeMap<PathBuf, (u32, Option<Vec<u8>>)>;

fn snapshot(root: &Path) -> Snapshot {
    fn visit(root: &Path, path: &Path, result: &mut Snapshot) {
        let metadata = std::fs::symlink_metadata(path).expect("fixture metadata");
        assert!(!metadata.file_type().is_symlink());
        let content = if metadata.is_file() {
            Some(std::fs::read(path).expect("fixture bytes"))
        } else {
            assert!(metadata.is_dir());
            None
        };
        result.insert(
            path.strip_prefix(root).expect("own fixture").to_path_buf(),
            (metadata.permissions().mode() & 0o7777, content),
        );
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).expect("fixture directory") {
                visit(root, &entry.expect("fixture entry").path(), result);
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

fn private_directory(path: &Path) {
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .expect("fresh fixture directory");
}

fn private_bytes(path: &Path, value: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("fresh fixture file");
    file.write_all(value).expect("fixture write");
    file.sync_all().expect("fixture sync");
    file.set_permissions(std::fs::Permissions::from_mode(0o400))
        .expect("fixture mode");
}

fn digest(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn fixture(route_fault: u8, expired: bool) -> (PathBuf, String) {
    let observation = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let account = Uuid::new_v4();
    let credential = Uuid::new_v4();
    let root = PathBuf::from(format!(
        "/var/tmp/cirrove-numbers-browser-editor-{observation}"
    ));
    private_directory(&root);
    println!("retained admission fixture preserved: {}", root.display());
    private_directory(&root.join("mount"));
    private_directory(&root.join("state"));
    private_directory(&root.join("state/accounts"));
    let account_dir = root.join("state/accounts").join(account.to_string());
    private_directory(&account_dir);
    // Deliberately not a decryptable session. The exact refusal must occur before
    // the durable attempt and saved-session loader, never by parsing these bytes.
    let sealed = b"synthetic opaque fixture: must not reach the saved-session loader";
    private_bytes(&account_dir.join("icloud-session.sealed"), sealed);
    let settings = Settings {
        version: 2,
        accounts: vec![Account {
            id: if route_fault == 2 {
                Uuid::new_v4()
            } else {
                account
            }
            .to_string(),
            label: "iCloudRetainedNumbersAdmissionFixture".into(),
            registration: AppRegistration::ICloud,
            identity: cirrove_auth::Identity {
                tenant_id: "icloud".into(),
                subject: "synthetic-admission".into(),
                username: "synthetic@example.invalid".into(),
                graph_user_id: "synthetic-admission".into(),
                display_name: "Synthetic admission".into(),
            },
            credential_id: if route_fault == 3 {
                Uuid::new_v4()
            } else {
                credential
            }
            .to_string(),
            access: if route_fault == 5 {
                AccessMode::ReadWrite
            } else {
                AccessMode::ReadOnly
            },
            drive: cirrove_core::CollectionInfo {
                id: if route_fault == 4 {
                    "foreign-drive"
                } else {
                    "drive"
                }
                .into(),
                name: "iCloud Drive".into(),
                drive_type: "icloud_drive".into(),
                web_url: String::new(),
            },
            root_id: if route_fault == 6 {
                "FOLDER::com.apple.CloudDocs::foreign"
            } else {
                cirrove_icloud::ROOT_ID
            }
            .into(),
            // Alter only routing; the declared account/collection and all byte
            // pins otherwise remain the same complete, isolated local inputs.
            mount_path: root.join(if route_fault == 1 {
                "foreign-mount"
            } else {
                "mount"
            }),
            enabled: true,
            poll_seconds: 30,
            cache_bytes: 1024 * 1024,
        }],
    };
    let settings_raw = serde_json::to_vec_pretty(&settings).expect("settings serialize");
    private_bytes(&root.join("state/accounts.json"), &settings_raw);
    let context = serde_json::json!({
        "version": 1, "observation_run": observation, "subject_run": subject,
        "account": account, "credential_id": credential,
        "source_closure_sha256": "a".repeat(64),
        "sealed_session_size": sealed.len(), "sealed_session_sha256": digest(sealed),
        "copied_ciphertext_only": true, "raw_key_or_plaintext_session_exported": false,
        "cloud_dispatched": false
    });
    let context_raw = serde_json::to_vec_pretty(&context).expect("context serialize");
    private_bytes(&root.join("same-account-context.json"), &context_raw);
    let parent_id = format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4());
    let document_id = Uuid::new_v4().to_string();
    let metadata = std::fs::metadata(&root).expect("root identity");
    let clock = now().expect("clock");
    let registration = serde_json::json!({
        "version": 1, "observation_run": observation, "subject_run": subject,
        "account": account, "credential_id": credential, "session_directory": root,
        "root_dev": metadata.dev(), "root_ino": metadata.ino(),
        "settings_sha256": digest(&settings_raw),
        "same_account_context_sha256": digest(&context_raw),
        "original_parent": {
            "drivewsid": parent_id, "docwsid": "synthetic-parent", "zone": "com.apple.CloudDocs",
            "name": format!("Cirrove-Native-{subject}"), "parentId": cirrove_icloud::ROOT_ID,
            "etag": "parent-a", "type": "FOLDER", "items": []
        },
        "original_document": {
            "drivewsid": format!("FILE::com.apple.CloudDocs::{document_id}"),
            "docwsid": document_id, "zone": "com.apple.CloudDocs",
            "name": format!("Cirrove-Numbers-Editor-{subject}"), "extension": "numbers",
            "parentId": parent_id, "etag": "a-v1", "type": "FILE", "size": 128, "items": []
        },
        "expected_current_etag": null,
        "started_unix_seconds": if expired { clock - 301 } else { clock - 1 },
        "deadline_unix_seconds": if expired { clock - 1 } else { clock + 300 }
    });
    let raw = serde_json::to_vec_pretty(&registration).expect("registration serialize");
    private_bytes(&root.join("retained-read-registration.json"), &raw);
    (root, digest(&raw))
}

async fn refusal_preserves_fixture(route_fault: u8, expired: bool, expected: &str) {
    let (root, registered_digest) = fixture(route_fault, expired);
    let before = snapshot(&root);
    let result = observe(
        &root.join("retained-read-registration.json"),
        &registered_digest,
    )
    .await;
    let error = result.expect_err("admission unexpectedly reached provider observation");
    // Preservation is established before the intended endpoint assertion, so an
    // unrelated fixture/setup failure cannot masquerade as the guard endpoint.
    assert_eq!(
        snapshot(&root),
        before,
        "admission changed fixture bytes or modes"
    );
    for output in [
        "retained-read.attempt.json",
        "source-b.numbers",
        "retained-current-fixture.json",
        "retained-current-read.json",
    ] {
        assert!(
            !root.join(output).try_exists().expect("output presence"),
            "admission crossed the one-shot attempt boundary"
        );
    }
    assert_eq!(error.to_string(), expected);
}

#[tokio::test]
async fn retained_read_foreign_route_refuses_before_attempt_or_session() {
    for route_fault in 1..=6 {
        refusal_preserves_fixture(route_fault, false, "retained account route refused").await;
    }
}

#[tokio::test]
async fn retained_read_expired_window_refuses_before_attempt_or_session() {
    refusal_preserves_fixture(0, true, "retained read deadline refused").await;
}

fn revision_registration() -> Result<(PathBuf, serde_json::Value, Snapshot)> {
    let (root, registered_digest) = fixture(0, false);
    let path = root.join("retained-read-registration.json");
    let raw = std::fs::read(&path)?;
    assert_eq!(digest(&raw), registered_digest);
    let original: Registration = serde_json::from_slice(&raw)?;
    validate(&original)?;
    let before = snapshot(&root);
    Ok((root, serde_json::from_slice(&raw)?, before))
}

fn no_observation_outputs(root: &Path) {
    for output in [
        "retained-read.attempt.json",
        "source-b.numbers",
        "retained-current-fixture.json",
        "retained-current-read.json",
    ] {
        assert!(
            !root.join(output).exists(),
            "schema admission started observation"
        );
    }
}

#[test]
fn retained_read_explicit_revision_mode_current_snapshot_admits_registered_reference() -> Result<()>
{
    let (root, mut wire, before) = revision_registration()?;
    // The genuine registered reference keeps its original revision. Current
    // snapshot opts into observing that same revision or an actual newer one;
    // it does not fabricate an older reference to satisfy changed-only policy.
    wire["revision_mode"] = serde_json::json!("current_snapshot");
    wire["expected_current_etag"] = wire["original_document"]["etag"].clone();
    let admitted = serde_json::from_value::<Registration>(wire);
    assert_eq!(snapshot(&root), before);
    no_observation_outputs(&root);
    assert!(
        admitted.is_ok(),
        "explicit current-snapshot registration refused"
    );
    validate(&admitted?)?;
    Ok(())
}

#[test]
fn retained_read_explicit_revision_mode_changed_only_preserves_registered_reference() -> Result<()>
{
    let (root, mut wire, before) = revision_registration()?;
    wire["revision_mode"] = serde_json::json!("changed_only");
    let admitted = serde_json::from_value::<Registration>(wire);
    assert_eq!(snapshot(&root), before);
    no_observation_outputs(&root);
    assert!(
        admitted.is_ok(),
        "explicit changed-only registration refused"
    );
    validate(&admitted?)?;
    Ok(())
}

#[test]
fn retained_read_legacy_registration_without_revision_mode_stays_admissible() -> Result<()> {
    let (root, wire, before) = revision_registration()?;
    assert!(wire.get("revision_mode").is_none());
    let original: Registration = serde_json::from_value(wire)?;
    validate(&original)?;
    assert_eq!(snapshot(&root), before);
    no_observation_outputs(&root);
    Ok(())
}

#[test]
fn retained_read_unknown_revision_mode_refuses_without_coercion_or_observation() -> Result<()> {
    let (root, wire, before) = revision_registration()?;
    for mode in [
        serde_json::json!("unknown"),
        serde_json::json!("CurrentSnapshot"),
        serde_json::json!("current-snapshot"),
        serde_json::json!(""),
        serde_json::Value::Null,
        serde_json::json!(1),
        serde_json::json!({"current_snapshot": true}),
        serde_json::json!(["current_snapshot"]),
    ] {
        let mut altered = wire.clone();
        altered["revision_mode"] = mode;
        assert!(serde_json::from_value::<Registration>(altered).is_err());
    }
    assert_eq!(snapshot(&root), before);
    no_observation_outputs(&root);
    Ok(())
}

#[test]
fn retained_read_current_document_policy_preserves_legacy_and_exact_expected_revision() -> Result<()>
{
    let (root, mut wire, before) = revision_registration()?;
    let legacy: Registration = serde_json::from_value(wire.clone())?;
    let parent = legacy.original_parent.clone();
    let original = legacy.original_document.clone();
    let mut changed = original.clone();
    changed.etag = "synthetic-current-v2".into();
    assert!(legacy.revision_mode == RevisionMode::ChangedOnly);
    assert!(current_document_binding(&legacy, &parent, &original).is_err());
    current_document_binding(&legacy, &parent, &changed)?;

    wire["revision_mode"] = serde_json::json!("changed_only");
    let explicit_legacy: Registration = serde_json::from_value(wire.clone())?;
    assert!(current_document_binding(&explicit_legacy, &parent, &original).is_err());
    current_document_binding(&explicit_legacy, &parent, &changed)?;

    wire["revision_mode"] = serde_json::json!("current_snapshot");
    let unpinned: Registration = serde_json::from_value(wire.clone())?;
    current_document_binding(&unpinned, &parent, &original)?;
    current_document_binding(&unpinned, &parent, &changed)?;
    for accepted in [&original, &changed] {
        wire["expected_current_etag"] = serde_json::json!(accepted.etag);
        let pinned: Registration = serde_json::from_value(wire.clone())?;
        current_document_binding(&pinned, &parent, accepted)?;
        let other = if accepted == &original {
            &changed
        } else {
            &original
        };
        let refused = current_document_binding(&pinned, &parent, other)
            .err()
            .context("foreign expected revision accepted")?;
        assert_eq!(
            refused.to_string(),
            "retained current identity or revision refused"
        );
        assert!(!format!("{refused:#}").contains("synthetic-current-v2"));
    }
    assert_eq!(snapshot(&root), before);
    no_observation_outputs(&root);
    Ok(())
}

#[test]
fn retained_read_current_snapshot_refuses_foreign_or_malformed_current_metadata() -> Result<()> {
    let (root, mut wire, before) = revision_registration()?;
    wire["revision_mode"] = serde_json::json!("current_snapshot");
    let r: Registration = serde_json::from_value(wire)?;
    let parent = r.original_parent.clone();
    let original = r.original_document.clone();
    current_document_binding(&r, &parent, &original)?;
    let original_bytes = serde_json::to_vec(&original)?;
    for fault in 0..12 {
        let mut foreign = original.clone();
        match fault {
            0 => foreign.drivewsid = format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
            1 => foreign.docwsid = Uuid::new_v4().to_string(),
            2 => foreign.parent_id = format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            3 => foreign.name = "foreign-document".into(),
            4 => foreign.zone = "foreign-zone".into(),
            5 => foreign.kind = "FOLDER".into(),
            6 => foreign.extension = "pages".into(),
            7 => foreign.size = LIMIT + 1,
            8 => foreign.etag.clear(),
            9 => foreign.etag = "*".into(),
            10 => foreign.etag = "synthetic\nprivate-revision".into(),
            _ => foreign.etag = "x".repeat(4097),
        }
        let foreign_bytes = serde_json::to_vec(&foreign)?;
        let refusal = current_document_binding(&r, &parent, &foreign);
        assert_eq!(serde_json::to_vec(&foreign)?, foreign_bytes);
        assert_eq!(serde_json::to_vec(&original)?, original_bytes);
        assert!(
            refusal.is_err(),
            "current snapshot accepted foreign metadata arm {fault}"
        );
        assert_eq!(
            refusal.err().context("missing refusal")?.to_string(),
            "retained current identity or revision refused"
        );
    }
    let mut foreign_parent = parent.clone();
    foreign_parent.drivewsid = format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4());
    let mut moved = original.clone();
    moved.parent_id.clone_from(&foreign_parent.drivewsid);
    assert!(current_document_binding(&r, &foreign_parent, &moved).is_err());
    current_document_binding(&r, &parent, &original)?;
    assert_eq!(snapshot(&root), before);
    no_observation_outputs(&root);
    Ok(())
}

#[tokio::test]
async fn retained_read_current_snapshot_keeps_actual_account_and_deadline_admission() -> Result<()>
{
    for (route_fault, expired) in (1..=6)
        .map(|fault| (fault, false))
        .chain(std::iter::once((0, true)))
    {
        let (root, _) = fixture(route_fault, expired);
        let path = root.join("retained-read-registration.json");
        let mut wire: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        wire["revision_mode"] = serde_json::json!("current_snapshot");
        let raw = serde_json::to_vec_pretty(&wire)?;
        // Change only our retained synthetic setup, before taking the preservation snapshot.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let mut file = OpenOptions::new().write(true).truncate(true).open(&path)?;
        file.write_all(&raw)?;
        file.sync_all()?;
        file.set_permissions(std::fs::Permissions::from_mode(0o400))?;
        drop(file);
        let before = snapshot(&root);
        let refusal = observe(&path, &digest(&raw)).await;
        assert_eq!(snapshot(&root), before);
        no_observation_outputs(&root);
        let error = refusal
            .err()
            .context("current snapshot reached session loading")?;
        assert_eq!(
            error.to_string(),
            if expired {
                "retained read deadline refused"
            } else {
                "retained account route refused"
            }
        );
    }
    Ok(())
}
