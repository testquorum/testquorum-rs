mod artifact;

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::Signer;
use ed25519_dalek::SigningKey;
use git2::Oid;
use testquorum_api::types::Commit;
use testquorum_api::types::ExchangeRequest;
use testquorum_api::types::InitiateRequest;
use testquorum_api::types::Run;
use testquorum_api::types::RunKind;
use testquorum_api::types::Trigger;
use tracing::warn;

use super::Environment;
use super::RunContext;
use super::client;
use super::git;

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

    async fn run_context(&self) -> Result<Option<RunContext>, anyhow::Error> {
        let repo_source_id = match std::env::var("GITHUB_REPOSITORY_ID") {
            Ok(v) if !v.is_empty() => v,
            _ => {
                warn!("upload skipped: GITHUB_REPOSITORY_ID is not set");
                return Ok(None);
            }
        };
        let head_sha = match std::env::var("GITHUB_SHA") {
            Ok(v) if !v.is_empty() => v,
            _ => {
                warn!("upload skipped: GITHUB_SHA is not set");
                return Ok(None);
            }
        };
        let event_name = std::env::var("GITHUB_EVENT_NAME").unwrap_or_default();
        let base_ref = std::env::var("GITHUB_BASE_REF").unwrap_or_default();
        let event_path = std::env::var("GITHUB_EVENT_PATH").unwrap_or_default();
        let ref_name = std::env::var("GITHUB_REF_NAME").unwrap_or_default();
        let ref_type = std::env::var("GITHUB_REF_TYPE").unwrap_or_default();

        // All git2 work happens off the async runtime — libgit2 is blocking.
        let run = tokio::task::spawn_blocking(move || {
            build_run(
                &head_sha,
                &event_name,
                &base_ref,
                &event_path,
                &ref_name,
                &ref_type,
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("git task panicked: {}", e))?;
        let run = match run {
            BuildRunOutcome::Ok(run) => run,
            BuildRunOutcome::Skip(reason) => {
                warn!("upload skipped: {}", reason);
                return Ok(None);
            }
        };

        Ok(Some(RunContext {
            repo_id: format!("github:{}", repo_source_id),
            run,
        }))
    }
}

enum BuildRunOutcome {
    Ok(Run),
    Skip(String),
}

/// Minimal typed view of the `merge_group` event payload. GitHub sends many
/// more fields; serde silently drops what we don't ask for.
#[derive(serde::Deserialize)]
struct MergeGroupEvent {
    merge_group: MergeGroupPayload,
}

#[derive(serde::Deserialize)]
struct MergeGroupPayload {
    base_sha: String,
}

/// Minimal typed view of the `push` event payload — we only need the
/// repository's default branch to tell a land (push to default) apart from a
/// push to some other branch (treated as a diff).
#[derive(serde::Deserialize)]
struct PushEvent {
    repository: PushRepository,
}

#[derive(serde::Deserialize)]
struct PushRepository {
    default_branch: String,
}

fn build_run(
    head_sha: &str,
    event_name: &str,
    base_ref: &str,
    event_path: &str,
    ref_name: &str,
    ref_type: &str,
) -> BuildRunOutcome {
    let repo = match git::open() {
        Ok(r) => r,
        Err(e) => return BuildRunOutcome::Skip(format!("could not open repo: {}", e.message())),
    };

    let head_oid = match git::resolve_oid(&repo, head_sha) {
        Ok(o) => o,
        Err(e) => {
            return BuildRunOutcome::Skip(format!(
                "head sha {} not present locally (shallow clone?): {}",
                head_sha,
                e.message()
            ));
        }
    };
    let head_height = match git::rev_count(&repo, head_oid) {
        Ok(h) => h,
        Err(e) => {
            return BuildRunOutcome::Skip(format!(
                "could not walk history from {}: {}",
                head_sha,
                e.message()
            ));
        }
    };
    let head = Commit {
        sha: head_sha.to_string(),
        height: head_height,
    };

    enum KindTag {
        Diff,
        Land,
    }

    let (tag, base_oid): (KindTag, Oid) = match event_name {
        "pull_request" | "pull_request_target" => {
            if base_ref.is_empty() {
                return BuildRunOutcome::Skip(
                    "GITHUB_BASE_REF is empty on a pull_request event".to_string(),
                );
            }
            match diff_base(&repo, head_oid, base_ref) {
                Ok(base) => (KindTag::Diff, base),
                Err(reason) => return BuildRunOutcome::Skip(reason),
            }
        }
        // A push to the default branch is a land (post-land check). A push to any
        // other branch is treated as a diff against the default branch. Tag
        // pushes aren't supported yet.
        "push" => {
            if ref_type == "tag" {
                return BuildRunOutcome::Skip(
                    "tag pushes are not supported for upload".to_string(),
                );
            }
            if ref_name.is_empty() {
                return BuildRunOutcome::Skip(
                    "GITHUB_REF_NAME is empty on a push event".to_string(),
                );
            }
            if event_path.is_empty() {
                return BuildRunOutcome::Skip(
                    "GITHUB_EVENT_PATH is not set on a push event".to_string(),
                );
            }
            let payload = match std::fs::read_to_string(event_path) {
                Ok(s) => s,
                Err(e) => {
                    return BuildRunOutcome::Skip(format!(
                        "could not read GITHUB_EVENT_PATH: {}",
                        e
                    ));
                }
            };
            let event: PushEvent = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(e) => {
                    return BuildRunOutcome::Skip(format!(
                        "could not parse push event payload: {}",
                        e
                    ));
                }
            };
            if ref_name == event.repository.default_branch {
                return BuildRunOutcome::Ok(Run {
                    head,
                    kind: RunKind::PostLand {
                        branch: ref_name.to_string(),
                        trigger: Trigger::Push,
                    },
                });
            }
            match diff_base(&repo, head_oid, &event.repository.default_branch) {
                Ok(base) => (KindTag::Diff, base),
                Err(reason) => return BuildRunOutcome::Skip(reason),
            }
        }
        // schedule (nightly) and workflow_dispatch (manual) are post-land checks
        // against the branch tip at the time the job ran — no merge-base.
        "schedule" | "workflow_dispatch" => {
            if ref_name.is_empty() {
                return BuildRunOutcome::Skip(format!(
                    "GITHUB_REF_NAME is empty on a {} event",
                    event_name
                ));
            }
            let trigger = if event_name == "schedule" {
                Trigger::Scheduled
            } else {
                Trigger::Manual
            };
            return BuildRunOutcome::Ok(Run {
                head,
                kind: RunKind::PostLand {
                    branch: ref_name.to_string(),
                    trigger,
                },
            });
        }
        // merge_group.base_sha is "the SHA of the merge group's parent
        // commit" — the destination tip when this group is first in the
        // queue, otherwise the previous group's tip. Either way it's the
        // right merge_base for Land (the commit this batch was built on).
        // The queue head's git first-parent isn't a substitute: under the
        // rebase strategy a multi-commit batch's first parent is inside
        // the batch, not at the group boundary.
        "merge_group" => {
            if event_path.is_empty() {
                return BuildRunOutcome::Skip(
                    "GITHUB_EVENT_PATH is not set on a merge_group event".to_string(),
                );
            }
            let payload = match std::fs::read_to_string(event_path) {
                Ok(s) => s,
                Err(e) => {
                    return BuildRunOutcome::Skip(format!(
                        "could not read GITHUB_EVENT_PATH: {}",
                        e
                    ));
                }
            };
            let event: MergeGroupEvent = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(e) => {
                    return BuildRunOutcome::Skip(format!(
                        "could not parse merge_group event payload: {}",
                        e
                    ));
                }
            };
            let base = match git::resolve_oid(&repo, &event.merge_group.base_sha) {
                Ok(o) => o,
                Err(e) => {
                    return BuildRunOutcome::Skip(format!(
                        "merge_group base sha {} not present locally (shallow clone?): {}",
                        event.merge_group.base_sha,
                        e.message()
                    ));
                }
            };
            (KindTag::Land, base)
        }
        other => {
            return BuildRunOutcome::Skip(format!(
                "GITHUB_EVENT_NAME={} is not supported for upload",
                other
            ));
        }
    };

    let base_height = match git::rev_count(&repo, base_oid) {
        Ok(h) => h,
        Err(e) => {
            return BuildRunOutcome::Skip(format!(
                "could not walk history from merge base: {}",
                e.message()
            ));
        }
    };
    let merge_base = Commit {
        sha: base_oid.to_string(),
        height: base_height,
    };

    let kind = match tag {
        KindTag::Diff => RunKind::Diff { merge_base },
        KindTag::Land => RunKind::Land { merge_base },
    };

    BuildRunOutcome::Ok(Run { head, kind })
}

