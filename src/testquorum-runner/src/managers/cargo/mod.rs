use std::collections::HashMap;
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

pub(crate) use detect::detect_cargo;
pub(crate) use errors::CargoError;

fn manager_identity() -> api::TestManager {
    api::WellKnownTestManager::Cargo.into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CargoTestPayload {
    pub(crate) package: String,
    pub(crate) test_name: String,
    pub(crate) kind: TestKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum TestKind {
    Unit,
    Doctests,
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<MetadataPackage>,
}

#[derive(Deserialize)]
struct MetadataPackage {
    name: String,
    id: String,
    targets: Vec<MetadataTarget>,
}

#[derive(Deserialize)]
struct MetadataTarget {
    #[serde(default)]
    doctest: bool,
}

#[derive(Deserialize)]
struct CompilerArtifact {
    reason: String,
    package_id: String,
    profile: ArtifactProfile,
    executable: Option<String>,
}

#[derive(Deserialize)]
struct ArtifactProfile {
    test: bool,
}

pub(crate) struct CargoManager {
    manifest_path: String,
}

impl CargoManager {
    pub(crate) fn new(manifest_path: String) -> Self {
        Self { manifest_path }
    }
}

#[async_trait]
impl TestManager for CargoManager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let metadata = discover_metadata(&self.manifest_path).await?;

        let workspace_packages: HashMap<String, MetadataPackage> = metadata
            .packages
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect();

        let artifacts = compile_test_binaries(&self.manifest_path).await?;

        let mut tests = Vec::new();

        for artifact in artifacts {
            if artifact.reason != "compiler-artifact" || !artifact.profile.test {
                continue;
            }
            let Some(exe) = artifact.executable else {
                continue;
            };
            let Some(pkg) = workspace_packages.get(&artifact.package_id) else {
                continue;
            };

            let list = list_tests_in_binary(&exe).await?;
            for test_name in list {
                tests.push(Test {
                    name: format!("{}::{}", pkg.name, test_name),
                    manager: manager_identity(),
                    payload: serde_json::to_value(&CargoTestPayload {
                        package: pkg.name.clone(),
                        test_name,
                        kind: TestKind::Unit,
                    })
                    .expect("CargoTestPayload serialise is infallible"),
                });
            }
        }

        // Emit one synthetic test per package that has doctests.
        for pkg in workspace_packages.values() {
            if pkg.targets.iter().any(|t| t.doctest) {
                tests.push(Test {
                    name: format!("{}::[doctests]", pkg.name),
                    manager: manager_identity(),
                    payload: serde_json::to_value(&CargoTestPayload {
                        package: pkg.name.clone(),
                        test_name: "[doctests]".to_string(),
                        kind: TestKind::Doctests,
                    })
                    .expect("CargoTestPayload serialise is infallible"),
                });
            }
        }

        Ok(tests)
    }

    async fn run(&self, tests: Vec<Test>) -> Pin<Box<dyn Stream<Item = TestEvent> + Send>> {
        let (tx, rx) = mpsc::channel::<TestEvent>(1);
        let manifest_path = self.manifest_path.clone();

        tokio::spawn(async move {
            // Group by package so each package runs in its own sequential
            // stream, while different packages run in parallel.
            let mut by_package: HashMap<String, Vec<Test>> = HashMap::new();
            for test in tests {
                if let Ok(payload) =
                    serde_json::from_value::<CargoTestPayload>(test.payload.clone())
                {
                    by_package
                        .entry(payload.package.clone())
                        .or_default()
                        .push(test);
                }
            }

            let mut handles = Vec::new();
            for (_pkg, tests) in by_package {
                let tx = tx.clone();
                let manifest_path = manifest_path.clone();
                handles.push(tokio::spawn(async move {
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

                        let outcome = match serde_json::from_value::<CargoTestPayload>(test.payload)
                        {
                            Ok(payload) => run_one(&payload, &manifest_path).await,
                            Err(e) => TestOutcome {
                                passed: false,
                                duration_ms: 0,
                                stderr: format!("malformed cargo payload: {}", e),
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
                }));
            }

            for h in handles {
                let _ = h.await;
            }
        });

        Box::pin(ReceiverStream::new(rx))
    }
}

async fn discover_metadata(manifest_path: &str) -> Result<CargoMetadata, CargoError> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
            manifest_path,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| CargoError::MetadataFailed {
            stderr: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(CargoError::MetadataFailed { stderr });
    }

    serde_json::from_slice(&output.stdout).map_err(|e| CargoError::MetadataFailed {
        stderr: e.to_string(),
    })
}

async fn compile_test_binaries(manifest_path: &str) -> Result<Vec<CompilerArtifact>, CargoError> {
    let output = Command::new("cargo")
        .args([
            "test",
            "--no-run",
            "--message-format=json",
            "--manifest-path",
            manifest_path,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| CargoError::CompilationFailed {
            stderr: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(CargoError::CompilationFailed { stderr });
    }

    let mut artifacts = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Ok(artifact) = serde_json::from_str::<CompilerArtifact>(line) {
            artifacts.push(artifact);
        }
    }

    Ok(artifacts)
}

async fn list_tests_in_binary(exe: &str) -> Result<Vec<String>, CargoError> {
    let output = Command::new(exe)
        .arg("--list")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| CargoError::MetadataFailed {
            stderr: format!("failed to run {} --list: {}", exe, e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(CargoError::MetadataFailed {
            stderr: format!("{} --list failed: {}", exe, stderr),
        });
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut tests = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_suffix(": test") {
            tests.push(name.to_string());
        }
    }
    Ok(tests)
}

async fn run_one(payload: &CargoTestPayload, manifest_path: &str) -> TestOutcome {
    let start = Instant::now();
    let (passed, stderr) = match payload.kind {
        TestKind::Unit => run_unit_test(&payload.package, &payload.test_name, manifest_path).await,
        TestKind::Doctests => run_doctests(&payload.package, manifest_path).await,
    };
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr,
    }
}

async fn run_unit_test(package: &str, test_name: &str, manifest_path: &str) -> (bool, String) {
    let output = Command::new("cargo")
        .args([
            "test",
            "--manifest-path",
            manifest_path,
            "-p",
            package,
            "--",
            "--exact",
            test_name,
            "--test-threads=1",
        ])
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

async fn run_doctests(package: &str, manifest_path: &str) -> (bool, String) {
    let output = Command::new("cargo")
        .args([
            "test",
            "--manifest-path",
            manifest_path,
            "--doc",
            "-p",
            package,
        ])
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
    fn parse_test_list_skips_benchmarks() {
        let output = "foo: test\nbar: benchmark\nbaz: test\n";
        let lines: Vec<&str> = output.lines().collect();
        let tests: Vec<String> = lines
            .iter()
            .filter_map(|line| {
                let trimmed = line.trim();
                trimmed.strip_suffix(": test").map(|s| s.to_string())
            })
            .collect();
        assert_eq!(tests, vec!["foo", "baz"]);
    }

    #[test]
    fn cargo_test_payload_roundtrips() {
        let payload = CargoTestPayload {
            package: "my-crate".to_string(),
            test_name: "tests::it_works".to_string(),
            kind: TestKind::Unit,
        };
        let json = serde_json::to_value(&payload).unwrap();
        let back: CargoTestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.package, payload.package);
        assert_eq!(back.test_name, payload.test_name);
    }
}
