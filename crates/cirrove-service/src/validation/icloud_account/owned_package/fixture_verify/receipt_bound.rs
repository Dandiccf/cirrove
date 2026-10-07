//! Read-only exact owned receipt observer. Never login, enqueue or reconcile.
use super::super::public_verify::exact_entry;
use super::*;
use crate::journal::{
    MutationRecord, MutationState, NamespaceObject, PackagePublicationStatus, RecoveryJournal,
    UploadRecord, UploadState,
};
use cirrove_core::{
    Node, NodeKind, Scope,
    mutation::{MutationIntent, MutationReceipt},
    upload::{PackageSemanticIdentity, UploadIntent, UploadRepresentation},
};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: PathBuf,
    size: u64,
    sha256: String,
    root: String,
    semantic: PackageSemanticIdentity,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    run: Uuid,
    account: Uuid,
    label: String,
    session_directory: PathBuf,
    settings_sha256: String,
    parent_creation: Uuid,
    import: Uuid,
    replacement: Option<Uuid>,
    source_a: Source,
    source_b: Source,
}
impl Registration {
    fn scope(&self) -> Scope {
        Scope {
            account: self.account.to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        }
    }
    fn parent_name(&self) -> String {
        format!("Cirrove-Native-{}", self.run)
    }
    fn name(&self) -> String {
        format!("Cirrove-Numbers-Parent-{}.numbers", self.run)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1
                && !self.run.is_nil()
                && !self.account.is_nil()
                && self.label == "iCloudNumbersParentValidation"
                && self.session_directory
                    == format!("/var/tmp/cirrove-numbers-parent-{}", self.run)
                && hex_digest(&self.settings_sha256)
                && !self.parent_creation.is_nil()
                && !self.import.is_nil()
                && self.parent_creation != self.import
                && self.replacement.is_none_or(|id| !id.is_nil()
                    && id != self.import
                    && id != self.parent_creation),
            "owned receipt registration refused"
        );
        for (source, name) in [
            (&self.source_a, "source-a.numbers"),
            (&self.source_b, "source-b.numbers"),
        ] {
            source.semantic.validate()?;
            ensure!(
                source.path == self.session_directory.join(name)
                    && source.root == "Source.numbers"
                    && source.size > 0
                    && source.size <= LIMIT
                    && hex_digest(&source.sha256)
                    && source.semantic.version == 2,
                "owned source registration refused"
            );
        }
        ensure!(
            self.source_a.sha256 != self.source_b.sha256
                && self.source_a.semantic != self.source_b.semantic,
            "owned sources do not differ"
        );
        Ok(())
    }
}
fn registered(bytes: &[u8], digest: &str) -> Result<Registration> {
    ensure!(
        bytes.len() <= 32 * 1024
            && hex_digest(digest)
            && hex::encode(Sha256::digest(bytes)) == digest,
        "owned registration digest changed"
    );
    let plan: Registration = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("owned registration schema refused"))?;
    plan.validate()?;
    Ok(plan)
}
fn read_private(path: &Path, max: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_private(path, false, max)?
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= max,
        "owned private artifact exceeds bound"
    );
    Ok(bytes)
}
fn source_verified(source: &Source) -> Result<()> {
    let mut file = open_private(&source.path, false, LIMIT)?;
    let receipt = PackageDownload {
        size: file.metadata()?.len(),
        sha256: sha256(&mut file)?,
    };
    ensure!(
        receipt.size == source.size && receipt.sha256 == source.sha256,
        "owned source digest changed"
    );
    let proof = cirrove_icloud::package_archive_semantic_identity_versioned(
        &file,
        &receipt,
        &source.root,
        2,
        &CancellationToken::new(),
    )?;
    ensure!(
        proof == source.semantic,
        "owned source semantic content changed"
    );
    Ok(())
}
fn settings(plan: &Registration) -> Result<Account> {
    settings_for(
        plan.account,
        &plan.label,
        &plan.session_directory,
        &plan.settings_sha256,
    )
}
fn settings_for(
    account_id: Uuid,
    label: &str,
    directory: &Path,
    settings_sha256: &str,
) -> Result<Account> {
    let _directory = open_private(directory, true, 0)?;
    let _state = open_private(&directory.join("state"), true, 0)?;
    let bytes = read_private(&directory.join("state/accounts.json"), 256 * 1024)?;
    ensure!(
        hex::encode(Sha256::digest(&bytes)) == settings_sha256,
        "owned settings changed"
    );
    let settings: Settings =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("owned settings refused"))?;
    ensure!(
        settings.version == 2 && settings.accounts.len() == 1,
        "owned account inventory changed"
    );
    let account = settings.accounts[0].clone();
    ensure!(
        account.id == account_id.to_string()
            && account.label == label
            && account.enabled
            && account.access == cirrove_auth::AccessMode::ReadWrite
            && matches!(account.registration, cirrove_auth::AppRegistration::ICloud)
            && account.drive.id == "drive"
            && account.root_id == cirrove_icloud::ROOT_ID
            && account.mount_path == directory.join("mount"),
        "owned account binding refused"
    );
    Ok(account)
}
fn parent_binding(
    plan: &Registration,
    mutations: &[MutationRecord],
    parent: &NamespaceObject,
) -> Result<Node> {
    parent_binding_for(
        &plan.scope(),
        plan.parent_creation,
        &plan.parent_name(),
        mutations,
        parent,
    )
}
fn parent_binding_for(
    scope: &Scope,
    parent_creation: Uuid,
    parent_name: &str,
    mutations: &[MutationRecord],
    parent: &NamespaceObject,
) -> Result<Node> {
    ensure!(
        mutations.len() == 1,
        "owned folder mutation inventory changed"
    );
    let mutation = &mutations[0];
    ensure!(
        mutation.id == parent_creation
            && mutation.request.scope == *scope
            && mutation.state == MutationState::Applied
            && mutation.attempt.is_none()
            && mutation.base.is_none()
            && mutation.working_file.is_none()
            && mutation.request.intent
                == MutationIntent::CreateFolder {
                    parent: cirrove_icloud::ROOT_ID.into(),
                    name: parent_name.to_owned()
                },
        "owned folder receipt refused"
    );
    let Some(MutationReceipt::Upsert(created)) = &mutation.receipt else {
        bail!("owned folder receipt missing");
    };
    let remote = parent
        .remote
        .as_ref()
        .context("owned parent provider binding missing")?;
    let mut local = remote.clone();
    local.id = parent.node.id.clone();
    ensure!(
        parent.scope == *scope
            && parent.follows_remote
            && parent.remote_owned
            && !parent.unlinked
            && parent.latest.is_none()
            && parent.working_file.is_none()
            && parent.native_archive.is_none()
            && parent.node.id == format!("local-directory-{}", parent.id)
            && parent.node == local
            && remote.id == created.id
            && remote.kind == NodeKind::Folder
            && !remote.package
            && remote.target.is_none()
            && remote.parent_id.as_deref() == Some(cirrove_icloud::ROOT_ID)
            && remote.name == parent_name
            && remote.id.starts_with("FOLDER::com.apple.CloudDocs::")
            && !remote.id.ends_with("::")
            && created.kind == remote.kind
            && created.parent_id == remote.parent_id
            && created.name == remote.name
            && !created.package
            && created.target.is_none(),
        "owned parent handoff refused"
    );
    Ok(remote.clone())
}
fn imported<'a>(plan: &Registration, parent: &Node, rows: &'a [UploadRecord]) -> Result<&'a Node> {
    ensure!(
        rows.len() == if plan.replacement.is_some() { 2 } else { 1 },
        "owned upload inventory changed"
    );
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        ensure!(
            seen.insert(row.id)
                && (row.id == plan.import || Some(row.id) == plan.replacement)
                && row.scope == plan.scope()
                && row.state == UploadState::Uploaded
                && row.attempt.is_none()
                && row.base.is_none()
                && row.working_file.is_none(),
            "owned upload receipt refused"
        );
    }
    let row = rows
        .iter()
        .find(|row| row.id == plan.import)
        .context("owned import receipt absent")?;
    ensure!(
        row.intent
            == UploadIntent::Create {
                parent: parent.id.clone(),
                name: plan.name()
            }
            && row.representation
                == UploadRepresentation::PackageArchive {
                    expected_root: plan.source_a.root.clone(),
                    semantic: plan.source_a.semantic.clone()
                }
            && row.size == plan.source_a.size
            && row.sha256 == plan.source_a.sha256
            && row.package_completion.as_ref() == Some(&plan.source_a.semantic)
            && row.identity_handoff.is_none(),
        "owned import source binding changed"
    );
    let node = row
        .remote
        .as_ref()
        .context("owned import provider receipt absent")?;
    ensure!(
        node.id.starts_with("FILE::com.apple.CloudDocs::")
            && !node.id.ends_with("::")
            && node.parent_id.as_ref() == Some(&parent.id)
            && node.name == plan.name()
            && node.kind == NodeKind::Folder
            && node.package
            && node.target.is_none()
            && node.content_version.is_none()
            && node
                .etag
                .as_ref()
                .is_some_and(|v| !v.is_empty() && !v.contains('*')),
        "owned imported identity changed"
    );
    Ok(node)
}
fn replacement<'a>(
    plan: &Registration,
    original: &Node,
    rows: &'a [UploadRecord],
) -> Result<(&'a Node, &'a Node)> {
    replacement_bound(
        plan.replacement
            .context("owned replacement identity missing")?,
        original,
        UploadRepresentation::PackageReplacementArchive {
            expected_root: plan.source_b.root.clone(),
            semantic: plan.source_b.semantic.clone(),
            original: Box::new(original.clone()),
            original_semantic: plan.source_a.semantic.clone(),
        },
        plan.source_b.size,
        &plan.source_b.sha256,
        &plan.source_b.semantic,
        rows,
    )
}
fn replacement_bound<'a>(
    id: Uuid,
    original: &Node,
    expected_representation: UploadRepresentation,
    size: u64,
    sha256: &str,
    semantic: &PackageSemanticIdentity,
    rows: &'a [UploadRecord],
) -> Result<(&'a Node, &'a Node)> {
    let row = rows
        .iter()
        .find(|row| row.id == id)
        .context("owned replacement receipt absent")?;
    ensure!(
        row.intent
            == UploadIntent::Replace {
                item: original.id.clone(),
                expected_etag: original
                    .etag
                    .clone()
                    .context("owned original revision absent")?
            }
            && row.representation == expected_representation
            && row.size == size
            && row.sha256 == sha256
            && row.package_completion.as_ref() == Some(semantic),
        "owned replacement source binding changed"
    );
    let (before, current, backup) = row
        .native_replacement_receipt()
        .context("owned typed handoff receipt missing")?;
    ensure!(before == original, "owned original receipt changed");
    Ok((current, backup))
}
fn publication(status: &PackagePublicationStatus, node: &Node) -> Result<()> {
    ensure!(
        matches!(status,PackagePublicationStatus::Present(current) if current==node),
        "owned publication unconfirmed"
    );
    Ok(())
}
fn journal_binding(
    plan: &Registration,
    journal: &RecoveryJournal,
) -> Result<(Vec<UploadRecord>, Node, Node, Option<Node>)> {
    let rows = journal.list(0, 3)?;
    let mutations = journal.native_validation_mutations(0, 2)?;
    let create = mutations
        .iter()
        .find(|m| m.id == plan.parent_creation)
        .context("owned folder receipt missing")?;
    let Some(MutationReceipt::Upsert(created)) = &create.receipt else {
        bail!("owned folder receipt absent");
    };
    let local = journal
        .native_validation_namespace_by_remote(&plan.scope(), &created.id)?
        .context("owned parent local binding missing")?;
    let parent = parent_binding(plan, &mutations, &local)?;
    ensure!(
        journal.native_validation_incomplete_queue()? == 0,
        "owned write queue unfinished"
    );
    let original = imported(plan, &parent, &rows)?;
    publication(
        &journal.native_validation_package_publication(plan.import)?,
        original,
    )?;
    let (current, backup) = if let Some(id) = plan.replacement {
        let (current, backup) = replacement(plan, original, &rows)?;
        publication(&journal.native_validation_package_publication(id)?, current)?;
        (current.clone(), Some(backup.clone()))
    } else {
        (original.clone(), None)
    };
    Ok((rows, parent, current, backup))
}
fn trash_binding(
    backup: &Node,
    semantic: &PackageSemanticIdentity,
    recovered: &cirrove_icloud::VerifiedPackageTrash,
) -> Result<()> {
    ensure!(
        backup.etag.as_ref() == Some(&recovered.trash_etag) && &recovered.semantic == semantic,
        "owned Trash receipt changed"
    );
    Ok(())
}
async fn observe(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let bytes = read_private(path, 32 * 1024)?;
    let plan = registered(&bytes, digest)?;
    ensure!(
        path == plan.session_directory.join(if plan.replacement.is_some() {
            "postflight-registration.json"
        } else {
            "preflight-registration.json"
        }),
        "owned registration location refused"
    );
    let account = settings(&plan)?;
    source_verified(&plan.source_a)?;
    source_verified(&plan.source_b)?;
    let journal_path = plan
        .session_directory
        .join("state/accounts")
        .join(account.id.as_str())
        .join("journal");
    let _journal_dir = open_private(&journal_path, true, 0)?;
    // The read-only owner lease remains held through both independent reads.
    let journal = RecoveryJournal::open(&journal_path, &account.id)?;
    let (rows, parent, current, backup) = journal_binding(&plan, &journal)?;
    let frozen = serde_json::to_vec(&rows)?;
    let mut remote = session(&plan.session_directory, &account).await?;
    let root = remote.list_folder(cirrove_icloud::ROOT_ID).await?;
    let mut parents = root.iter().filter(|e| e.drivewsid == parent.id);
    let mut parent_entry = parents.next().context("owned parent absent")?.clone();
    ensure!(
        parents.next().is_none()
            && parent_entry.kind == "FOLDER"
            && parent_entry.zone == "com.apple.CloudDocs"
            && parent_entry.name == plan.parent_name(),
        "owned active parent changed"
    );
    parent_entry.items.clear();
    parent_entry.number_of_items = None;
    let document = exact_entry(&remote.list_folder(&parent.id).await?, &current)?;
    let source = if backup.is_some() {
        &plan.source_b
    } else {
        &plan.source_a
    };
    let manifest_path = plan.session_directory.join(if backup.is_some() {
        "postflight-fixture.json"
    } else {
        "preflight-fixture.json"
    });
    let manifest_value = serde_json::json!({"version":1,"run":plan.run,"account":plan.account,"session_directory":plan.session_directory,
        "settings_sha256":plan.settings_sha256,"parent":parent_entry,"document":document,"format":"numbers","representation":"package",
        "source":source.path,"source_size":source.size,"source_sha256":source.sha256,"source_root":source.root,"expected_root":current.name,"semantic":source.semantic});
    record(&manifest_path, &manifest_value)?;
    let manifest_digest = hex::encode(Sha256::digest(read_private(&manifest_path, 32 * 1024)?));
    let current_proof = icloud_owned_fixture_verify(&manifest_path, &manifest_digest).await?;
    let mut trash_proof = None;
    if let Some(backup) = backup.as_ref() {
        let attempt = verification_directory(&plan.session_directory)?;
        manifest(&attempt, plan.run, "owned-receipt-trash-read-only")?;
        let staging = tempfile::tempfile_in(&attempt)?;
        staging.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let recovered = remote
            .verify_owned_package_in_trash(
                cirrove_icloud::OwnedPackageTrashRequest {
                    apple_account: account.identity.username.clone(),
                    drive_id: backup.id.clone(),
                    document_id: backup
                        .id
                        .strip_prefix("FILE::com.apple.CloudDocs::")
                        .context("owned backup identity invalid")?
                        .into(),
                    expected_root: plan.name(),
                    semantic: plan.source_a.semantic.clone(),
                },
                staging,
                &CancellationToken::new(),
            )
            .await?;
        trash_binding(backup, &plan.source_a.semantic, &recovered)?;
        trash_proof = Some(
            serde_json::json!({"item":backup.id,"etag":recovered.trash_etag,"semantic":recovered.semantic,"size":recovered.archive.size,"sha256":recovered.archive.sha256}),
        );
    }
    ensure!(
        exact_entry(&remote.list_folder(&parent.id).await?, &current)? == document,
        "owned current revision changed during proof"
    );
    let end_root = remote.list_folder(cirrove_icloud::ROOT_ID).await?;
    let mut end_parents = end_root.iter().filter(|e| e.drivewsid == parent.id);
    let mut end_parent = end_parents
        .next()
        .context("owned parent disappeared")?
        .clone();
    end_parent.items.clear();
    end_parent.number_of_items = None;
    ensure!(
        end_parents.next().is_none() && end_parent == parent_entry,
        "owned parent changed during proof"
    );
    ensure!(
        read_private(path, 32 * 1024)? == bytes,
        "owned registration changed during proof"
    );
    settings(&plan)?;
    source_verified(&plan.source_a)?;
    source_verified(&plan.source_b)?;
    let (end_rows, end_parent, end_current, end_backup) = journal_binding(&plan, &journal)?;
    ensure!(
        serde_json::to_vec(&end_rows)? == frozen
            && end_parent == parent
            && end_current == current
            && end_backup == backup,
        "owned journal receipt changed during proof"
    );
    let output = serde_json::json!({"run":plan.run,"account":plan.account,"import":plan.import,"replacement":plan.replacement,"registration_sha256":digest,
        "fixture_manifest_sha256":manifest_digest,"current":current_proof,"original_trash":trash_proof,"cloud_mutated":false,"gui_fidelity_verified":false});
    record(
        &plan.session_directory.join(if backup.is_some() {
            "postflight-verified.json"
        } else {
            "preflight-verified.json"
        }),
        &output,
    )?;
    Ok(output)
}
/// Static privacy boundary: dynamic provider/parser causes never reach stdout.
pub async fn icloud_owned_receipt_verify(path: &Path, digest: &str) -> Result<serde_json::Value> {
    tokio::time::timeout(std::time::Duration::from_secs(900), observe(path, digest))
        .await
        .map_err(|_| anyhow::anyhow!("owned receipt observation timed out; no mutation submitted"))?
        .map_err(|_| anyhow::anyhow!("owned receipt observation refused; no mutation submitted"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    pub(super) fn fixture() -> (
        Registration,
        Node,
        MutationRecord,
        NamespaceObject,
        Vec<UploadRecord>,
    ) {
        let run = Uuid::new_v4();
        let account = Uuid::new_v4();
        let create = Uuid::new_v4();
        let import = Uuid::new_v4();
        let base = PathBuf::from(format!("/var/tmp/cirrove-numbers-parent-{run}"));
        let semantic = |digest: &str| PackageSemanticIdentity {
            version: 2,
            sha256: digest.repeat(64),
            entries: 2,
            files: 1,
            expanded_bytes: 3,
        };
        let plan = Registration {
            version: 1,
            run,
            account,
            label: "iCloudNumbersParentValidation".into(),
            session_directory: base.clone(),
            settings_sha256: "a".repeat(64),
            parent_creation: create,
            import,
            replacement: None,
            source_a: Source {
                path: base.join("source-a.numbers"),
                size: 10,
                sha256: "b".repeat(64),
                root: "Source.numbers".into(),
                semantic: semantic("c"),
            },
            source_b: Source {
                path: base.join("source-b.numbers"),
                size: 11,
                sha256: "d".repeat(64),
                root: "Source.numbers".into(),
                semantic: semantic("e"),
            },
        };
        let parent = Node {
            id: "FOLDER::com.apple.CloudDocs::owned-parent".into(),
            parent_id: Some(cirrove_icloud::ROOT_ID.into()),
            name: plan.parent_name(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("parent-r1".into()),
            content_version: None,
            target: None,
            package: false,
        };
        let mutation:MutationRecord=serde_json::from_value(serde_json::json!({"id":create,"sequence":1,"request":{"scope":plan.scope(),"intent":{"kind":"create_folder","parent":cirrove_icloud::ROOT_ID,"name":plan.parent_name()}},"state":"applied","attempt":null,"receipt":{"kind":"upsert","value":parent},"retry_at":0,"failed_attempts":0})).unwrap();
        let local_id = Uuid::new_v4();
        let mut local_node = parent.clone();
        local_node.id = format!("local-directory-{local_id}");
        let local = NamespaceObject {
            native_archive: None,
            id: local_id,
            scope: plan.scope(),
            names: crate::journal::NamespaceNames::Insensitive,
            node: local_node,
            remote: Some(parent.clone()),
            remote_owned: true,
            remote_sequence: 1,
            working_file: None,
            latest: None,
            revision: 2,
            follows_remote: true,
            unlinked: false,
        };
        let original = Node {
            id: "FILE::com.apple.CloudDocs::original".into(),
            parent_id: Some(parent.id.clone()),
            name: plan.name(),
            kind: NodeKind::Folder,
            size: 3,
            modified_unix: 0,
            etag: Some("original-r1".into()),
            content_version: None,
            target: None,
            package: true,
        };
        let imported:UploadRecord=serde_json::from_value(serde_json::json!({"id":import,"sequence":2,"scope":plan.scope(),"intent":{"kind":"create","parent":parent.id,"name":plan.name()},"state":"uploaded","size":plan.source_a.size,"sha256":plan.source_a.sha256,"attempt":null,"remote":original,"representation":{"kind":"package_archive","expected_root":plan.source_a.root,"semantic":plan.source_a.semantic},"package_completion":plan.source_a.semantic})).unwrap();
        (plan, parent, mutation, local, vec![imported])
    }
    fn admitted(
        plan: &Registration,
        parent: &NamespaceObject,
        mutation: &MutationRecord,
        rows: &[UploadRecord],
    ) -> Result<()> {
        plan.validate()?;
        let remote = parent_binding(plan, std::slice::from_ref(mutation), parent)?;
        let original = imported(plan, &remote, rows)?;
        if plan.replacement.is_some() {
            replacement(plan, original, rows)?;
        }
        Ok(())
    }
    #[test]
    fn owned_receipt_registration_and_import_contract_bind_fresh_owned_inventory() {
        let (plan, parent, mutation, local, rows) = fixture();
        admitted(&plan, &local, &mutation, &rows).unwrap();
        let bytes = serde_json::to_vec(&plan).unwrap();
        let digest = hex::encode(Sha256::digest(&bytes));
        registered(&bytes, &digest).unwrap();
        let mut changed = bytes.clone();
        changed.push(b' ');
        assert!(registered(&changed, &digest).is_err());
        publication(
            &PackagePublicationStatus::Present(rows[0].remote.clone().unwrap()),
            rows[0].remote.as_ref().unwrap(),
        )
        .unwrap();
        assert!(
            publication(
                &PackagePublicationStatus::Pending,
                rows[0].remote.as_ref().unwrap()
            )
            .is_err()
        );
        assert_eq!(
            parent_binding(&plan, std::slice::from_ref(&mutation), &local).unwrap(),
            parent
        );
    }
    #[test]
    fn owned_receipt_import_contract_refuses_tampered_scope_source_owner_and_operations() {
        for arm in 0..17 {
            let (mut plan, _, mut mutation, mut local, mut rows) = fixture();
            match arm {
                0 => plan.account = Uuid::new_v4(),
                1 => plan.run = Uuid::new_v4(),
                2 => plan.import = Uuid::new_v4(),
                3 => plan.source_a.sha256 = "f".repeat(64),
                4 => plan.source_a.root = "Wrong.numbers".into(),
                5 => plan.source_a.semantic = plan.source_b.semantic.clone(),
                6 => rows.push(rows[0].clone()),
                7 => rows.clear(),
                8 => rows[0].state = UploadState::Pending,
                9 => rows[0].scope.collection = "foreign".into(),
                10 => rows[0].remote.as_mut().unwrap().etag = None,
                11 => {
                    rows[0].remote.as_mut().unwrap().parent_id =
                        Some(cirrove_icloud::ROOT_ID.into())
                }
                12 => local.follows_remote = false,
                13 => local.latest = Some(Uuid::new_v4()),
                14 => mutation.state = MutationState::Pending,
                15 => local.remote.as_mut().unwrap().name = "foreign".into(),
                _ => plan.parent_creation = plan.import,
            }
            assert!(
                admitted(&plan, &local, &mutation, &rows).is_err(),
                "tampered arm {arm} accepted"
            );
        }
    }
    #[test]
    fn owned_receipt_replacement_contract_binds_both_sources_and_typed_trash_receipt() {
        let (mut plan, parent, mutation, local, mut rows) = fixture();
        let operation = Uuid::new_v4();
        plan.replacement = Some(operation);
        let original = rows[0].remote.clone().unwrap();
        let mut current = original.clone();
        current.id = "FILE::com.apple.CloudDocs::current".into();
        current.etag = Some("current-r1".into());
        let mut backup = original.clone();
        backup.parent_id = Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into());
        backup.etag = Some("trash-r1".into());
        let replacement_row:UploadRecord=serde_json::from_value(serde_json::json!({"id":operation,"sequence":3,"scope":plan.scope(),"intent":{"kind":"replace","item":original.id,"expected_etag":original.etag},"state":"uploaded","size":plan.source_b.size,"sha256":plan.source_b.sha256,"attempt":null,"remote":current,"representation":{"kind":"package_replacement_archive","expected_root":plan.source_b.root,"semantic":plan.source_b.semantic,"original":original,"original_semantic":plan.source_a.semantic},"package_completion":plan.source_b.semantic,"identity_handoff":{"recovery_object":Uuid::new_v4(),"old_item":original.id,"recovery_name":original.name,"trash_parent":"FOLDER::com.apple.CloudDocs::TRASH_ROOT","backup":backup}})).unwrap();
        rows.push(replacement_row);
        admitted(&plan, &local, &mutation, &rows).unwrap();
        let (actual, old) = replacement(&plan, &original, &rows).unwrap();
        assert_eq!(actual, &current);
        assert_eq!(old, &backup);
        for arm in 0..8 {
            let mut changed = rows.clone();
            let mut altered = plan.clone();
            match arm {
                0 => altered.replacement = Some(Uuid::new_v4()),
                1 => changed[1].sha256 = altered.source_a.sha256.clone(),
                2 => changed[1].representation = changed[0].representation.clone(),
                3 => changed[1].package_completion = None,
                4 => {
                    changed[1].remote.as_mut().unwrap().parent_id =
                        Some(cirrove_icloud::ROOT_ID.into())
                }
                5 => changed[1].identity_handoff = None,
                6 => changed[0].remote.as_mut().unwrap().etag = Some("changed-original".into()),
                _ => altered.source_a.semantic = altered.source_b.semantic.clone(),
            }
            assert!(
                admitted(&altered, &local, &mutation, &changed).is_err(),
                "replacement tamper {arm} accepted"
            );
        }
        assert_eq!(imported(&plan, &parent, &rows).unwrap(), &original);
    }
    #[test]
    fn owned_receipt_source_and_trash_guards_refuse_changed_bytes_proof_and_revision() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().join("source.numbers");
        let archive = crate::native_import::synthetic_package_archive(
            "Source.numbers/Metadata/data",
            b"owned",
        );
        std::fs::write(&path, &archive).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let receipt = PackageDownload {
            size: archive.len() as u64,
            sha256: hex::encode(Sha256::digest(&archive)),
        };
        let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
            &File::open(&path).unwrap(),
            &receipt,
            "Source.numbers",
            2,
            &CancellationToken::new(),
        )
        .unwrap();
        let source = Source {
            path: path.clone(),
            size: receipt.size,
            sha256: receipt.sha256.clone(),
            root: "Source.numbers".into(),
            semantic: semantic.clone(),
        };
        source_verified(&source).unwrap();
        let (_, _, _, _, rows) = fixture();
        let mut backup = rows[0].remote.clone().unwrap();
        backup.etag = Some("trash-r1".into());
        let recovered = cirrove_icloud::VerifiedPackageTrash {
            archive: receipt,
            semantic: semantic.clone(),
            trash_etag: "trash-r1".into(),
        };
        trash_binding(&backup, &semantic, &recovered).unwrap();
        backup.etag = Some("trash-r2".into());
        assert!(trash_binding(&backup, &semantic, &recovered).is_err());
        let mut altered = source.clone();
        altered.semantic.sha256 = "f".repeat(64);
        assert!(source_verified(&altered).is_err());
        assert!(trash_binding(&backup, &altered.semantic, &recovered).is_err());
        std::fs::write(path, b"changed source").unwrap();
        assert!(source_verified(&source).is_err());
    }
}

mod fuse;
pub use fuse::{
    icloud_owned_fuse_capture_verify, icloud_owned_fuse_receipt_verify,
    icloud_owned_fuse_source_verify,
};

mod keynote;
pub use keynote::{
    icloud_owned_keynote_import_receipt_verify, icloud_owned_keynote_replacement_receipt_verify,
    icloud_owned_keynote_source_verify,
};

mod numbers_data;
pub use numbers_data::{
    icloud_owned_keynote_data_receipt_verify, icloud_owned_keynote_data_source_verify,
    icloud_owned_numbers_data_receipt_verify, icloud_owned_numbers_data_source_verify,
    icloud_owned_pages_data_receipt_verify, icloud_owned_pages_data_source_verify,
};

mod pages;
pub use pages::icloud_owned_pages_replacement_receipt_verify;
