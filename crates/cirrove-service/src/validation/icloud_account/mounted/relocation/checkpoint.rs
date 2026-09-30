//! Feature-only failpoint around the real encrypted plan vault. Never changes a
//! cloud response: process loss happens before persisting a confirmed child step.
use super::*;
use secrecy::SecretString;
use std::sync::Mutex;

pub(super) struct Watch {
    run_dir: PathBuf,
    scope: Scope,
    completed: u8,
    recovering: Option<Uuid>,
    sent: Mutex<Vec<u8>>,
}
impl Watch {
    pub fn new(f: &Fixture, completed: u8, recovering: Option<Uuid>) -> Arc<Self> {
        Arc::new(Self {
            run_dir: f.run_dir.clone(),
            scope: f.scope.clone(),
            completed,
            recovering,
            sent: Mutex::new(Vec::new()),
        })
    }
    fn check(&self, key: &str, value: &SecretString) -> Result<Option<serde_json::Value>> {
        ensure!(
            value.expose_secret().len() <= 32 * 1024,
            "oversized checkpoint"
        );
        let plan: serde_json::Value =
            serde_json::from_str(value.expose_secret()).context("invalid checkpoint")?;
        if plan.get("step").is_none() {
            return Ok(None);
        }
        let operation = Uuid::parse_str(plan["operation"].as_str().context("missing operation")?)?;
        ensure!(
            key == format!("icloud-folder-plan/{}/{operation}", self.scope.account),
            "foreign checkpoint key"
        );
        if self.recovering.is_some_and(|id| id != operation) {
            return Ok(None);
        }
        let setup: Setup = read(&self.run_dir.join("setup.json"))?;
        let request: MutationRequest = serde_json::from_value(plan["request"].clone())?;
        ensure!(
            request.scope == self.scope && expected(&request, &setup),
            "foreign checkpoint request"
        );
        let step = plan["step"].as_u64().context("missing step")?;
        if self.recovering.is_some() {
            if plan["phase"].as_str() == Some("sent") {
                let mut sent = self
                    .sent
                    .lock()
                    .map_err(|_| anyhow::anyhow!("checkpoint lock"))?;
                ensure!(
                    step == u64::from(self.completed) + sent.len() as u64 && step < 3,
                    "completed relocation step would replay"
                );
                record(
                    &self
                        .run_dir
                        .join(format!("recovery-dispatch-step-{step}.json")),
                    &serde_json::json!({"step":step,"operation":operation}),
                )?;
                sent.push(step as u8);
            }
            return Ok(None);
        }
        if step != u64::from(self.completed) || plan["phase"].as_str() != Some("ready") {
            return Ok(None);
        }
        let current: Node = serde_json::from_value(plan["current"].clone())?;
        ensure!(
            current.id == setup.original.id
                && current.kind == NodeKind::File
                && current.size == FIRST.len() as u64
                && !current.package
                && current.target.is_none()
                && current.name == format!(".cirrove-move-{operation}")
                && current.parent_id.as_ref()
                    == Some(if self.completed == 1 {
                        setup
                            .original
                            .parent_id
                            .as_ref()
                            .context("missing parent")?
                    } else {
                        &setup.destination.id
                    })
                && current.etag.as_ref().is_some_and(|s| !s.is_empty())
                && plan["original_sha256"].as_str()
                    == Some(hex::encode(Sha256::digest(FIRST)).as_str()),
            "intermediate receipt differs"
        );
        Ok(Some(plan))
    }
    pub fn verify(&self) -> Result<()> {
        let sent = self
            .sent
            .lock()
            .map_err(|_| anyhow::anyhow!("checkpoint lock"))?;
        ensure!(
            *sent == (self.completed..3).collect::<Vec<_>>(),
            "remaining dispatch sequence differs"
        );
        Ok(())
    }
}
pub(super) struct Vault {
    pub inner: SealedFolderCheckpointVault,
    pub watch: Arc<Watch>,
}
#[async_trait::async_trait]
impl CredentialVault for Vault {
    async fn load(&self, key: &str) -> Result<Option<SecretString>> {
        self.inner.load(key).await
    }
    async fn remove(&self, key: &str) -> Result<()> {
        self.inner.remove(key).await
    }
    async fn save(&self, key: &str, value: SecretString) -> Result<()> {
        if let Some(plan) = self.watch.check(key, &value)? {
            let old = self
                .inner
                .load(key)
                .await?
                .context("missing prior checkpoint")?;
            let old: serde_json::Value =
                serde_json::from_str(old.expose_secret()).context("invalid prior checkpoint")?;
            ensure!(
                old["phase"].as_str() == Some("sent")
                    && old["step"].as_u64() == Some(u64::from(self.watch.completed - 1))
                    && old["operation"] == plan["operation"]
                    && old["request"] == plan["request"],
                "prior checkpoint is not the dispatched step"
            );
            record(
                &self.watch.run_dir.join("interrupted.json"),
                &serde_json::json!({
                    "operation":plan["operation"],"pid":std::process::id(),
                    "acknowledgement_not_returned":true,"boundary":"combined_step_before_checkpoint",
                    "completed_step":self.watch.completed,"receipt":plan["current"]
                }),
            )?;
            std::fs::File::open(&self.watch.run_dir)?.sync_all()?;
            std::process::exit(86);
        }
        self.inner.save(key, value).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn plan(request: &MutationRequest, operation: Uuid, step: u8, phase: &str) -> SecretString {
        let mut node = request.intent.before().unwrap().clone();
        node.name = format!(".cirrove-move-{operation}");
        if step >= 2 {
            node.parent_id = Some("destination".into());
        }
        node.etag = Some("confirmed-child".into());
        SecretString::from(
            serde_json::json!({"operation":operation,"request":request,
            "step":step,"phase":phase,"current":node,
            "original_sha256":hex::encode(Sha256::digest(FIRST))})
            .to_string(),
        )
    }
    #[test]
    fn intermediate_boundary_requires_owned_request_and_exact_step() {
        for step in [1, 2] {
            let tmp = tempfile::tempdir().unwrap();
            let (_, r) = super::super::tests::fixture(tmp.path());
            let id = Uuid::new_v4();
            let key = format!("icloud-folder-plan/{}/{id}", r.scope.account);
            let watch = Watch {
                run_dir: tmp.path().into(),
                scope: r.scope.clone(),
                completed: step,
                recovering: None,
                sent: Mutex::new(Vec::new()),
            };
            assert!(
                watch
                    .check(&key, &plan(&r, id, step, "ready"))
                    .unwrap()
                    .is_some()
            );
            assert!(
                watch
                    .check(&key, &plan(&r, id, step, "sent"))
                    .unwrap()
                    .is_none()
            );
            assert!(
                watch
                    .check("foreign-key", &plan(&r, id, step, "ready"))
                    .is_err()
            );
            let mut foreign = r.clone();
            foreign.scope.account = "other".into();
            assert!(
                watch
                    .check(&key, &plan(&foreign, id, step, "ready"))
                    .is_err()
            );
            let mut bad: serde_json::Value =
                serde_json::from_str(plan(&r, id, step, "ready").expose_secret()).unwrap();
            bad["current"]["id"] = "other".into();
            assert!(
                watch
                    .check(&key, &SecretString::from(bad.to_string()))
                    .is_err()
            );
            assert!(!tmp.path().join("interrupted.json").exists());
        }
    }
    #[test]
    fn recovery_only_allows_remaining_steps_once_in_order() {
        for completed in [1, 2] {
            let tmp = tempfile::tempdir().unwrap();
            let (_, r) = super::super::tests::fixture(tmp.path());
            let id = Uuid::new_v4();
            let key = format!("icloud-folder-plan/{}/{id}", r.scope.account);
            let watch = Watch {
                run_dir: tmp.path().into(),
                scope: r.scope.clone(),
                completed,
                recovering: Some(id),
                sent: Mutex::new(Vec::new()),
            };
            assert!(watch.verify().is_err());
            assert!(
                watch
                    .check(&key, &plan(&r, id, completed - 1, "sent"))
                    .is_err()
            );
            assert!(watch.check(&key, &plan(&r, id, 3, "sent")).is_err());
            for step in completed..3 {
                assert!(
                    watch
                        .check(&key, &plan(&r, id, step, "ready"))
                        .unwrap()
                        .is_none()
                );
                assert!(
                    watch
                        .check(&key, &plan(&r, id, step, "sent"))
                        .unwrap()
                        .is_none()
                );
                assert!(watch.check(&key, &plan(&r, id, step, "sent")).is_err());
            }
            watch.verify().unwrap();
        }
    }
}
