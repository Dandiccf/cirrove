//! Explicit one-shot auth refresh experiment; never part of normal read paths.
use super::{ICloudReadSession, OwnedReadinessObservation, account_hash};
use anyhow::{Result, anyhow, ensure};
use secrecy::ExposeSecret;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

impl ICloudReadSession {
    pub(super) fn independent_renewal_session(&self, expected_apple_id: &str) -> Result<Self> {
        let expected = account_hash(expected_apple_id)?;
        ensure!(
            self.account_hash.as_deref() == Some(expected.as_str()),
            "renewal local account binding mismatch"
        );
        let snapshot = self.session_snapshot()?;
        let session = Self::from_session_snapshot(&snapshot, expected_apple_id)?;
        ensure!(
            !Arc::ptr_eq(&self.cookies, &session.cookies),
            "renewal requires independent session storage"
        );
        Ok(session)
    }

    /// No passwords, SRP, challenge/terms completion, retries or persistence.
    /// The original jar is independent; server-side session rotation is unknown.
    pub async fn probe_owned_pages_renewal_readiness(
        &self,
        expected_apple_id: &str,
        cancel: &CancellationToken,
    ) -> Result<OwnedReadinessObservation> {
        ensure!(!cancel.is_cancelled(), "renewal request cancelled");
        let mut session = self.independent_renewal_session(expected_apple_id)?;
        self.renewal_readiness_with_session(&mut session, expected_apple_id, cancel)
            .await
    }

    pub(super) async fn renewal_readiness_with_session(
        &self,
        session: &mut Self,
        expected_apple_id: &str,
        cancel: &CancellationToken,
    ) -> Result<OwnedReadinessObservation> {
        let expected = account_hash(expected_apple_id)?;
        ensure!(
            self.account_hash.as_deref() == Some(expected.as_str())
                && session.account_hash == self.account_hash
                && !Arc::ptr_eq(&self.cookies, &session.cookies)
                && session.trusted_dsid_hash == self.trusted_dsid_hash,
            "renewal session binding mismatch"
        );
        ensure!(
            session.session_snapshot()?.expose_secret() == self.session_snapshot()?.expose_secret(),
            "renewal source snapshot binding mismatch"
        );
        let prior_anchor = self.trusted_dsid_hash.clone();
        let action = async {
            ensure!(!cancel.is_cancelled(), "renewal request cancelled");
            // Exactly one existing token-authenticated accountLogin. Failure
            // returns directly, without validate, a challenge or another login.
            session.account_login().await?;
            let new_anchor = session
                .trusted_dsid_hash
                .as_ref()
                .ok_or_else(|| anyhow!("renewal response has no stable account anchor"))?;
            ensure!(
                prior_anchor.as_ref().is_none_or(|old| old == new_anchor),
                "renewal stable account identity mismatch"
            );
            ensure!(!cancel.is_cancelled(), "renewal request cancelled");
            session
                .probe_owned_pages_readiness(expected_apple_id, cancel)
                .await
        };
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(anyhow!("renewal request cancelled")),
            result = tokio::time::timeout(Duration::from_secs(60), action) => result.map_err(|_| anyhow!("renewal request timed out"))?,
        }
    }
}
