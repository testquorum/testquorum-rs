use std::pin::Pin;
use std::process::Stdio;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use serde::Deserialize;
use serde::Serialize;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::Test;
use crate::TestEvent;
use crate::TestManager;
use crate::TestOutcome;

pub(crate) mod detect;
pub(crate) mod errors;

pub(crate) use detect::detect_npm;
pub(crate) use errors::NpmError;

const MANAGER_NAME: &str = "npm";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NpmTestPayload {
    pub(crate) package_json_path: String,
}

pub(crate) struct NpmManager {
    package_json_path: String,
}

impl NpmManager {
    pub(crate) fn new(package_json_path: String) -> Self {
        Self { package_json_path }
    }
}

#[async_trait]
impl TestManager for NpmManager {
    fn name(&self) -> &'static str {
        MANAGER_NAME
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let tests = vec![Test {
            name: "npm test".to_string(),
            manager: MANAGER_NAME.to_string(),
            payload: serde_json::to_value(&NpmTestPayload {
                package_json_path: self.package_json_path.clone(),
            })
            .expect("NpmTestPayload serialise is infallible"),
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
                        manager: MANAGER_NAME.to_string(),
                    })
                    .await
                    .is_err()
                {
                    return;
                }

                let outcome = match serde_json::from_value::<NpmTestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed npm payload: {}", e),
                    },
                };

                if tx
                    .send(TestEvent::Finished {
                        name: test.name,
                        manager: MANAGER_NAME.to_string(),
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

async fn run_one(payload: &NpmTestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, stderr) = npm_test(&payload.package_json_path).await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr,
    }
}

async fn npm_test(package_json_path: &str) -> (bool, String) {
    let prefix = std::path::Path::new(package_json_path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(".");

    let output = Command::new("npm")
        .args(["test", "--prefix", prefix])
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
    fn npm_test_payload_roundtrips() {
        let payload = NpmTestPayload {
            package_json_path: "package.json".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: NpmTestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.package_json_path, payload.package_json_path);
    }

    #[test]
    fn discover_emits_single_test() {
        let payload = NpmTestPayload {
            package_json_path: "package.json".to_string(),
        };
        let test = Test {
            name: "npm test".to_string(),
            manager: MANAGER_NAME.to_string(),
            payload: serde_json::to_value(&payload).unwrap(),
        };

        assert_eq!(test.manager, "npm");
        let parsed: NpmTestPayload = serde_json::from_value(test.payload).unwrap();
        assert_eq!(parsed.package_json_path, "package.json");
    }
}
