//! Access-token supply shared by provider adapters; secrets never implement Debug.
use crate::ProviderError;
use async_trait::async_trait;
use secrecy::SecretString;

#[async_trait]
pub trait TokenSource: Send + Sync {
    async fn access_token(&self) -> Result<SecretString, ProviderError>;
    async fn invalidate(&self, _rejected: &SecretString) {}
}

/// For isolated developer probes and synthetic fixtures, without refresh.
pub struct StaticToken(pub SecretString);
#[async_trait]
impl TokenSource for StaticToken {
    async fn access_token(&self) -> Result<SecretString, ProviderError> {
        Ok(self.0.clone())
    }
}
