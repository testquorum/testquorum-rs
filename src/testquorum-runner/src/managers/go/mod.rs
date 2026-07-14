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

pub(crate) use detect::detect_go;
pub(crate) use errors::GoError;

const MANAGER_NAME: &str = "go";

fn manager_identity() -> api::TestManager {
    api::TestManager::custom(MANAGER_NAME).expect("\"go\" is a valid custom manager name")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GoTestPayload {
    pub(crate) go_mod_path: String,
}

pub(crate) struct GoManager {
    go_mod_path: String,
}

impl GoManager {
    pub(crate) fn new(go_mod_path: String) -> Self {
        Self { go_mod_path }
    }
}

#[async_trait]
impl TestManager for GoManager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let tests = vec![Test {
            name: "go test".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&GoTestPayload {
                go_mod_path: self.go_mod_path.clone(),
            })
            .expect("GoTestPayload serialise is infallible"),
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

                let outcome = match serde_json::from_value::<GoTestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed go payload: {}", e),
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

async fn run_one(payload: &GoTestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, output) = go_test(&payload.go_mod_path).await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr: output,
    }
}

async fn go_test(go_mod_path: &str) -> (bool, String) {
    let module_dir = std::path::Path::new(go_mod_path)
        .parent()
        .and_then(|p| p.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(".");

    let output = Command::new("go")
        .args(["test", "./..."])
        .current_dir(module_dir)
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
    fn go_test_payload_roundtrips() {
        let payload = GoTestPayload {
            go_mod_path: "go.mod".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: GoTestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.go_mod_path, payload.go_mod_path);
    }

    #[test]
    fn discover_emits_single_test() {
        let payload = GoTestPayload {
            go_mod_path: "go.mod".to_string(),
        };
        let test = Test {
            name: "go test".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&payload).unwrap(),
        };

        assert_eq!(test.manager.to_string(), "custom:go");
        assert_eq!(
            serde_json::to_value(&test.manager).unwrap(),
            serde_json::json!("custom:go")
        );
        let parsed: GoTestPayload = serde_json::from_value(test.payload).unwrap();
        assert_eq!(parsed.go_mod_path, "go.mod");
    }
}
