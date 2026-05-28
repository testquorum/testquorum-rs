use async_trait::async_trait;

mod client;
mod git;
mod github;
mod unknown;

pub(crate) use github::GitHubEnvironment;
pub(crate) use unknown::UnknownEnvironment;

/// The repo and `Run` an invocation should be reported under. `None` from
/// `Environment::run_context` means upload is skipped (unsupported event,
/// missing config); `Err` means the environment was opted in but the inputs
/// are malformed.
pub(crate) struct RunContext {
    pub(crate) repo_id: String,
    pub(crate) run: testquorum_api::types::Run,
}

#[async_trait]
pub(crate) trait Environment: Send + Sync {
    fn name(&self) -> &'static str;

    async fn authenticated_client(&self) -> Result<Option<testquorum_api::Client>, anyhow::Error>;

    async fn run_context(&self) -> Result<Option<RunContext>, anyhow::Error>;
}

pub(crate) fn detect_environment() -> Box<dyn Environment> {
    if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
        Box::new(GitHubEnvironment::new())
    } else {
        Box::new(UnknownEnvironment::new())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // Tests in this module all read/write the same process env var, so cargo's
    // default parallel runner races them against each other. A shared mutex
    // serialises them; PoisonError is unwrapped because a panic in one test
    // shouldn't silently let the next observe a half-restored env.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(key);
        match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        f();
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn detects_github_when_actions_env_set() {
        with_env("GITHUB_ACTIONS", Some("true"), || {
            assert_eq!(detect_environment().name(), "github");
        });
    }

    #[test]
    fn detects_unknown_when_actions_env_unset() {
        with_env("GITHUB_ACTIONS", None, || {
            assert_eq!(detect_environment().name(), "unknown");
        });
    }

    #[test]
    fn detects_unknown_when_actions_env_not_true() {
        with_env("GITHUB_ACTIONS", Some("false"), || {
            assert_eq!(detect_environment().name(), "unknown");
        });
    }
}
