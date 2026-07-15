use std::pin::Pin;
use std::process::Stdio;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use serde::Deserialize;
use serde::Serialize;
use testquorum_api::types as api;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::Test;
use crate::TestEvent;
use crate::TestManager;
use crate::TestOutcome;

pub(crate) mod detect;
pub(crate) mod errors;

pub(crate) use detect::detect_pytest;
pub(crate) use errors::PytestError;

const MANAGER_NAME: &str = "pytest";

fn manager_identity() -> api::TestManager {
    api::TestManager::custom(MANAGER_NAME).expect("\"pytest\" is a valid custom manager name")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PytestTestPayload {
    pub(crate) config_path: String,
}

pub(crate) struct PytestManager {
    config_path: String,
}

impl PytestManager {
    pub(crate) fn new(config_path: String) -> Self {
        Self { config_path }
    }
}

#[async_trait]
impl TestManager for PytestManager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let tests = vec![Test {
            name: "pytest".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&PytestTestPayload {
                config_path: self.config_path.clone(),
            })
            .expect("PytestTestPayload serialise is infallible"),
        }];
        Ok(tests)
    }

    async fn run(&self, tests: Vec<Test>) -> Pin<Box<dyn Stream<Item = TestEvent> + Send>> {
        let (tx, rx) = mpsc::channel::<TestEvent>(1);

        tokio::spawn(async move {
            for test in tests {
                if tx
                    .send(TestEvent::Started {
                        name: test.name.clone(),
                        manager: manager_identity(),
                    })
                    .await
                    .is_err()
                {
                    return;
                }

                let outcome = match serde_json::from_value::<PytestTestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed pytest payload: {}", e),
                    },
                };

                if tx
                    .send(TestEvent::Finished {
                        name: test.name,
                        manager: manager_identity(),
                        outcome,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });

        Box::pin(ReceiverStream::new(rx))
    }
}

async fn run_one(payload: &PytestTestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, output) = pytest_run(&payload.config_path).await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr: output,
    }
}

async fn pytest_run(config_path: &str) -> (bool, String) {
    let root = std::path::Path::new(config_path)
        .parent()
        .and_then(|p| p.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(".");

    let output = Command::new("python3")
        .args(["-m", "pytest"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match output {
        Ok(output) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }
            (output.status.success(), combined)
        }
        Err(e) => (false, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pytest_test_payload_roundtrips() {
        let payload = PytestTestPayload {
            config_path: "pyproject.toml".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: PytestTestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.config_path, payload.config_path);
    }

    #[test]
    fn discover_emits_single_test() {
        let payload = PytestTestPayload {
            config_path: "pyproject.toml".to_string(),
        };
        let test = Test {
            name: "pytest".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&payload).unwrap(),
        };

        assert_eq!(test.manager.to_string(), "custom:pytest");
        assert_eq!(
            serde_json::to_value(&test.manager).unwrap(),
            serde_json::json!("custom:pytest")
        );
        let parsed: PytestTestPayload = serde_json::from_value(test.payload).unwrap();
        assert_eq!(parsed.config_path, "pyproject.toml");
    }
}
