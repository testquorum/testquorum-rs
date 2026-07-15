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

pub(crate) use detect::detect_dart;
pub(crate) use errors::DartError;

const MANAGER_NAME: &str = "dart";

fn manager_identity() -> api::TestManager {
    api::TestManager::custom(MANAGER_NAME).expect("\"dart\" is a valid custom manager name")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DartTestPayload {
    pub(crate) pubspec_path: String,
}

pub(crate) struct DartManager {
    pubspec_path: String,
}

impl DartManager {
    pub(crate) fn new(pubspec_path: String) -> Self {
        Self { pubspec_path }
    }
}

#[async_trait]
impl TestManager for DartManager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let tests = vec![Test {
            name: "dart test".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&DartTestPayload {
                pubspec_path: self.pubspec_path.clone(),
            })
            .expect("DartTestPayload serialise is infallible"),
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

                let outcome = match serde_json::from_value::<DartTestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed dart payload: {}", e),
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

async fn run_one(payload: &DartTestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, output) = dart_test(&payload.pubspec_path).await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr: output,
    }
}

async fn dart_test(pubspec_path: &str) -> (bool, String) {
    let dir = std::path::Path::new(pubspec_path)
        .parent()
        .and_then(|p| p.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(".");

    // First: dart pub get
    let get_output = Command::new("dart")
        .args(["pub", "get"])
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    let (get_ok, get_combined) = match get_output {
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
    };

    if !get_ok {
        return (false, get_combined);
    }

    // Then: dart test
    let test_output = Command::new("dart")
        .arg("test")
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match test_output {
        Ok(output) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }
            let mut full = get_combined;
            if !combined.is_empty() {
                if !full.is_empty() {
                    full.push('\n');
                }
                full.push_str(&combined);
            }
            (output.status.success(), full)
        }
        Err(e) => (false, format!("{}{}", get_combined, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dart_test_payload_roundtrips() {
        let payload = DartTestPayload {
            pubspec_path: "pubspec.yaml".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: DartTestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.pubspec_path, payload.pubspec_path);
    }

    #[test]
    fn discover_emits_single_test() {
        let payload = DartTestPayload {
            pubspec_path: "pubspec.yaml".to_string(),
        };
        let test = Test {
            name: "dart test".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&payload).unwrap(),
        };

        assert_eq!(test.manager.to_string(), "custom:dart");
        assert_eq!(
            serde_json::to_value(&test.manager).unwrap(),
            serde_json::json!("custom:dart")
        );
        let parsed: DartTestPayload = serde_json::from_value(test.payload).unwrap();
        assert_eq!(parsed.pubspec_path, "pubspec.yaml");
    }
}
