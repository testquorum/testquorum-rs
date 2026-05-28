use async_trait::async_trait;
use testquorum_api::types::Commit;
use testquorum_api::types::Run;
use testquorum_api::types::RunKind;

use super::Environment;
use super::RunContext;
use super::client;

pub(crate) struct UnknownEnvironment;

impl UnknownEnvironment {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Environment for UnknownEnvironment {
    fn name(&self) -> &'static str {
        "unknown"
    }

    async fn authenticated_client(&self) -> Result<Option<testquorum_api::Client>, anyhow::Error> {
        let token = match std::env::var("TQ_AUTH_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => return Ok(None),
        };
        Ok(Some(client::with_bearer(&token)?))
    }

    async fn run_context(&self) -> Result<Option<RunContext>, anyhow::Error> {
        match read_env() {
            EnvRead::Missing => Ok(None),
            EnvRead::Present(ctx) => Ok(Some(ctx?)),
        }
    }
}

enum EnvRead {
    Missing,
    Present(Result<RunContext, anyhow::Error>),
}

fn read_env() -> EnvRead {
    let repo_id = match non_empty("TQ_REPO_ID") {
        Some(v) => v,
        None => return EnvRead::Missing,
    };
    EnvRead::Present(build_context(repo_id))
}

fn build_context(repo_id: String) -> Result<RunContext, anyhow::Error> {
    let kind_str = require("TQ_RUN_KIND")?;
    let head = Commit {
        sha: require("TQ_HEAD_SHA")?,
        height: require_i64("TQ_HEAD_HEIGHT")?,
    };
    let merge_base = Commit {
        sha: require("TQ_MERGE_BASE_SHA")?,
        height: require_i64("TQ_MERGE_BASE_HEIGHT")?,
    };
    let kind = match kind_str.as_str() {
        "diff" => RunKind::Diff(merge_base),
        "land" => RunKind::Land(merge_base),
        "merge" => RunKind::Merge(merge_base),
        other => {
            anyhow::bail!(
                "TQ_RUN_KIND must be one of diff|land|merge, got {:?}",
                other
            );
        }
    };
    Ok(RunContext {
        repo_id,
        run: Run { head, kind },
    })
}

fn non_empty(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

fn require(key: &str) -> Result<String, anyhow::Error> {
    non_empty(key).ok_or_else(|| anyhow::anyhow!("{} must be set when TQ_REPO_ID is set", key))
}

fn require_i64(key: &str) -> Result<i64, anyhow::Error> {
    require(key)?
        .parse::<i64>()
        .map_err(|e| anyhow::anyhow!("{} is not a valid i64: {}", key, e))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // Like environments::tests::ENV_LOCK — these tests mutate the process env.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_envs<F: FnOnce()>(pairs: &[(&str, Option<&str>)], f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(String, Option<String>)> = pairs
            .iter()
            .map(|(k, _)| (k.to_string(), std::env::var(*k).ok()))
            .collect();
        for (k, v) in pairs {
            match v {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
        f();
        for (k, v) in saved {
            match v {
                Some(v) => unsafe { std::env::set_var(&k, v) },
                None => unsafe { std::env::remove_var(&k) },
            }
        }
    }

    #[test]
    fn missing_repo_id_yields_none() {
        with_envs(&[("TQ_REPO_ID", None)], || {
            assert!(matches!(read_env(), EnvRead::Missing));
        });
    }

    #[test]
    fn full_context_parses() {
        with_envs(
            &[
                ("TQ_REPO_ID", Some("github:12345")),
                ("TQ_RUN_KIND", Some("diff")),
                ("TQ_HEAD_SHA", Some("aaaa")),
                ("TQ_HEAD_HEIGHT", Some("42")),
                ("TQ_MERGE_BASE_SHA", Some("bbbb")),
                ("TQ_MERGE_BASE_HEIGHT", Some("40")),
            ],
            || {
                let ctx = match read_env() {
                    EnvRead::Present(r) => r.expect("should parse"),
                    EnvRead::Missing => panic!("expected Present"),
                };
                assert_eq!(ctx.repo_id, "github:12345");
                assert_eq!(ctx.run.head.sha, "aaaa");
                assert_eq!(ctx.run.head.height, 42);
                match ctx.run.kind {
                    RunKind::Diff(base) => {
                        assert_eq!(base.sha, "bbbb");
                        assert_eq!(base.height, 40);
                    }
                    _ => panic!("expected Diff"),
                }
            },
        );
    }

    #[test]
    fn malformed_height_is_error() {
        with_envs(
            &[
                ("TQ_REPO_ID", Some("github:1")),
                ("TQ_RUN_KIND", Some("diff")),
                ("TQ_HEAD_SHA", Some("x")),
                ("TQ_HEAD_HEIGHT", Some("not-a-number")),
                ("TQ_MERGE_BASE_SHA", Some("y")),
                ("TQ_MERGE_BASE_HEIGHT", Some("1")),
            ],
            || {
                let r = match read_env() {
                    EnvRead::Present(r) => r,
                    EnvRead::Missing => panic!("expected Present"),
                };
                assert!(r.is_err());
            },
        );
    }

    #[test]
    fn missing_kind_when_repo_id_set_is_error() {
        with_envs(
            &[
                ("TQ_REPO_ID", Some("github:1")),
                ("TQ_RUN_KIND", None),
                ("TQ_HEAD_SHA", Some("x")),
                ("TQ_HEAD_HEIGHT", Some("1")),
                ("TQ_MERGE_BASE_SHA", Some("y")),
                ("TQ_MERGE_BASE_HEIGHT", Some("0")),
            ],
            || {
                let r = match read_env() {
                    EnvRead::Present(r) => r,
                    EnvRead::Missing => panic!("expected Present"),
                };
                assert!(r.is_err());
            },
        );
    }

    #[test]
    fn unknown_kind_is_error() {
        with_envs(
            &[
                ("TQ_REPO_ID", Some("github:1")),
                ("TQ_RUN_KIND", Some("rebase")),
                ("TQ_HEAD_SHA", Some("x")),
                ("TQ_HEAD_HEIGHT", Some("1")),
                ("TQ_MERGE_BASE_SHA", Some("y")),
                ("TQ_MERGE_BASE_HEIGHT", Some("0")),
            ],
            || {
                let r = match read_env() {
                    EnvRead::Present(r) => r,
                    EnvRead::Missing => panic!("expected Present"),
                };
                assert!(r.is_err());
            },
        );
    }
}