/// Merge-base of `head_oid` against `refs/remotes/origin/<branch>`, for building
/// a Diff run. Returns a human-readable skip reason on any failure (commonly a
/// too-shallow fetch that left the target ref or merge-base unreachable).
fn diff_base(repo: &git2::Repository, head_oid: Oid, branch: &str) -> Result<Oid, String> {
    let target = format!("refs/remotes/origin/{}", branch);
    let target_oid = repo.refname_to_id(&target).map_err(|e| {
        format!(
            "could not resolve {} (fetch-depth too shallow?): {}",
            target,
            e.message()
        )
    })?;
    git::merge_base(repo, head_oid, target_oid)
        .map_err(|e| format!("could not compute merge-base: {}", e.message()))
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

    let signing_key = SigningKey::from_bytes(&rand::random());
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
        let key = SigningKey::from_bytes(&rand::random());
        let mut payload = EXCHANGE_PREFIX.to_vec();
        payload.extend_from_slice(b"some-challenge-value");
        let sig = key.sign(&payload);
        assert_eq!(sig.to_bytes().len(), 64);
    }

    #[test]
    fn public_key_encodes_to_43_chars_base64url() {
        // 32 bytes → 43 chars in base64url-no-pad.
        let key = SigningKey::from_bytes(&rand::random());
        let encoded = URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes());
        assert_eq!(encoded.len(), 43);
        assert!(!encoded.contains('='));
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
    }
}
