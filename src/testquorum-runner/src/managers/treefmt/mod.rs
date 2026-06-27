use std::path::Path;
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

pub(crate) use detect::detect_treefmt;
pub(crate) use errors::TreefmtError;

const CONFIG_FILE_NAMES: &[&str] = &["treefmt.toml", ".treefmt.toml"];

fn manager_identity() -> api::TestManager {
    api::WellKnownTestManager::Treefmt.into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TreefmtTestPayload {
    pub(crate) test_name: String,
}

pub(crate) struct TreefmtManager {
    enabled: Option<bool>,
}

impl TreefmtManager {
    pub(crate) fn new(enabled: Option<bool>) -> Self {
        Self { enabled }
    }
}

#[async_trait]
impl TestManager for TreefmtManager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        match self.enabled {
            Some(false) => return Ok(Vec::new()),
            Some(true) => {}
            None => {
                if find_config_file().is_none() {
                    return Ok(Vec::new());
                }
            }
        }

        let tests = vec![Test {
            name: "treefmt".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&TreefmtTestPayload {
                test_name: "treefmt".to_string(),
            })
            .expect("TreefmtTestPayload serialise is infallible"),
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

                let outcome = match serde_json::from_value::<TreefmtTestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed treefmt payload: {}", e),
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

fn find_config_file() -> Option<std::path::PathBuf> {
    CONFIG_FILE_NAMES
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
        .map(|p| p.to_path_buf())
}

async fn run_one(_payload: &TreefmtTestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, stderr) = treefmt_check().await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr,
    }
}

async fn treefmt_check() -> (bool, String) {
    let output = Command::new("treefmt")
        .args(["--ci"])
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
    fn treefmt_test_payload_roundtrips() {
        let payload = TreefmtTestPayload {
            test_name: "treefmt".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: TreefmtTestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.test_name, payload.test_name);
    }

    #[test]
    fn config_file_names_are_standard() {
        assert!(CONFIG_FILE_NAMES.contains(&"treefmt.toml"));
        assert!(CONFIG_FILE_NAMES.contains(&".treefmt.toml"));
    }
}
