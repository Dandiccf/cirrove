//! One owned Keynote CLI import: offline source proof and read-only receipt proof.
//! Import APIs retain their original scope. The separate replacement observer
//! verifies registered receipts/current/Trash content, never GUI fidelity.
use super::*;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Sources {
    version: u32,
    run: Uuid,
    session_directory: PathBuf,
    source: Source,
}
impl Sources {
    fn validate(&self) -> Result<()> {
        self.source.semantic.validate()?;
        ensure!(
            self.version == 1
                && !self.run.is_nil()
                && self.session_directory
                    == format!("/var/tmp/cirrove-keynote-import-{}", self.run)
                && self.source.path == self.session_directory.join("source-a.key")
                && self.source.root == "Source.key"
                && self.source.size > 0
                && self.source.size <= LIMIT
                && hex_digest(&self.source.sha256)
                && self.source.semantic.version == 2,
            "owned Keynote source registration refused"
        );
        Ok(())
    }
    fn verify(&self) -> Result<()> {
        self.validate()?;
        source_verified(&self.source)
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KeynoteRegistration {
    version: u32,
    run: Uuid,
    account: Uuid,
    label: String,
    session_directory: PathBuf,
    settings_sha256: String,
    parent_creation: Uuid,
    import: Uuid,
    source: Source,
}
impl KeynoteRegistration {
    fn sources(&self) -> Sources {
        Sources {
            version: self.version,
            run: self.run,
            session_directory: self.session_directory.clone(),
            source: self.source.clone(),
        }
    }
    fn name(&self) -> String {
        format!("Cirrove-Keynote-Import-{}.key", self.run)
    }
    fn validate(&self) -> Result<()> {
        self.sources().validate()?;
        ensure!(
            !self.account.is_nil()
                && self.label == "iCloudKeynoteImportValidation"
                && hex_digest(&self.settings_sha256)
                && !self.parent_creation.is_nil()
                && !self.import.is_nil()
                && self.import != self.parent_creation,
            "owned Keynote import registration refused"
        );
        Ok(())
    }
    // Only unchanged settings/parent helpers consume this internal view. Its
    // Numbers-only source/name validator and import/replacement helpers never run.
    fn parent_plan(&self) -> Registration {
        Registration {
            version: self.version,
            run: self.run,
            account: self.account,
            label: self.label.clone(),
            session_directory: self.session_directory.clone(),
            settings_sha256: self.settings_sha256.clone(),
            parent_creation: self.parent_creation,
            import: self.import,
            replacement: None,
            source_a: self.source.clone(),
            source_b: self.source.clone(),
        }
    }
}
fn decode_keynote<T: serde::de::DeserializeOwned>(bytes: &[u8], digest: &str) -> Result<T> {
    ensure!(
        bytes.len() <= 32 * 1024
            && hex_digest(digest)
            && hex::encode(Sha256::digest(bytes)) == digest,
        "owned Keynote registration digest changed"
    );
    serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("owned Keynote registration schema refused"))
}
fn verify_sources(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let bytes = read_private(path, 32 * 1024)?;
    let sources: Sources = decode_keynote(&bytes, digest)?;
    sources.validate()?;
    ensure!(
        path == sources.session_directory.join("source-registration.json"),
        "owned Keynote source registration location refused"
    );
    let directory = open_private(&sources.session_directory, true, 0)?;
    sources.verify()?;
    ensure!(
        read_private(path, 32 * 1024)? == bytes,
        "owned Keynote source registration changed"
    );
    sources.verify()?;
    same_directory(&sources.session_directory, &directory)?;
    let proof = serde_json::json!({"run":sources.run,"source":sources.source,"registration_sha256":digest,
        "source_identity_verified":true,"offline_only":true,"cloud_mutated":false,"gui_fidelity_verified":false});
    record(
        &sources.session_directory.join("source-verified.json"),
        &proof,
    )?;
    Ok(proof)
}
pub fn icloud_owned_keynote_source_verify(path: &Path, digest: &str) -> Result<serde_json::Value> {
    verify_sources(path, digest)
        .map_err(|_| anyhow::anyhow!("owned Keynote source observation refused; no cloud access"))
}
fn imported_keynote<'a>(
    plan: &KeynoteRegistration,
    parent: &Node,
    rows: &'a [UploadRecord],
) -> Result<&'a Node> {
    ensure!(rows.len() == 1, "owned Keynote upload inventory changed");
    let row = &rows[0];
    ensure!(
        row.id == plan.import
            && row.scope == plan.parent_plan().scope()
            && row.state == UploadState::Uploaded
            && row.attempt.is_none()
            && row.base.is_none()
            && row.working_file.is_none()
            && row.identity_handoff.is_none()
            && row.intent
                == UploadIntent::Create {
                    parent: parent.id.clone(),
                    name: plan.name()
                }
            && row.representation
                == UploadRepresentation::PackageArchive {
                    expected_root: plan.source.root.clone(),
                    semantic: plan.source.semantic.clone()
                }
            && row.size == plan.source.size
            && row.sha256 == plan.source.sha256
            && row.package_completion.as_ref() == Some(&plan.source.semantic),
        "owned Keynote import source receipt changed"
    );
    let node = row
        .remote
        .as_ref()
        .context("owned Keynote import identity absent")?;
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
        "owned Keynote imported identity changed"
    );
    Ok(node)
}
#[derive(serde::Serialize)]
struct ImportFrontier {
    incomplete: i64,
    working: (i64, i64),
    auxiliary: (i64, i64, i64),
}
fn admitted(
    plan: &KeynoteRegistration,
    mutations: &[MutationRecord],
    local: &NamespaceObject,
    rows: &[UploadRecord],
    published: &PackagePublicationStatus,
    frontier: &ImportFrontier,
) -> Result<(Node, Node)> {
    plan.validate()?;
    let parent = parent_binding(&plan.parent_plan(), mutations, local)?;
    ensure!(
        frontier.incomplete == 0
            && frontier.working == (0, 0)
            && frontier.auxiliary.0 == 1
            && frontier.auxiliary.1 == 0,
        "owned Keynote write frontier unfinished"
    );
    ensure!(
        frontier.auxiliary.2 == 2,
        "owned Keynote total operation queue changed"
    );
    let current = imported_keynote(plan, &parent, rows)?;
    publication(published, current)?;
    Ok((parent, current.clone()))
}
fn journal_binding(
    plan: &KeynoteRegistration,
    journal: &RecoveryJournal,
) -> Result<(Node, Node, Vec<u8>)> {
    let rows = journal.list(0, 2)?;
    let mutations = journal.native_validation_mutations(0, 2)?;
    let mutation = mutations
        .iter()
        .find(|row| row.id == plan.parent_creation)
        .context("owned Keynote parent mutation missing")?;
    let Some(MutationReceipt::Upsert(created)) = &mutation.receipt else {
        bail!("owned Keynote parent receipt missing");
    };
    let local = journal
        .native_validation_namespace_by_remote(&plan.parent_plan().scope(), &created.id)?
        .context("owned Keynote parent namespace missing")?;
    let frontier = ImportFrontier {
        incomplete: journal.native_validation_incomplete_queue()?,
        working: journal.native_validation_working_inventory()?,
        auxiliary: journal.native_validation_import_auxiliary_inventory()?,
    };
    let (parent, current) = admitted(
        plan,
        &mutations,
        &local,
        &rows,
        &journal.native_validation_package_publication(plan.import)?,
        &frontier,
    )?;
    let frozen = serde_json::to_vec(
        &serde_json::json!({"rows":rows,"mutations":mutations,"parent_namespace":local,"frontier":frontier}),
    )?;
    Ok((parent, current, frozen))
}
async fn observe_keynote(path: &Path, digest: &str) -> Result<serde_json::Value> {
    let bytes = read_private(path, 32 * 1024)?;
    let plan: KeynoteRegistration = decode_keynote(&bytes, digest)?;
    plan.validate()?;
    ensure!(
        path == plan.session_directory.join("import-registration.json"),
        "owned Keynote receipt registration location refused"
    );
    let directory = open_private(&plan.session_directory, true, 0)?;
    let account = settings(&plan.parent_plan())?;
    plan.sources().verify()?;
    let journal_path = plan
        .session_directory
        .join("state/accounts")
        .join(&account.id)
        .join("journal");
    let journal_directory = open_private(&journal_path, true, 0)?;
    // Lease held throughout independent read and all before/after fences.
    let journal = RecoveryJournal::open(&journal_path, &account.id)?;
    let (parent, current, frozen) = journal_binding(&plan, &journal)?;
    let mut remote = session(&plan.session_directory, &account).await?;
    let root = remote.list_folder(cirrove_icloud::ROOT_ID).await?;
    let mut parents = root.iter().filter(|e| e.drivewsid == parent.id);
    let mut parent_entry = parents
        .next()
        .context("owned Keynote parent absent")?
        .clone();
    ensure!(
        parents.next().is_none()
            && parent_entry.kind == "FOLDER"
            && parent_entry.zone == "com.apple.CloudDocs"
            && parent_entry.name == plan.parent_plan().parent_name(),
        "owned Keynote active parent changed"
    );
    parent_entry.items.clear();
    parent_entry.number_of_items = None;
    let document = exact_entry(&remote.list_folder(&parent.id).await?, &current)?;
    let fixture_path = plan.session_directory.join("import-fixture.json");
    let fixture = serde_json::json!({"version":1,"run":plan.run,"account":plan.account,"session_directory":plan.session_directory,
        "settings_sha256":plan.settings_sha256,"parent":parent_entry,"document":document,"format":"keynote","representation":"package",
        "source":plan.source.path,"source_size":plan.source.size,"source_sha256":plan.source.sha256,
        "source_root":plan.source.root,"expected_root":plan.name(),"semantic":plan.source.semantic});
    record(&fixture_path, &fixture)?;
    let fixture_digest = hex::encode(Sha256::digest(read_private(&fixture_path, 32 * 1024)?));
    let current_proof = icloud_owned_fixture_verify(&fixture_path, &fixture_digest).await?;
    ensure!(
        exact_entry(&remote.list_folder(&parent.id).await?, &current)? == document,
        "owned Keynote current revision changed during proof"
    );
    let end_root = remote.list_folder(cirrove_icloud::ROOT_ID).await?;
    let mut parents = end_root.iter().filter(|e| e.drivewsid == parent.id);
    let mut end_parent = parents
        .next()
        .context("owned Keynote parent disappeared")?
        .clone();
    end_parent.items.clear();
    end_parent.number_of_items = None;
    ensure!(
        parents.next().is_none() && end_parent == parent_entry,
        "owned Keynote parent changed during proof"
    );
    ensure!(
        read_private(path, 32 * 1024)? == bytes,
        "owned Keynote registration changed during proof"
    );
    settings(&plan.parent_plan())?;
    plan.sources().verify()?;
    let (end_parent, end_current, end_frozen) = journal_binding(&plan, &journal)?;
    ensure!(
        end_parent == parent && end_current == current && end_frozen == frozen,
        "owned Keynote journal changed during proof"
    );
    same_directory(&plan.session_directory, &directory)?;
    same_directory(&journal_path, &journal_directory)?;
    let proof = serde_json::json!({"run":plan.run,"account":plan.account,"parent_creation":plan.parent_creation,"import":plan.import,
        "registration_sha256":digest,"fixture_manifest_sha256":fixture_digest,"current":current_proof,
        "native_import_verified":true,"cloud_mutated":false,"gui_fidelity_verified":false});
    record(&plan.session_directory.join("import-verified.json"), &proof)?;
    Ok(proof)
}
pub async fn icloud_owned_keynote_import_receipt_verify(
    path: &Path,
    digest: &str,
) -> Result<serde_json::Value> {
    tokio::time::timeout(
        std::time::Duration::from_secs(900),
        observe_keynote(path, digest),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!("owned Keynote receipt observation timed out; no mutation submitted")
    })?
    .map_err(|_| {
        anyhow::anyhow!("owned Keynote receipt observation refused; no mutation submitted")
    })
}

#[cfg(test)]
mod tests;

pub(super) mod replacement;
pub use replacement::icloud_owned_keynote_replacement_receipt_verify;
