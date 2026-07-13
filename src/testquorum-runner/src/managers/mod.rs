use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::Deserialize;
use serde::Serialize;
use testquorum_api::types as api;

pub(crate) mod cargo;
pub(crate) mod composer;
pub(crate) mod nix;
pub(crate) mod npm;
pub(crate) mod treefmt;

pub(crate) use cargo::CargoManager;
pub(crate) use cargo::detect_cargo;
pub(crate) use composer::ComposerManager;
pub(crate) use composer::detect_composer;
pub(crate) use nix::NixManager;
pub(crate) use nix::detect_nix;
pub(crate) use npm::NpmManager;
pub(crate) use npm::detect_npm;
pub(crate) use treefmt::TreefmtManager;
pub(crate) use treefmt::detect_treefmt;

/// The wire-bound description of a test. Discovered on one runner, may be
/// serialised and shipped to another runner via the API. `manager` is the
/// manager's wire identity; it routes the `Test` back to a [`TestManager`] and
/// tags every result the test produces. `payload` is opaque to anything that
/// isn't the matching manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Test {
    pub(crate) name: String,
    pub(crate) manager: api::TestManager,
    pub(crate) payload: serde_json::Value,
}

#[derive(Debug, Clone)]
pub(crate) enum TestEvent {
    Discovered {
        name: String,
        manager: api::TestManager,
    },
    Started {
        name: String,
        manager: api::TestManager,
    },
    Finished {
        name: String,
        manager: api::TestManager,
        outcome: TestOutcome,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct TestOutcome {
    pub(crate) passed: bool,
    pub(crate) duration_ms: u64,
    pub(crate) stderr: String,
}

#[async_trait]
pub(crate) trait TestManager: Send + Sync {
    /// This manager's wire identity: a built-in [`api::WellKnownTestManager`]
    /// or a `custom:<name>` runner. Used both to tag the results it produces
    /// and to route ranked tests back to it.
    fn identity(&self) -> api::TestManager;

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
            manager: api::WellKnownTestManager::Nix.into(),
            payload: serde_json::json!({ "anything": "the manager wants", "n": 42 }),
        };
        let json = serde_json::to_string(&test).unwrap();
        let back: Test = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, test.name);
        assert_eq!(back.manager, test.manager);
        assert_eq!(back.payload, test.payload);
    }
}
