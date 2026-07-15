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

pub(crate) use detect::detect_elixir;
pub(crate) use errors::ElixirError;

const MANAGER_NAME: &str = "elixir";

fn manager_identity() -> api::TestManager {
    api::TestManager::custom(MANAGER_NAME).expect("\"elixir\" is a valid custom manager name")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ElixirTestPayload {
    pub(crate) mix_exs_path: String,
}

pub(crate) struct ElixirManager {
    mix_exs_path: String,
}

impl ElixirManager {
    pub(crate) fn new(mix_exs_path: String) -> Self {
        Self { mix_exs_path }
    }
}

#[async_trait]
impl TestManager for ElixirManager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let tests = vec![Test {
            name: "mix test".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&ElixirTestPayload {
                mix_exs_path: self.mix_exs_path.clone(),
            })
            .expect("ElixirTestPayload serialise is infallible"),
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

                let outcome = match serde_json::from_value::<ElixirTestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed elixir payload: {}", e),
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

async fn run_one(payload: &ElixirTestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, stderr) = mix_test(&payload.mix_exs_path).await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr,
    }
}

async fn mix_test(mix_exs_path: &str) -> (bool, String) {
    let project_dir = std::path::Path::new(mix_exs_path)
        .parent()
        .and_then(|p| p.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(".");

    let output = Command::new("mix")
        .arg("test")
        .current_dir(project_dir)
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
    fn elixir_test_payload_roundtrips() {
        let payload = ElixirTestPayload {
            mix_exs_path: "mix.exs".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: ElixirTestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.mix_exs_path, payload.mix_exs_path);
    }

    #[test]
    fn discover_emits_single_test() {
        let payload = ElixirTestPayload {
            mix_exs_path: "mix.exs".to_string(),
        };
        let test = Test {
            name: "mix test".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&payload).unwrap(),
        };

        assert_eq!(test.manager.to_string(), "custom:elixir");
        assert_eq!(
            serde_json::to_value(&test.manager).unwrap(),
            serde_json::json!("custom:elixir")
        );
        let parsed: ElixirTestPayload = serde_json::from_value(test.payload).unwrap();
        assert_eq!(parsed.mix_exs_path, "mix.exs");
    }
}
