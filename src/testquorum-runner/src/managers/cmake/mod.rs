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

pub(crate) use detect::detect_cmake;
pub(crate) use errors::CmakeError;

const MANAGER_NAME: &str = "cmake";

fn manager_identity() -> api::TestManager {
    api::TestManager::custom(MANAGER_NAME).expect("\"cmake\" is a valid custom manager name")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CmakeTestPayload {
    pub(crate) cmake_lists_path: String,
}

pub(crate) struct CmakeManager {
    cmake_lists_path: String,
}

impl CmakeManager {
    pub(crate) fn new(cmake_lists_path: String) -> Self {
        Self { cmake_lists_path }
    }
}

#[async_trait]
impl TestManager for CmakeManager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let tests = vec![Test {
            name: "ctest".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&CmakeTestPayload {
                cmake_lists_path: self.cmake_lists_path.clone(),
            })
            .expect("CmakeTestPayload serialise is infallible"),
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

                let outcome = match serde_json::from_value::<CmakeTestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed cmake payload: {}", e),
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

async fn run_one(payload: &CmakeTestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, output) = cmake_build_and_test(&payload.cmake_lists_path).await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr: output,
    }
}

async fn cmake_build_and_test(cmake_lists_path: &str) -> (bool, String) {
    let source_dir = std::path::Path::new(cmake_lists_path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(".");

    let build_dir = format!("{}/build", source_dir);
    let mut combined = String::new();

    // Step 1: cmake configure
    let configure = Command::new("cmake")
        .args(["-B", &build_dir, source_dir])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match configure {
        Ok(output) => {
            append_output(&mut combined, &output.stdout, &output.stderr);
            if !output.status.success() {
                return (false, combined);
            }
        }
        Err(e) => return (false, e.to_string()),
    }

    // Step 2: cmake build
    let build = Command::new("cmake")
        .args(["--build", &build_dir])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match build {
        Ok(output) => {
            append_output(&mut combined, &output.stdout, &output.stderr);
            if !output.status.success() {
                return (false, combined);
            }
        }
        Err(e) => return (false, e.to_string()),
    }

    // Step 3: ctest
    let test = Command::new("ctest")
        .arg("--output-on-failure")
        .current_dir(&build_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match test {
        Ok(output) => {
            append_output(&mut combined, &output.stdout, &output.stderr);
            (output.status.success(), combined)
        }
        Err(e) => (false, e.to_string()),
    }
}

fn append_output(combined: &mut String, stdout: &[u8], stderr: &[u8]) {
    let out = String::from_utf8_lossy(stdout);
    if !out.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&out);
    }
    let err = String::from_utf8_lossy(stderr);
    if !err.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmake_test_payload_roundtrips() {
        let payload = CmakeTestPayload {
            cmake_lists_path: "CMakeLists.txt".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: CmakeTestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.cmake_lists_path, payload.cmake_lists_path);
    }

    #[test]
    fn discover_emits_single_ctest() {
        let payload = CmakeTestPayload {
            cmake_lists_path: "CMakeLists.txt".to_string(),
        };
        let test = Test {
            name: "ctest".to_string(),
            manager: manager_identity(),
            payload: serde_json::to_value(&payload).unwrap(),
        };

        assert_eq!(test.manager.to_string(), "custom:cmake");
        assert_eq!(
            serde_json::to_value(&test.manager).unwrap(),
            serde_json::json!("custom:cmake")
        );
        let parsed: CmakeTestPayload = serde_json::from_value(test.payload).unwrap();
        assert_eq!(parsed.cmake_lists_path, "CMakeLists.txt");
    }
}
