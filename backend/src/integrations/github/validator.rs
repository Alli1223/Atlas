//! The credential [`Validator`] for `Provider::Github`.
//!
//! Routed to from [`crate::secrets::vault::default_validator`]. It builds a
//! throwaway [`GithubClient`] for the secret and probes `GET /user`; all the logic
//! — the 200/401/403 classification, the scope and expiry header parsing — lives in
//! [`crate::integrations::github::client`] as pure functions and is tested there.

use std::pin::Pin;

use crate::error::AppResult;
use crate::secrets::Secret;
use crate::secrets::vault::{ValidationOutcome, Validator};

use super::client::GithubClient;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Validates a GitHub PAT by probing `GET /user`.
///
/// Holds an optional base-URL override so the probe target is never a literal here;
/// production leaves it `None` and the client uses `api.github.com`.
#[derive(Debug, Default)]
pub struct GithubValidator {
    /// A base-URL override. `None` = `api.github.com`.
    base_url: Option<String>,
}

impl GithubValidator {
    /// The production validator, probing `api.github.com`.
    #[must_use]
    pub fn new() -> Self {
        Self { base_url: None }
    }
}

impl Validator for GithubValidator {
    fn validate<'a>(
        &'a self,
        secret: &'a Secret<String>,
    ) -> BoxFuture<'a, AppResult<ValidationOutcome>> {
        Box::pin(async move {
            let client = match &self.base_url {
                Some(base) => GithubClient::with_base_url(secret.clone(), base.clone())?,
                None => GithubClient::new(secret.clone())?,
            };
            client.validate().await
        })
    }
}
