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

pub(crate) use detect::detect_dotnet;
pub(crate) use errors::DotnetError;

const MANAGER_NAME: &str = "dotnet";

fn manager_identity() -> api::TestManager {
    api::TestManager::custom(MANAGER_NAME).expect("\"dotnet\" is a valid custom manager name")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DotnetTestPayload {
    pub(crate) project_path: String,
}

pub(crate) struct DotnetManager {
    project_path: String,
}

impl DotnetManager {
    pub(crate) fn new(project_path: String) -> Self {
        Self { project_path }
    }
}

#[async_trait]
impl TestManager for DotnetManager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let tests = vec![Test {
            name: "dotnet test".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&DotnetTestPayload {
                project_path: self.project_path.clone(),
            })
            .expect("DotnetTestPayload serialise is infallible"),
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

                let outcome = match serde_json::from_value::<DotnetTestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed dotnet payload: {}", e),
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

async fn run_one(payload: &DotnetTestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, stderr) = dotnet_test(&payload.project_path).await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr,
    }
}

async fn dotnet_test(project_path: &str) -> (bool, String) {
    let output = Command::new("dotnet")
        .args(["test", project_path])
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
    fn dotnet_test_payload_roundtrips() {
        let payload = DotnetTestPayload {
            project_path: "MyApp.sln".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: DotnetTestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.project_path, payload.project_path);
    }

    #[test]
    fn discover_emits_single_test() {
        let payload = DotnetTestPayload {
            project_path: "MyApp.sln".to_string(),
        };
        let test = Test {
            name: "dotnet test".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&payload).unwrap(),
        };

        assert_eq!(test.manager.to_string(), "custom:dotnet");
        assert_eq!(
            serde_json::to_value(&test.manager).unwrap(),
            serde_json::json!("custom:dotnet")
        );
        let parsed: DotnetTestPayload = serde_json::from_value(test.payload).unwrap();
        assert_eq!(parsed.project_path, "MyApp.sln");
    }
}
