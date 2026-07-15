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

pub(crate) use detect::detect_sbt;
pub(crate) use errors::SbtError;

const MANAGER_NAME: &str = "sbt";

fn manager_identity() -> api::TestManager {
    api::TestManager::custom(MANAGER_NAME).expect("\"sbt\" is a valid custom manager name")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SbtTestPayload {
    pub(crate) build_sbt_path: String,
}

pub(crate) struct SbtManager {
    build_sbt_path: String,
}

impl SbtManager {
    pub(crate) fn new(build_sbt_path: String) -> Self {
        Self { build_sbt_path }
    }
}

#[async_trait]
impl TestManager for SbtManager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let tests = vec![Test {
            name: "sbt test".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&SbtTestPayload {
                build_sbt_path: self.build_sbt_path.clone(),
            })
            .expect("SbtTestPayload serialise is infallible"),
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

                let outcome = match serde_json::from_value::<SbtTestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed sbt payload: {}", e),
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

async fn run_one(payload: &SbtTestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, output) = sbt_test(&payload.build_sbt_path).await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr: output,
    }
}

async fn sbt_test(build_sbt_path: &str) -> (bool, String) {
    let dir = std::path::Path::new(build_sbt_path)
        .parent()
        .and_then(|p| p.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(".");

    let output = Command::new("sbt")
        .arg("test")
        .current_dir(dir)
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
    fn sbt_test_payload_roundtrips() {
        let payload = SbtTestPayload {
            build_sbt_path: "build.sbt".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: SbtTestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.build_sbt_path, payload.build_sbt_path);
    }

    #[test]
    fn discover_emits_single_test() {
        let payload = SbtTestPayload {
            build_sbt_path: "build.sbt".to_string(),
        };
        let test = Test {
            name: "sbt test".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&payload).unwrap(),
        };

        assert_eq!(test.manager.to_string(), "custom:sbt");
        assert_eq!(
            serde_json::to_value(&test.manager).unwrap(),
            serde_json::json!("custom:sbt")
        );
        let parsed: SbtTestPayload = serde_json::from_value(test.payload).unwrap();
        assert_eq!(parsed.build_sbt_path, "build.sbt");
    }
}
