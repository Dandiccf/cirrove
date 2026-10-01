//! One exact retained conflict. Never advances the write state machine.
use super::*;
const OPERATION: &str = "82767008-4553-4f8b-99cc-8a521ad9c5ac";
fn exact_attempt(run: Uuid, operation: Uuid) -> Result<()> {
    ensure!(
        run.to_string() == RUN && operation.to_string() == OPERATION,
        "diagnostic is not the preregistered owned operation"
    );
    Ok(())
}
pub async fn icloud_public_native_replacement_diagnose(run: Uuid, operation: Uuid) -> Result<()> {
    exact_attempt(run, operation)?;
    let dir = bound_run(run)?;
    let attempt = verification_directory(dir)?;
    manifest(&attempt, run, "replacement-conflict-diagnostic-read-only")?;
    let binding =
        retained_public_import_for_replacement(Uuid::parse_str(IMPORT_RUN)?, Some(operation))?;
    let proof: Preflight = read_json(&dir.join("preflight.json"), 64 * 1024)?;
    let proof = preflight_binding(&proof, &binding, dir)?;
    let row = binding.journal.native_validation_upload(operation)?;
    prepared_replacement_binding(&row, &proof, operation)?;
    ensure!(
        row.state == UploadState::Conflict
            && row.remote.is_none()
            && row.package_completion.is_none()
            && row.session_key == Some(operation),
        "diagnostic retained conflict changed"
    );
    let state = Path::new(PUBLIC).join("state");
    let vault = cirrove_icloud::SealedUploadCheckpointVault::new(&state, &proof.account)
        .map_err(|_| anyhow::anyhow!("diagnostic checkpoint vault unavailable"))?;
    let checkpoint = vault
        .load(&format!("upload/{operation}"))
        .await
        .map_err(|_| anyhow::anyhow!("diagnostic checkpoint could not be read"))?
        .context("diagnostic checkpoint absent")?;
    let request = cirrove_core::upload::UploadRequest {
        scope: row.scope.clone(),
        intent: row.intent.clone(),
        representation: row.representation.clone(),
        size: row.size,
        sha256: row.sha256.clone(),
    };
    let provider =
        cirrove_icloud::ICloudFileReplace::restore_native_package_from_sealed_checkpoint(
            request.clone(),
            operation,
            cirrove_icloud::ICloudSealedSignIn {
                apple_id: binding.account.identity.username.clone(),
                credential_id: binding.account.credential_id.clone(),
            },
            &state,
            &attempt,
            &checkpoint,
        )
        .map_err(|_| anyhow::anyhow!("diagnostic checkpoint identity refused"))?;
    let summary = provider
        .native_checkpoint_diagnostic(&operation.to_string(), &request, &checkpoint)
        .map_err(|_| anyhow::anyhow!("diagnostic checkpoint binding refused"))?;
    record(
        &attempt.join("checkpoint-summary.json"),
        &serde_json::json!({"run":run,"operation":operation,"account":proof.account,"checkpoint":summary,"cloud_mutated":false,"phase_does_not_prove_dispatch":true}),
    )?;
    File::open(&attempt)?.sync_all()?;
    let locations = provider
        .native_checkpoint_locations_read_only(
            &operation.to_string(),
            &request,
            &checkpoint,
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(
            |_| serde_json::json!({"category":"metadata-unavailable-no-location-conclusion"}),
        );
    record(&attempt.join("owned-location-summary.json"), &locations)?;
    File::open(&attempt)?.sync_all()?;
    owned_directory(proof.archive.parent().context("snapshot parent missing")?)?;
    let source = private_file(&proof.archive, LIMIT)?;
    ensure!(
        source.metadata()?.uid() == std::fs::metadata("/proc/self")?.uid(),
        "snapshot owner refused"
    );
    let stage = provider
        .inspect_native_stage_read_only(
            &operation.to_string(),
            &request,
            &checkpoint,
            source,
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|_| serde_json::json!({"category":"stage-diagnostic-unavailable"}));
    record(&attempt.join("stage-verification-summary.json"), &stage)?;
    File::open(&attempt)?.sync_all()?;
    // Only native_observe is reachable; no UploadProvider state-machine method.
    let observation = match provider
        .inspect_native_checkpoint_read_only(
            &operation.to_string(),
            &request,
            &checkpoint,
            &CancellationToken::new(),
        )
        .await
    {
        Ok(category) => category,
        Err(cirrove_core::upload::UploadError::Conflict) => {
            "inspection-conflict-no-location-conclusion"
        }
        Err(_) => "inspection-unavailable-no-location-conclusion",
    };
    record(
        &attempt.join("inspection-summary.json"),
        &serde_json::json!({"run":run,"operation":operation,"observation":observation,"cloud_mutated":false,"journal_changed":false,"checkpoint_changed":false}),
    )?;
    File::open(&attempt)?.sync_all()?;
    println!("Owned replacement read-only diagnostic retained: {observation}.");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn diagnostic_wrong_run_or_operation_refuses_before_local_or_remote_access() {
        for (run, operation) in [
            (Uuid::nil(), Uuid::nil()),
            (Uuid::parse_str(RUN).expect("run"), Uuid::nil()),
        ] {
            assert!(
                icloud_public_native_replacement_diagnose(run, operation)
                    .await
                    .is_err()
            );
        }
    }
}
