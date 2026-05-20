mod artifact;

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::Signer;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use testquorum_api::types::ExchangeRequest;
use testquorum_api::types::InitiateRequest;

use super::Environment;
use super::client;

const EXCHANGE_PREFIX: &[u8] = b"testquorum-exchange-v1:";

/// Backoff schedule for /github/auth/exchange when the backend reports the
/// artifact isn't yet visible via GitHub's public artifacts API (409). Three
/// retries totalling 7s of wall time, picked to ride out typical propagation
/// without making the failure modes feel hung.
const EXCHANGE_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

pub(crate) struct GitHubEnvironment;

impl GitHubEnvironment {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Environment for GitHubEnvironment {
    fn name(&self) -> &'static str {
        "github"
    }

    async fn authenticated_client(&self) -> Result<Option<testquorum_api::Client>, anyhow::Error> {
        if let Ok(token) = std::env::var("TQ_AUTH_TOKEN") {
            if !token.is_empty() {
                return Ok(Some(client::with_bearer(&token)?));
            }
        }

        let api_key = run_handshake().await?;
        Ok(Some(client::with_bearer(&api_key)?))
    }
}

async fn run_handshake() -> Result<String, anyhow::Error> {
    let repository = require_env("GITHUB_REPOSITORY")?;
    let workflow_run_id: i64 = require_env("GITHUB_RUN_ID")?
        .parse()
        .map_err(|_| anyhow::anyhow!("GITHUB_RUN_ID is not a valid integer"))?;
    let run_attempt: i32 = require_env("GITHUB_RUN_ATTEMPT")?
        .parse()
        .map_err(|_| anyhow::anyhow!("GITHUB_RUN_ATTEMPT is not a valid integer"))?;
    let runtime_token = require_env("ACTIONS_RUNTIME_TOKEN")?;
    let results_url = require_env("ACTIONS_RESULTS_URL")?;

    let signing_key = SigningKey::generate(&mut OsRng);
    let public_key_b64 = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

    let api = client::unauthenticated();
    let initiate_resp = api
        .initiate_workflow_auth(&InitiateRequest {
            public_key: public_key_b64,
            repository,
            run_attempt,
            workflow_run_id,
        })
        .await
        .map_err(|e| anyhow::anyhow!("/github/auth/initiate failed: {}", e))?
        .into_inner();

    let backend_ids = artifact::backend_ids_from_token(&runtime_token)?;
    let http = reqwest::Client::new();
    artifact::upload_text_artifact(
        &http,
        &results_url,
        &runtime_token,
        &backend_ids,
        &initiate_resp.artifact_name,
        &initiate_resp.artifact_filename,
        &initiate_resp.challenge,
    )
    .await?;

    let mut signing_payload =
        Vec::with_capacity(EXCHANGE_PREFIX.len() + initiate_resp.challenge.len());
    signing_payload.extend_from_slice(EXCHANGE_PREFIX);
    signing_payload.extend_from_slice(initiate_resp.challenge.as_bytes());
    let signature = signing_key.sign(&signing_payload);
    let signature_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

    let exchange_req = ExchangeRequest {
        challenge: initiate_resp.challenge,
        signature: signature_b64,
    };
    let exchange_resp = exchange_with_retry(&api, &exchange_req).await?;

    Ok(exchange_resp.api_key)
}

async fn exchange_with_retry(
    api: &testquorum_api::Client,
    req: &ExchangeRequest,
) -> Result<testquorum_api::types::ExchangeResponse, anyhow::Error> {
    let mut attempt = 0usize;
    loop {
        match api.exchange_workflow_auth(req).await {
            Ok(resp) => return Ok(resp.into_inner()),
            Err(e) => {
                let is_409 = e.status() == Some(reqwest::StatusCode::CONFLICT);
                if is_409 && attempt < EXCHANGE_RETRY_DELAYS.len() {
                    tokio::time::sleep(EXCHANGE_RETRY_DELAYS[attempt]).await;
                    attempt += 1;
                    continue;
                }
                return Err(anyhow::anyhow!("/github/auth/exchange failed: {}", e));
            }
        }
    }
}

fn require_env(name: &str) -> Result<String, anyhow::Error> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(anyhow::anyhow!("missing {}", name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_has_expected_length() {
        let key = SigningKey::generate(&mut OsRng);
        let mut payload = EXCHANGE_PREFIX.to_vec();
        payload.extend_from_slice(b"some-challenge-value");
        let sig = key.sign(&payload);
        assert_eq!(sig.to_bytes().len(), 64);
    }

    #[test]
    fn public_key_encodes_to_43_chars_base64url() {
        // 32 bytes → 43 chars in base64url-no-pad.
        let key = SigningKey::generate(&mut OsRng);
        let encoded = URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes());
        assert_eq!(encoded.len(), 43);
        assert!(!encoded.contains('='));
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
    }
}
