//! Validation-only ordinary DATA atomic takeover. No writer or replay API.
use super::*;
use crate::journal::ReplacementRecord;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Authority {
    pub version: u32,
    pub victim: Uuid,
    pub victim_working: Uuid,
    pub temporary_create: Uuid,
    pub cleanup: Uuid,
    pub cleanup_object: Uuid,
    /// Exact typed objects captured by the controller before the sole rename.
    pub source_before: NamespaceObject,
    pub victim_before: NamespaceObject,
}
impl Authority {
    pub(super) fn validate(&self, plan: &Registration) -> Result<()> {
        ensure!(
            self.version == 1 && plan.phase == Phase::SavedB,
            "owned DATA atomic phase refused"
        );
        let operations = [
            plan.parent_creation,
            plan.create,
            self.temporary_create,
            plan.save.context("owned DATA atomic save absent")?,
            self.cleanup,
        ];
        let owners = [self.victim, plan.owner, self.cleanup_object];
        ensure!(
            operations.iter().all(|id| !id.is_nil())
                && operations
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    == 5
                && owners.iter().all(|id| !id.is_nil())
                && owners
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    == 3
                && self.source_before.id == plan.owner
                && self.victim_before.id == self.victim
                && self.source_before.scope == plan.scope()
                && self.victim_before.scope == plan.scope()
                && self.source_before.revision > 0
                && self.victim_before.revision > 0
                && !self.victim_working.is_nil()
                && plan
                    .working_b
                    .is_some_and(|id| id != self.victim_working && id != plan.working_a),
            "owned DATA atomic identities refused"
        );
        Ok(())
    }
    fn temporary_name(&self, plan: &Registration) -> String {
        format!(".cirrove-atomic-{}.{}", plan.run, plan.arm.extension())
    }
}
#[derive(serde::Deserialize, serde::Serialize)]
struct Snapshot {
    objects: Vec<NamespaceObject>,
    working: Vec<WorkingFile>,
    replacements: Vec<ReplacementRecord>,
    namespace_operations: Vec<(String, String)>,
    #[allow(clippy::type_complexity)]
    columns: Vec<(String, String, u64, String, Option<u64>, Option<bool>)>,
    counts: (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64),
}
fn object(snapshot: &Snapshot, id: Uuid) -> Result<&NamespaceObject> {
    snapshot
        .objects
        .iter()
        .find(|o| o.id == id)
        .context("owned DATA atomic namespace absent")
}
fn same_object(a: &NamespaceObject, b: &NamespaceObject) -> Result<bool> {
    Ok(serde_json::to_value(a)? == serde_json::to_value(b)?)
}
fn raw(row: &UploadRecord, plan: &Registration, source: &RawSource) -> Result<()> {
    ensure!(
        row.scope == plan.scope()
            && row.state == UploadState::Uploaded
            && row.attempt.is_none()
            && row.representation == UploadRepresentation::FileBytes
            && row.package_completion.is_none()
            && row.size == source.size
            && row.sha256 == source.sha256,
        "owned DATA atomic raw generation changed"
    );
    Ok(())
}
fn checked_working(
    snapshot: &Snapshot,
    owner: &NamespaceObject,
    id: Uuid,
    plan: &Registration,
    source: &RawSource,
    unlinked: bool,
    journal: &RecoveryJournal,
) -> Result<()> {
    let latest = if unlinked { owner.latest } else { plan.save };
    let working = snapshot
        .working
        .iter()
        .find(|w| w.id == id)
        .context("owned DATA atomic working missing")?;
    ensure!(
        owner.working_file == Some(id)
            && working.scope == plan.scope()
            && !working.native
            && !working.dirty
            && working.unlinked == unlinked
            && working.latest == latest
            && owner.latest == latest
            && working.node == owner.node
            && journal.ordinary_validation_working_digest(id)?
                == (source.size, source.sha256.clone()),
        "owned DATA atomic working bytes changed"
    );
    Ok(())
}
fn admitted_atomic(
    plan: &Registration,
    authority: &Authority,
    rows: &[UploadRecord],
    mutations: &[MutationRecord],
    snapshot: &Snapshot,
    journal: &RecoveryJournal,
) -> Result<(Node, Node, Option<Node>)> {
    plan.validate()?;
    authority.validate(plan)?;
    ensure!(
        rows.len() == 3
            && mutations.len() == 2
            && snapshot.replacements.len() == 1
            && snapshot.objects.len() == 5
            && snapshot.working.len() <= 2,
        "owned DATA atomic complete inventory changed"
    );
    // The native operation/publication counts remain zero. All five queue rows
    // are completed and every working row is checked below, including victim A.
    let unlinked = snapshot
        .working
        .iter()
        .filter(|w| w.unlinked || w.dirty)
        .count() as i64;
    ensure!(
        snapshot.counts
            == (
                5,
                0,
                5,
                snapshot.working.len() as i64,
                unlinked,
                0,
                1,
                0,
                0,
                0
            ),
        "owned DATA atomic total counts changed"
    );
    let mut columns = rows
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
    columns.sort_by_key(|r| r.2);
    ensure!(
        snapshot.columns == columns,
        "owned DATA atomic SQL queue binding changed"
    );
    let [created, temporary, saved] = rows else {
        bail!("owned DATA atomic uploads changed");
    };
    let [folder, cleanup] = mutations else {
        bail!("owned DATA atomic mutations changed");
    };
    ensure!(
        created.id == plan.create
            && temporary.id == authority.temporary_create
            && Some(saved.id) == plan.save
            && cleanup.id == authority.cleanup
            && folder.id == plan.parent_creation
            && folder.sequence < created.sequence
            && created.sequence < temporary.sequence
            && temporary.sequence < saved.sequence
            && saved.sequence < cleanup.sequence,
        "owned DATA atomic operation order changed"
    );
    let parent_object = snapshot
        .objects
        .iter()
        .find(|o| {
            o.remote
                .as_ref()
                .is_some_and(|n| n.kind == NodeKind::Folder && n.name == plan.parent_name())
        })
        .context("owned DATA atomic folder absent")?;
    let parent = parent_binding(plan, std::slice::from_ref(folder), parent_object)?;
    let source = object(snapshot, plan.owner)?;
    let victim = object(snapshot, authority.victim)?;
    let cleaned = object(snapshot, authority.cleanup_object)?;
    let replacement = &snapshot.replacements[0];
    ensure!(
        replacement.id == saved.id
            && replacement.source == source.id
            && replacement.victim == victim.id
            && replacement.cleanup == cleanup.id
            && replacement.cleanup_object == cleaned.id
            && replacement.local_ready
            && replacement.remote_applied
            && replacement.rescued_as.is_none()
            && replacement.source_unconfirmed_create.is_none()
            && source.revision > authority.source_before.revision
            && victim.revision > authority.victim_before.revision,
        "owned DATA atomic replacement authority changed"
    );
    let expected_pairs = [
        (folder.id, parent_object.id),
        (created.id, victim.id),
        (temporary.id, source.id),
        (saved.id, source.id),
        (cleanup.id, cleaned.id),
    ];
    let mut pairs = expected_pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect::<Vec<_>>();
    pairs.sort();
    ensure!(
        snapshot.namespace_operations == pairs,
        "owned DATA atomic namespace operation authority changed"
    );
    for (operation, owner) in expected_pairs {
        let associated = journal
            .ordinary_validation_namespace_for_operation(operation)?
            .context("owned DATA atomic actual association absent")?;
        ensure!(
            same_object(&associated, object(snapshot, owner)?)?,
            "owned DATA atomic actual association changed"
        );
    }
    raw(created, plan, &plan.source_a)?;
    raw(temporary, plan, &plan.source_b)?;
    raw(saved, plan, &plan.source_b)?;
    let original = created
        .remote
        .as_ref()
        .context("owned DATA atomic original absent")?;
    let temp = temporary
        .remote
        .as_ref()
        .context("owned DATA atomic temporary absent")?;
    ordinary(original, &parent.id, &plan.name(), plan.source_a.size)?;
    ordinary(
        temp,
        &parent.id,
        &authority.temporary_name(plan),
        plan.source_b.size,
    )?;
    ensure!(
        created.base.is_none()
            && created.identity_handoff.is_none()
            && created.working_file == Some(plan.working_a)
            && created.intent
                == UploadIntent::Create {
                    parent: parent.id.clone(),
                    name: plan.name()
                }
            && temporary.base.is_none()
            && temporary.identity_handoff.is_none()
            && temporary.working_file == plan.working_b
            && temporary.intent
                == UploadIntent::Create {
                    parent: parent.id.clone(),
                    name: authority.temporary_name(plan)
                }
            && saved.working_file == plan.working_b
            && saved.intent
                == UploadIntent::Replace {
                    item: original.id.clone(),
                    expected_etag: original.etag.clone().unwrap_or_default()
                }
            && match &saved.base {
                Some(base) =>
                    base.resolved
                        && base.predecessor == created.id
                        && authority.victim_before.latest == Some(created.id),
                None => authority.victim_before.latest.is_none(),
            },
        "owned DATA atomic source/victim receipt lineage changed"
    );
    let (current, backup) = saved
        .ordinary_handoff_receipt()
        .context("owned DATA atomic typed replacement receipt absent")?;
    ordinary(current, &parent.id, &plan.name(), plan.source_b.size)?;
    ordinary(
        backup,
        "FOLDER::com.apple.CloudDocs::TRASH_ROOT",
        &plan.name(),
        plan.source_a.size,
    )?;
    ensure!(
        backup.id == original.id
            && current.id != original.id
            && temp.id != current.id
            && temp.id != original.id,
        "owned DATA atomic cloud identities changed"
    );
    ensure!(
        cleanup.request.scope == plan.scope()
            && cleanup.state == MutationState::Applied
            && cleanup.attempt.is_none()
            && cleanup.local_ready
            && cleanup.working_file.is_none()
            && cleanup.request.intent
                == MutationIntent::RemoveFile {
                    before: temp.clone()
                }
            && cleanup.receipt
                == Some(MutationReceipt::Removed {
                    item: temp.id.clone()
                })
            && match &cleanup.base {
                Some(base) => base.resolved && base.predecessor == temporary.id,
                None => false,
            },
        "owned DATA atomic temporary cleanup receipt changed"
    );
    for owner in [source, victim] {
        ensure!(
            owner.scope == plan.scope()
                && owner.native_archive.is_none()
                && owner.node.kind == NodeKind::File
                && !owner.node.package
                && owner.node.target.is_none()
                && owner.node.parent_id == Some(parent_object.node.id.clone()),
            "owned DATA atomic local namespace scope changed"
        );
    }
    let mut expected_original = original.clone();
    if victim
        .remote
        .as_ref()
        .is_some_and(|r| r.content_version.is_none())
        && original
            .content_version
            .as_ref()
            .is_some_and(|v| original.etag.as_ref() == Some(v))
    {
        expected_original.content_version = None;
    }
    for (before, remote, name, working) in [
        (
            &authority.source_before,
            temp,
            authority.temporary_name(plan),
            plan.working_b,
        ),
        (
            &authority.victim_before,
            &expected_original,
            plan.name(),
            Some(authority.victim_working),
        ),
    ] {
        ensure!(
            !before.unlinked
                && before.remote_owned
                && !before.follows_remote
                && before.native_archive.is_none()
                && before.remote.as_ref() == Some(remote)
                && before.working_file == working
                && before.node.kind == NodeKind::File
                && !before.node.package
                && before.node.target.is_none()
                && before.node.parent_id == Some(parent_object.node.id.clone())
                && before.node.name == name
                && before.node.size == remote.size,
            "owned DATA atomic pre-rename identity changed"
        );
    }
    ensure!(
        authority.source_before.latest == Some(temporary.id)
            && authority.source_before.remote_sequence == temporary.sequence
            && authority.victim_before.remote_sequence == created.sequence
            && authority
                .victim_before
                .latest
                .is_none_or(|id| id == created.id),
        "owned DATA atomic pre-rename generation changed"
    );
    ensure!(
        victim.unlinked
            && !victim.remote_owned
            && !victim.follows_remote
            && victim.remote.as_ref() == Some(&expected_original)
            && victim.remote_sequence == created.sequence
            && victim.node.name == plan.name()
            && victim.node.size == plan.source_a.size
            && victim.latest == authority.victim_before.latest
            && victim.node == authority.victim_before.node,
        "owned DATA atomic detached original changed"
    );
    checked_working(
        snapshot,
        victim,
        authority.victim_working,
        plan,
        &plan.source_a,
        true,
        journal,
    )?;
    ensure!(
        cleaned.scope == plan.scope()
            && cleaned.native_archive.is_none()
            && cleaned.unlinked
            && cleaned.remote_owned
            && !cleaned.follows_remote
            && cleaned.working_file.is_none()
            && cleaned.latest == Some(cleanup.id)
            && cleaned.remote.as_ref() == Some(temp)
            && cleaned.remote_sequence == temporary.sequence,
        "owned DATA atomic cleanup namespace changed"
    );
    let mut expected_cleanup = temp.clone();
    expected_cleanup.id = format!("local-{}", cleaned.id);
    ensure!(
        cleaned.node == expected_cleanup,
        "owned DATA atomic full cleanup projection changed"
    );
    let mut expected_remote = current.clone();
    if source.follows_remote
        && source
            .remote
            .as_ref()
            .is_some_and(|r| r.content_version.is_none())
        && current
            .content_version
            .as_ref()
            .is_some_and(|v| current.etag.as_ref() == Some(v))
    {
        expected_remote.content_version = None;
    }
    ensure!(
        !source.unlinked
            && source.remote_owned
            && source.remote.as_ref() == Some(&expected_remote)
            && source.remote_sequence == saved.sequence
            && source.node.name == plan.name()
            && source.node.size == plan.source_b.size,
        "owned DATA atomic published B changed"
    );
    if source.follows_remote {
        let mut expected = expected_remote;
        expected.id.clone_from(&source.node.id);
        expected.parent_id = Some(parent_object.node.id.clone());
        ensure!(
            source.latest.is_none()
                && source.working_file.is_none()
                && source.node == expected
                && snapshot.working.len() == 1,
            "owned DATA atomic retired B changed"
        );
    } else {
        let working = plan
            .working_b
            .context("owned DATA atomic B working absent")?;
        checked_working(
            snapshot,
            source,
            working,
            plan,
            &plan.source_b,
            false,
            journal,
        )?;
        ensure!(
            snapshot.working.len() == 2,
            "owned DATA atomic extra working changed"
        );
    }
    let recovery_id = saved
        .ordinary_validation_recovery_owner()
        .context("owned DATA atomic recovery owner absent")?;
    let recovery = object(snapshot, recovery_id)?;
    ensure!(
        [source.id, victim.id, cleaned.id, parent_object.id]
            .iter()
            .all(|id| *id != recovery.id),
        "owned DATA atomic recovery owner aliases another role"
    );
    let location = saved
        .reserved_recovery_location()
        .context("owned DATA atomic recovery reservation absent")?;
    ensure!(
        location
            == cirrove_icloud::ICloudFileReplace::recovery_location_for_name(
                saved.id,
                &original.name
            )
            || location == cirrove_icloud::ICloudFileReplace::recovery_location(saved.id),
        "owned DATA atomic recovery suffix changed"
    );
    let cirrove_core::upload::RecoveryLocation::Trash { local_name, .. } = location else {
        bail!("owned DATA atomic recovery route changed");
    };
    let mut expected_recovery = backup.clone();
    expected_recovery.id = format!("local-recovery-{}", recovery.id);
    expected_recovery.parent_id = Some(parent.id.clone());
    expected_recovery.name = local_name;
    ensure!(
        recovery.scope == plan.scope()
            && recovery.unlinked
            && recovery.remote_owned
            && !recovery.follows_remote
            && recovery.native_archive.is_none()
            && recovery.working_file.is_none()
            && recovery.latest.is_none()
            && recovery.remote_sequence == saved.sequence
            && recovery.remote.as_ref() == Some(backup)
            && recovery.node == expected_recovery,
        "owned DATA atomic full original Trash projection changed"
    );
    Ok((parent, current.clone(), Some(backup.clone())))
}
pub(super) fn journal_binding(
    plan: &Registration,
    journal: &RecoveryJournal,
) -> Result<(Node, Node, Option<Node>, Vec<u8>)> {
    let authority = plan
        .atomic
        .as_ref()
        .context("owned DATA atomic authority absent")?;
    let inventory = journal.ordinary_validation_atomic_inventory()?;
    let snapshot: Snapshot = serde_json::from_value(inventory)?;
    let rows = journal.list(0, 4)?;
    let mutations = journal.native_validation_mutations(0, 3)?;
    let result = admitted_atomic(plan, authority, &rows, &mutations, &snapshot, journal)?;
    let digests = snapshot
        .working
        .iter()
        .map(|w| Ok((w.id, journal.ordinary_validation_working_digest(w.id)?)))
        .collect::<Result<Vec<_>>>()?;
    let frozen = serde_json::to_vec(&serde_json::json!({"rows": rows, "mutations": mutations,
        "snapshot": snapshot, "working_digests": digests}))?;
    Ok((result.0, result.1, result.2, frozen))
}
