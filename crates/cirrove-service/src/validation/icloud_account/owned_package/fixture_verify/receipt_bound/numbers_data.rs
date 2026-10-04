//! Owned Numbers raw DATA create and one ordinary in-place FUSE save.
//! Never a PACKAGE, editor-fidelity, mutation or checkpoint-replay observer.
use super::*;
use crate::journal::WorkingFile;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    path: PathBuf,
    size: u64,
    sha256: String,
}
impl RawSource {
    fn verify(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let mut file = open_private(&self.path, false, LIMIT)?;
        let before = file.metadata()?;
        ensure!(
            self.size > 0
                && self.size <= LIMIT
                && before.len() == self.size
                && hex_digest(&self.sha256),
            "owned DATA source bounds changed"
        );
        let mut bounded = (&mut file).take(self.size + 1);
        let mut hash = Sha256::new();
        let mut received = 0u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = bounded.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            received += count as u64;
            ensure!(received <= self.size, "owned DATA source grew");
            hash.update(&buffer[..count]);
        }
        let after = file.metadata()?;
        ensure!(
            received == self.size
                && hex::encode(hash.finalize()) == self.sha256
                && after.len() == before.len()
                && after.dev() == before.dev()
                && after.ino() == before.ino()
                && after.mtime() == before.mtime()
                && after.mtime_nsec() == before.mtime_nsec()
                && after.ctime() == before.ctime()
                && after.ctime_nsec() == before.ctime_nsec(),
            "owned DATA source bytes changed"
        );
        Ok(())
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Sources {
    version: u32,
    run: Uuid,
    session_directory: PathBuf,
    source_a: RawSource,
    source_b: RawSource,
}
impl Sources {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1
                && !self.run.is_nil()
                && self.session_directory == format!("/var/tmp/cirrove-numbers-data-{}", self.run),
            "owned DATA source registration refused"
        );
        for (source, name) in [
            (&self.source_a, "source-a.numbers"),
            (&self.source_b, "source-b.numbers"),
        ] {
            ensure!(
                source.path == self.session_directory.join(name)
                    && source.size > 0
                    && source.size <= LIMIT
                    && hex_digest(&source.sha256),
                "owned DATA source shape refused"
            );
        }
        ensure!(
            self.source_a.sha256 != self.source_b.sha256,
            "owned DATA sources do not differ"
        );
        Ok(())
    }
    fn verify(&self) -> Result<()> {
        self.validate()?;
        self.source_a.verify()?;
        self.source_b.verify()
    }
}
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    CreatedA,
    SavedB,
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
    create: Uuid,
    save: Option<Uuid>,
    owner: Uuid,
    working_a: Uuid,
    working_b: Option<Uuid>,
    phase: Phase,
    created_proof_sha256: Option<String>,
    source_a: RawSource,
    source_b: RawSource,
}
impl Registration {
    fn sources(&self) -> Sources {
        Sources {
            version: self.version,
            run: self.run,
            session_directory: self.session_directory.clone(),
            source_a: self.source_a.clone(),
            source_b: self.source_b.clone(),
        }
    }
    fn scope(&self) -> Scope {
        Scope {
            account: self.account.to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        }
    }
    fn name(&self) -> String {
        format!("Cirrove-Numbers-Data-{}.numbers", self.run)
    }
    fn parent_name(&self) -> String {
        format!("Cirrove-Native-{}", self.run)
    }
    fn prefix(&self) -> &'static str {
        match self.phase {
            Phase::CreatedA => "created",
            Phase::SavedB => "saved",
        }
    }
    fn validate(&self) -> Result<()> {
        self.sources().validate()?;
        ensure!(
            !self.account.is_nil()
                && self.label == "iCloudNumbersDataValidation"
                && hex_digest(&self.settings_sha256)
                && !self.parent_creation.is_nil()
                && !self.create.is_nil()
                && self.create != self.parent_creation
                && !self.owner.is_nil()
                && !self.working_a.is_nil(),
            "owned DATA receipt registration refused"
        );
        match self.phase {
            Phase::CreatedA => ensure!(
                self.save.is_none()
                    && self.working_b.is_none()
                    && self.created_proof_sha256.is_none(),
                "owned DATA create scope changed"
            ),
            Phase::SavedB => ensure!(
                self.save.is_some_and(|id| !id.is_nil()
                    && id != self.create
                    && id != self.parent_creation)
                    && self.working_b.is_some_and(|id| !id.is_nil())
                    && self
                        .created_proof_sha256
                        .as_ref()
                        .is_some_and(|s| hex_digest(s)),
                "owned DATA save scope changed"
            ),
        }
        Ok(())
    }
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8], digest: &str) -> Result<T> {
    ensure!(
        bytes.len() <= 32 * 1024
            && hex_digest(digest)
            && hex::encode(Sha256::digest(bytes)) == digest,
        "owned DATA registration digest changed"
    );
    serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!("owned DATA schema refused"))
}
fn source_observation(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let bytes = read_private(path, 32 * 1024)?;
    let sources: Sources = decode(&bytes, digest)?;
    sources.validate()?;
    ensure!(
        path == sources.session_directory.join("source-registration.json"),
        "owned DATA source registration location changed"
    );
    let held = open_private(&sources.session_directory, true, 0)?;
    sources.verify()?;
    ensure!(
        read_private(path, 32 * 1024)? == bytes,
        "owned DATA source registration changed"
    );
    sources.verify()?;
    same_directory(&sources.session_directory, &held)?;
    let proof = serde_json::json!({"run":sources.run,"source_a":sources.source_a,"source_b":sources.source_b,
        "registration_sha256":digest,"raw_source_identity_verified":true,"representation_observed":false,
        "offline_only":true,"cloud_mutated":false,"gui_fidelity_verified":false});
    record(
        &sources.session_directory.join("source-verified.json"),
        &proof,
    )?;
    Ok(proof)
}
pub fn icloud_owned_numbers_data_source_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    source_observation(path, digest)
        .map_err(|_| anyhow::anyhow!("owned Numbers DATA source refused; no cloud access"))
}
fn account_binding(plan: &Registration) -> Result<Account> {
    let _state = open_private(&plan.session_directory.join("state"), true, 0)?;
    let bytes = read_private(
        &plan.session_directory.join("state/accounts.json"),
        256 * 1024,
    )?;
    ensure!(
        hex::encode(Sha256::digest(&bytes)) == plan.settings_sha256,
        "owned DATA settings changed"
    );
    let settings: Settings = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("owned DATA settings refused"))?;
    ensure!(
        settings.version == 2 && settings.accounts.len() == 1,
        "owned DATA account inventory changed"
    );
    let account = settings.accounts[0].clone();
    ensure!(
        account.id == plan.account.to_string()
            && account.label == plan.label
            && account.enabled
            && account.access == cirrove_auth::AccessMode::ReadWrite
            && matches!(account.registration, cirrove_auth::AppRegistration::ICloud)
            && account.drive.id == "drive"
            && account.root_id == cirrove_icloud::ROOT_ID
            && account.mount_path == plan.session_directory.join("mount"),
        "owned DATA account changed"
    );
    Ok(account)
}
fn parent_binding(
    plan: &Registration,
    mutations: &[MutationRecord],
    parent: &NamespaceObject,
) -> Result<Node> {
    ensure!(mutations.len() == 1, "owned DATA folder inventory changed");
    let row = &mutations[0];
    ensure!(
        row.id == plan.parent_creation
            && row.request.scope == plan.scope()
            && row.state == MutationState::Applied
            && row.attempt.is_none()
            && row.base.is_none()
            && row.working_file.is_none()
            && row.request.intent
                == MutationIntent::CreateFolder {
                    parent: cirrove_icloud::ROOT_ID.into(),
                    name: plan.parent_name()
                },
        "owned DATA folder receipt changed"
    );
    let Some(MutationReceipt::Upsert(created)) = &row.receipt else {
        bail!("owned DATA folder receipt absent");
    };
    let remote = parent
        .remote
        .as_ref()
        .context("owned DATA parent remote absent")?;
    let mut expected = remote.clone();
    expected.id.clone_from(&parent.node.id);
    ensure!(
        parent.scope == plan.scope()
            && parent.follows_remote
            && parent.remote_owned
            && !parent.unlinked
            && parent.latest.is_none()
            && parent.working_file.is_none()
            && parent.native_archive.is_none()
            && parent.node.id == format!("local-directory-{}", parent.id)
            && parent.node == expected
            && remote.id == created.id
            && remote.id.starts_with("FOLDER::com.apple.CloudDocs::")
            && !remote.id.ends_with("::")
            && remote.kind == NodeKind::Folder
            && !remote.package
            && remote.target.is_none()
            && remote.parent_id.as_deref() == Some(cirrove_icloud::ROOT_ID)
            && remote.name == plan.parent_name()
            && created.kind == remote.kind
            && created.parent_id == remote.parent_id
            && created.name == remote.name
            && !created.package
            && created.target.is_none(),
        "owned DATA parent handoff changed"
    );
    Ok(remote.clone())
}
fn ordinary(node: &Node, parent: &str, name: &str, size: u64) -> Result<()> {
    ensure!(
        node.id.starts_with("FILE::com.apple.CloudDocs::")
            && !node.id.ends_with("::")
            && node.parent_id.as_deref() == Some(parent)
            && node.name == name
            && node.size == size
            && node.kind == NodeKind::File
            && !node.package
            && node.target.is_none()
            && node
                .etag
                .as_ref()
                .is_some_and(|e| !e.is_empty() && !e.contains('*')),
        "owned DATA ordinary identity changed"
    );
    Ok(())
}
#[derive(serde::Serialize)]
struct Frontier {
    objects: Vec<NamespaceObject>,
    working: Vec<WorkingFile>,
    auxiliary: (i64, i64, i64),
    working_counts: (i64, i64),
    other: (i64, i64),
    incomplete: i64,
    working_digest: Option<(u64, String)>,
    namespace_operations: Vec<(String, String)>,
    #[allow(clippy::type_complexity)]
    columns: Vec<(String, String, u64, String, Option<u64>, Option<bool>)>,
}
fn admitted(
    plan: &Registration,
    rows: &[UploadRecord],
    mutations: &[MutationRecord],
    associations: &[NamespaceObject],
    frontier: &Frontier,
) -> Result<(Node, Node, Option<Node>)> {
    plan.validate()?;
    let saved = plan.phase == Phase::SavedB;
    ensure!(
        rows.len() == if saved { 2 } else { 1 },
        "owned DATA upload inventory changed"
    );
    let mut expected_columns = rows
        .iter()
        .map(|r| {
            (
                "upload".to_owned(),
                r.id.to_string(),
                r.sequence,
                "uploaded".to_owned(),
                Some(r.sequence),
                Some(true),
            )
        })
        .chain(mutations.iter().map(|r| {
            (
                "mutation".to_owned(),
                r.id.to_string(),
                r.sequence,
                "applied".to_owned(),
                Some(r.sequence),
                Some(true),
            )
        }))
        .collect::<Vec<_>>();
    expected_columns.sort_by_key(|r| r.2);
    ensure!(
        frontier.columns == expected_columns,
        "owned DATA SQL operation and queue binding changed"
    );
    ensure!(
        frontier.auxiliary == (if saved { 3 } else { 2 }, 0, if saved { 3 } else { 2 })
            && frontier.other == (0, 0)
            && frontier.incomplete == 0
            && frontier.objects.len() == frontier.auxiliary.0 as usize
            && frontier.working_counts == (frontier.working.len() as i64, 0)
            && frontier.working.len() <= 1,
        "owned DATA journal frontier changed"
    );
    let parent_object = frontier
        .objects
        .iter()
        .find(|o| {
            o.remote
                .as_ref()
                .is_some_and(|n| n.kind == NodeKind::Folder && n.name == plan.parent_name())
        })
        .context("owned DATA parent namespace absent")?;
    let parent = parent_binding(plan, mutations, parent_object)?;
    let mut expected_pairs = rows
        .iter()
        .map(|row| (row.id.to_string(), plan.owner.to_string()))
        .chain(std::iter::once((
            plan.parent_creation.to_string(),
            parent_object.id.to_string(),
        )))
        .collect::<Vec<_>>();
    expected_pairs.sort();
    ensure!(
        frontier.namespace_operations == expected_pairs,
        "owned DATA complete namespace operation binding changed"
    );
    let created = &rows[0];
    ensure!(
        created.id == plan.create
            && created.base.is_none()
            && created.identity_handoff.is_none()
            && created.working_file == Some(plan.working_a)
            && created.intent
                == UploadIntent::Create {
                    parent: parent.id.clone(),
                    name: plan.name()
                },
        "owned DATA create lineage changed"
    );
    for (row, source) in rows.iter().zip([&plan.source_a, &plan.source_b]) {
        ensure!(
            row.scope == plan.scope()
                && row.state == UploadState::Uploaded
                && row.attempt.is_none()
                && row.representation == UploadRepresentation::FileBytes
                && row.package_completion.is_none()
                && row.size == source.size
                && row.sha256 == source.sha256,
            "owned DATA raw upload receipt changed"
        );
    }
    let original = created
        .remote
        .as_ref()
        .context("owned DATA A identity absent")?;
    ordinary(original, &parent.id, &plan.name(), plan.source_a.size)?;
    let (latest, current, backup, working_id) = if saved {
        let row = &rows[1];
        ensure!(
            Some(row.id) == plan.save
                && row.sequence > created.sequence
                && row.working_file == plan.working_b
                && row.intent
                    == UploadIntent::Replace {
                        item: original.id.clone(),
                        expected_etag: original.etag.clone().unwrap_or_default()
                    },
            "owned DATA save lineage changed"
        );
        let chained = plan.working_b == Some(plan.working_a);
        ensure!(
            match &row.base {
                Some(base) => chained && base.predecessor == plan.create && base.resolved,
                None => !chained,
            },
            "owned DATA working predecessor changed"
        );
        let (current, backup) = row
            .ordinary_handoff_receipt()
            .context("owned DATA typed handoff absent")?;
        ordinary(current, &parent.id, &plan.name(), plan.source_b.size)?;
        ordinary(
            backup,
            "FOLDER::com.apple.CloudDocs::TRASH_ROOT",
            &plan.name(),
            plan.source_a.size,
        )?;
        ensure!(
            backup.id == original.id && current.id != original.id,
            "owned DATA original identity changed"
        );
        (
            row,
            current,
            Some(backup.clone()),
            plan.working_b.context("owned DATA working B missing")?,
        )
    } else {
        (created, original, None, plan.working_a)
    };
    // Ordinary FUSE sealing binds every operation to the stable namespace owner.
    let owner = frontier
        .objects
        .iter()
        .find(|o| o.id == plan.owner)
        .context("owned DATA owner absent")?;
    ensure!(
        associations.len() == rows.len()
            && associations.iter().all(|o| o.id == plan.owner
                && serde_json::to_value(o).ok() == serde_json::to_value(owner).ok()),
        "owned DATA actual working association changed"
    );
    ensure!(
        owner.scope == plan.scope()
            && owner.remote_owned
            && !owner.unlinked
            && owner.native_archive.is_none()
            && owner.remote.as_ref() == Some(current)
            && owner.remote_sequence == latest.sequence
            && owner.node.kind == NodeKind::File
            && !owner.node.package
            && owner.node.target.is_none()
            && owner.node.parent_id == Some(parent_object.node.id.clone())
            && owner.node.name == plan.name()
            && owner.node.size == current.size,
        "owned DATA published owner changed"
    );
    if owner.follows_remote {
        ensure!(
            owner.latest.is_none()
                && owner.working_file.is_none()
                && frontier.working.is_empty()
                && frontier.working_digest.is_none(),
            "owned DATA retired working frontier changed"
        );
        let mut expected = current.clone();
        expected.id.clone_from(&owner.node.id);
        expected.parent_id = Some(parent_object.node.id.clone());
        ensure!(
            owner.node == expected,
            "owned DATA retired publication changed"
        );
    } else {
        ensure!(
            owner.latest == Some(latest.id)
                && owner.working_file == Some(working_id)
                && frontier.working.len() == 1,
            "owned DATA active working ownership changed"
        );
        let working = &frontier.working[0];
        let source = if saved {
            &plan.source_b
        } else {
            &plan.source_a
        };
        ensure!(
            working.id == working_id
                && working.scope == plan.scope()
                && !working.native
                && !working.dirty
                && !working.unlinked
                && working.latest == Some(latest.id)
                && working.node == owner.node
                && frontier.working_digest == Some((source.size, source.sha256.clone())),
            "owned DATA active working bytes changed"
        );
    }
    if let Some(backup) = &backup {
        let recovery = frontier
            .objects
            .iter()
            .find(|o| o.remote.as_ref() == Some(backup))
            .context("owned DATA recovery owner absent")?;
        let mut expected_recovery = backup.clone();
        expected_recovery.id = format!("local-recovery-{}", recovery.id);
        expected_recovery.parent_id = Some(parent.id.clone());
        expected_recovery.name = format!("recovery-by-cirrove-{}.txt", latest.id);
        ensure!(
            Some(recovery.id) == latest.ordinary_validation_recovery_owner()
                && recovery.id != owner.id
                && recovery.scope == plan.scope()
                && recovery.unlinked
                && recovery.remote_owned
                && !recovery.follows_remote
                && recovery.native_archive.is_none()
                && recovery.working_file.is_none()
                && recovery.latest.is_none()
                && recovery.remote_sequence == latest.sequence
                && recovery.node.kind == NodeKind::File
                && !recovery.node.package
                && recovery.node.target.is_none()
                && recovery.node.name == format!("recovery-by-cirrove-{}.txt", latest.id)
                && recovery.node.parent_id == Some(parent.id.clone()),
            "owned DATA hidden recovery binding changed"
        );
        ensure!(
            recovery.node == expected_recovery,
            "owned DATA full recovery projection changed"
        );
    }
    Ok((parent, current.clone(), backup))
}
fn journal_binding(
    plan: &Registration,
    journal: &RecoveryJournal,
) -> Result<(Node, Node, Option<Node>, Vec<u8>)> {
    let rows = journal.list(0, 3)?;
    let mutations = journal.native_validation_mutations(0, 2)?;
    let associations = rows
        .iter()
        .map(|r| {
            journal
                .ordinary_validation_namespace_for_operation(r.id)?
                .ok_or_else(|| anyhow::anyhow!("owned DATA operation association absent"))
        })
        .collect::<Result<Vec<_>>>()?;
    let (objects, working, other) = journal.ordinary_validation_inventory()?;
    let working_digest = working
        .first()
        .map(|w| journal.ordinary_validation_working_digest(w.id))
        .transpose()?;
    let frontier = Frontier {
        objects,
        working,
        other,
        working_digest,
        auxiliary: journal.native_validation_import_auxiliary_inventory()?,
        working_counts: journal.native_validation_working_inventory()?,
        incomplete: journal.native_validation_incomplete_queue()?,
        namespace_operations: journal.ordinary_validation_namespace_operations()?,
        columns: journal.ordinary_validation_operation_columns()?,
    };
    let (parent, current, backup) = admitted(plan, &rows, &mutations, &associations, &frontier)?;
    let frozen = serde_json::to_vec(
        &serde_json::json!({"rows":rows,"mutations":mutations,"associations":associations,"frontier":frontier}),
    )?;
    Ok((parent, current, backup, frozen))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentDataProof {
    run: Uuid,
    account: Uuid,
    parent: String,
    item: String,
    etag: String,
    representation: FixtureRepresentation,
    format: String,
    size: u64,
    sha256: String,
    semantic: Option<serde_json::Value>,
    manifest_sha256: String,
    content_identity_verified: bool,
    gui_fidelity_verified: bool,
    cloud_mutated: bool,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatedProof {
    run: Uuid,
    account: Uuid,
    phase: Phase,
    parent_creation: Uuid,
    create: Uuid,
    save: Option<Uuid>,
    owner: Uuid,
    working_a: Uuid,
    working_b: Option<Uuid>,
    created_proof_sha256: Option<String>,
    registration_sha256: String,
    fixture_manifest_sha256: String,
    current: CurrentDataProof,
    trash: Option<serde_json::Value>,
    ordinary_data_create_verified: bool,
    ordinary_data_save_verified: bool,
    cloud_mutated: bool,
    gui_fidelity_verified: bool,
}
fn created_proof_binding(
    plan: &Registration,
    original: &Node,
    parent: &Node,
    bytes: &[u8],
) -> Result<CreatedProof> {
    let proof: CreatedProof = decode(
        bytes,
        plan.created_proof_sha256
            .as_deref()
            .context("owned DATA prior proof digest absent")?,
    )?;
    ensure!(
        proof.run == plan.run
            && proof.account == plan.account
            && proof.phase == Phase::CreatedA
            && proof.parent_creation == plan.parent_creation
            && proof.create == plan.create
            && proof.owner == plan.owner
            && proof.working_a == plan.working_a
            && proof.save.is_none()
            && proof.working_b.is_none()
            && proof.created_proof_sha256.is_none()
            && proof.trash.is_none()
            && proof.ordinary_data_create_verified
            && !proof.ordinary_data_save_verified
            && !proof.cloud_mutated
            && !proof.gui_fidelity_verified
            && hex_digest(&proof.registration_sha256)
            && hex_digest(&proof.fixture_manifest_sha256),
        "owned DATA prior create artifact changed"
    );
    let current = &proof.current;
    ensure!(
        current.run == plan.run
            && current.account == plan.account
            && current.parent == parent.id
            && current.item == original.id
            && Some(current.etag.as_str()) == original.etag.as_deref()
            && current.representation == FixtureRepresentation::Data
            && current.format == "numbers"
            && current.size == plan.source_a.size
            && current.sha256 == plan.source_a.sha256
            && current.semantic.is_none()
            && current.manifest_sha256 == proof.fixture_manifest_sha256
            && current.content_identity_verified
            && !current.gui_fidelity_verified
            && !current.cloud_mutated,
        "owned DATA prior independent DATA proof changed"
    );
    Ok(proof)
}
fn prior_created(plan: &Registration, original: &Node, parent: &Node) -> Result<Option<Vec<u8>>> {
    if plan.phase == Phase::CreatedA {
        return Ok(None);
    }
    let bytes = read_private(
        &plan.session_directory.join("created-verified.json"),
        32 * 1024,
    )?;
    let proof = created_proof_binding(plan, original, parent, &bytes)?;
    let prior_bytes = read_private(
        &plan.session_directory.join("created-registration.json"),
        32 * 1024,
    )?;
    let prior: Registration = decode(&prior_bytes, &proof.registration_sha256)?;
    prior.validate()?;
    let mut expected = plan.clone();
    expected.phase = Phase::CreatedA;
    expected.save = None;
    expected.working_b = None;
    expected.created_proof_sha256 = None;
    ensure!(
        serde_json::to_value(&prior)? == serde_json::to_value(expected)?,
        "owned DATA prior registration binding changed"
    );
    let fixture = read_private(
        &plan.session_directory.join("created-fixture.json"),
        32 * 1024,
    )?;
    ensure!(
        hex::encode(Sha256::digest(fixture)) == proof.fixture_manifest_sha256,
        "owned DATA prior fixture registration changed"
    );
    Ok(Some(bytes))
}
async fn observation(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let bytes = read_private(path, 32 * 1024)?;
    let plan: Registration = decode(&bytes, digest)?;
    plan.validate()?;
    ensure!(
        path == plan
            .session_directory
            .join(format!("{}-registration.json", plan.prefix())),
        "owned DATA registration location changed"
    );
    let held = open_private(&plan.session_directory, true, 0)?;
    let account = account_binding(&plan)?;
    plan.sources().verify()?;
    let journal_path = plan
        .session_directory
        .join("state/accounts")
        .join(&account.id)
        .join("journal");
    let journal_held = open_private(&journal_path, true, 0)?;
    let journal = RecoveryJournal::open(&journal_path, &account.id)?;
    let (parent, current, backup, frozen) = journal_binding(&plan, &journal)?;
    let rows = journal.list(0, 3)?;
    let original = rows[0]
        .remote
        .as_ref()
        .context("owned DATA original receipt absent")?;
    let prior_proof = prior_created(&plan, original, &parent)?;
    let mut remote = session(&plan.session_directory, &account).await?;
    let entries = remote.list_folder(cirrove_icloud::ROOT_ID).await?;
    let mut matching = entries.iter().filter(|e| e.drivewsid == parent.id);
    let mut parent_entry = matching.next().context("owned DATA parent absent")?.clone();
    ensure!(
        matching.next().is_none()
            && parent_entry.kind == "FOLDER"
            && parent_entry.zone == "com.apple.CloudDocs"
            && parent_entry.name == plan.parent_name(),
        "owned DATA parent changed"
    );
    parent_entry.items.clear();
    parent_entry.number_of_items = None;
    let document = exact_entry(&remote.list_folder(&parent.id).await?, &current)?;
    let source = if plan.phase == Phase::SavedB {
        &plan.source_b
    } else {
        &plan.source_a
    };
    let fixture_path = plan
        .session_directory
        .join(format!("{}-fixture.json", plan.prefix()));
    record(
        &fixture_path,
        &serde_json::json!({"version":1,"run":plan.run,"account":plan.account,
        "session_directory":plan.session_directory,"settings_sha256":plan.settings_sha256,"parent":parent_entry,
        "document":document,"format":"numbers","representation":"data","source":source.path,
        "source_size":source.size,"source_sha256":source.sha256,
        "source_root":if plan.phase == Phase::SavedB { "source-b.numbers" } else { "source-a.numbers" },
        "expected_root":plan.name(),"semantic":null}),
    )?;
    let fixture_digest = hex::encode(Sha256::digest(read_private(&fixture_path, 32 * 1024)?));
    let current_proof = icloud_owned_fixture_verify(&fixture_path, &fixture_digest).await?;
    let trash_proof = if let Some(backup) = &backup {
        // Narrow read-only API; exact saved original revision comes from create A.
        let observed = remote
            .verify_owned_data_in_trash(original, backup, &plan.source_a.sha256)
            .await?;
        ensure!(&observed == backup, "owned DATA Trash receipt changed");
        Some(
            serde_json::json!({"item":observed.id,"etag":observed.etag,"size":observed.size,
            "sha256":plan.source_a.sha256,"raw_content_identity_verified":true}),
        )
    } else {
        None
    };
    ensure!(
        exact_entry(&remote.list_folder(&parent.id).await?, &current)? == document,
        "owned DATA current changed during proof"
    );
    let end_entries = remote.list_folder(cirrove_icloud::ROOT_ID).await?;
    let mut matching = end_entries.iter().filter(|e| e.drivewsid == parent.id);
    let mut end_parent = matching
        .next()
        .context("owned DATA parent disappeared")?
        .clone();
    end_parent.items.clear();
    end_parent.number_of_items = None;
    ensure!(
        matching.next().is_none() && end_parent == parent_entry,
        "owned DATA parent changed during proof"
    );
    ensure!(
        read_private(path, 32 * 1024)? == bytes,
        "owned DATA registration changed during proof"
    );
    account_binding(&plan)?;
    plan.sources().verify()?;
    ensure!(
        prior_created(&plan, original, &parent)? == prior_proof,
        "owned DATA prior proof changed during observation"
    );
    let (end_parent, end_current, end_backup, end_frozen) = journal_binding(&plan, &journal)?;
    ensure!(
        end_parent == parent
            && end_current == current
            && end_backup == backup
            && end_frozen == frozen,
        "owned DATA journal changed during proof"
    );
    same_directory(&plan.session_directory, &held)?;
    same_directory(&journal_path, &journal_held)?;
    let proof = serde_json::json!({"run":plan.run,"account":plan.account,"phase":plan.phase,
        "parent_creation":plan.parent_creation,"create":plan.create,"save":plan.save,"owner":plan.owner,
        "working_a":plan.working_a,"working_b":plan.working_b,"registration_sha256":digest,
        "created_proof_sha256":plan.created_proof_sha256,
        "fixture_manifest_sha256":fixture_digest,"current":current_proof,"trash":trash_proof,
        "ordinary_data_create_verified":true,"ordinary_data_save_verified":plan.phase == Phase::SavedB,
        "cloud_mutated":false,"gui_fidelity_verified":false});
    record(
        &plan
            .session_directory
            .join(format!("{}-verified.json", plan.prefix())),
        &proof,
    )?;
    Ok(proof)
}
pub async fn icloud_owned_numbers_data_receipt_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    tokio::time::timeout(
        std::time::Duration::from_secs(900),
        observation(path, digest),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!("owned Numbers DATA observation timed out; no mutation submitted")
    })?
    .map_err(|_| anyhow::anyhow!("owned Numbers DATA observation refused; no mutation submitted"))
}

#[cfg(test)]
mod tests;
