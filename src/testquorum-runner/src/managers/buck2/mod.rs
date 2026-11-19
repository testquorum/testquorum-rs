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

pub(crate) use detect::detect_buck2;
pub(crate) use errors::Buck2Error;

const MANAGER_NAME: &str = "buck2";

fn manager_identity() -> api::TestManager {
    api::TestManager::custom(MANAGER_NAME).expect("\"buck2\" is a valid custom manager name")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Buck2TestPayload {
    pub(crate) target: String,
}

pub(crate) struct Buck2Manager {
    target: String,
}

impl Buck2Manager {
    pub(crate) fn new(target: String) -> Self {
        Self { target }
    }
}

#[async_trait]
impl TestManager for Buck2Manager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let tests = vec![Test {
            name: format!("buck2 test {}", self.target),
            manager: manager_identity(),
            payload: serde_json::to_value(&Buck2TestPayload {
                target: self.target.clone(),
            })
            .expect("Buck2TestPayload serialise is infallible"),
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

                let outcome = match serde_json::from_value::<Buck2TestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed buck2 payload: {}", e),
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

async fn run_one(payload: &Buck2TestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, output) = buck2_test(&payload.target).await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr: output,
    }
}

async fn buck2_test(target: &str) -> (bool, String) {
    let output = Command::new("buck2")
        .args(["test", target])
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
    fn buck2_test_payload_roundtrips() {
        let payload = Buck2TestPayload {
            target: "//...".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: Buck2TestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.target, payload.target);
    }

    #[test]
    fn discover_emits_single_test() {
        let payload = Buck2TestPayload {
            target: "//...".to_string(),
        };
        let test = Test {
            name: "buck2 test //...".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&payload).unwrap(),
        };

        assert_eq!(test.manager.to_string(), "custom:buck2");
        assert_eq!(
            serde_json::to_value(&test.manager).unwrap(),
            serde_json::json!("custom:buck2")
        );
        let parsed: Buck2TestPayload = serde_json::from_value(test.payload).unwrap();
        assert_eq!(parsed.target, "//...");
    }
}
