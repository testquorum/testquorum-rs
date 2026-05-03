use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::Deserialize;
use serde::Serialize;

pub(crate) mod nix;

pub(crate) use nix::NixManager;
pub(crate) use nix::detect_nix;

/// The wire-bound description of a test. Discovered on one runner, may be
/// serialised and shipped to another runner via the API. `manager` routes the
/// `Test` to a `TestManager`; `payload` is opaque to anything that isn't the
/// matching manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Test {
    pub(crate) name: String,
    pub(crate) manager: String,
    pub(crate) payload: serde_json::Value,
}

#[derive(Debug, Clone)]
pub(crate) enum TestEvent {
    Started { name: String },
    Finished { name: String, outcome: TestOutcome },
}

#[derive(Debug, Clone)]
pub(crate) struct TestOutcome {
    pub(crate) passed: bool,
    pub(crate) duration_ms: u64,
    pub(crate) stderr: String,
}

#[async_trait]
pub(crate) trait TestManager: Send + Sync {
    fn name(&self) -> &'static str;

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error>;

    /// Runs the supplied tests, emitting a `Started` event when each test
    /// begins and a `Finished` event with its outcome when it terminates.
    async fn run(&self, tests: Vec<Test>) -> Pin<Box<dyn Stream<Item = TestEvent> + Send>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde_roundtrips_with_arbitrary_payload() {
        let test = Test {
            name: "some-test".to_string(),
            manager: "nix".to_string(),
            payload: serde_json::json!({ "anything": "the manager wants", "n": 42 }),
        };
        let json = serde_json::to_string(&test).unwrap();
        let back: Test = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, test.name);
        assert_eq!(back.manager, test.manager);
        assert_eq!(back.payload, test.payload);
    }
}
