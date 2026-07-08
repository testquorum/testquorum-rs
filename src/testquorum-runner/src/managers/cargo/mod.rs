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
use tokio::sync::OnceCell;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::Test;
use crate::TestEvent;
use crate::TestManager;
use crate::TestOutcome;

pub(crate) mod detect;
pub(crate) mod errors;
pub(crate) mod nextest;

pub(crate) use detect::detect_cargo;
pub(crate) use errors::CargoError;
use nextest::NextestSource;
use nextest::nextest_list;
use nextest::nextest_run_one;
use nextest::prepare_archive;

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

/// Which tool actually compiles and runs the unit/integration tests. Doctests
/// are unaffected — they always run via `cargo test --doc` because nextest
/// cannot execute them.
#[derive(Clone)]
enum CargoBackend {
    /// Plain `cargo test`: compile test binaries, then run each one with
    /// `--exact`. The original (and fallback) backend.
    CargoTest,
    /// `cargo nextest`, sourcing test binaries per the [`NextestSource`].
    Nextest(NextestSource),
}

impl CargoBackend {
    /// Short human-readable name for diagnostics.
    fn name(&self) -> &'static str {
        match self {
            CargoBackend::CargoTest => "cargo test",
            CargoBackend::Nextest(_) => "cargo-nextest",
        }
    }
}

/// Picks the execution backend from the configured `nextest` preference and
/// whether `cargo nextest` is actually available. Kept pure (no `PATH` lookup)
/// so the decision matrix is unit-testable; [`CargoManager::new`] supplies the
/// real availability. `None` auto-detects (nextest when present), `Some(false)`
/// forces plain `cargo test`, `Some(true)` requires nextest and warns on the
/// fallback when it is missing.
fn decide_backend(nextest: Option<bool>, available: bool, manifest_path: &str) -> CargoBackend {
    let backend = if nextest == Some(false) {
        CargoBackend::CargoTest
    } else if available {
        CargoBackend::Nextest(NextestSource::Local {
            manifest_path: manifest_path.to_string(),
        })
    } else {
        if nextest == Some(true) {
            eprintln!("warning: cargo nextest requested but not found on PATH; using `cargo test`");
        }
        CargoBackend::CargoTest
    };
    // Announce the selected backend so a CI run can confirm which one drove the
    // tests — the two are otherwise indistinguishable in the output.
    eprintln!("cargo: using {} backend", backend.name());
    backend
}

/// The workspace root nextest should remap an archive's paths onto: the
/// directory containing the (workspace) manifest, or the current directory when
/// that is the manifest's own folder.
fn workspace_root(manifest_path: &str) -> String {
    match std::path::Path::new(manifest_path).parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
        _ => ".".to_string(),
    }
}

pub(crate) struct CargoManager {
    manifest_path: String,
    nextest: Option<bool>,
    nextest_archive: Option<String>,
    /// The execution backend, resolved on first use rather than at
    /// construction. Staging an archive is fallible (and, for a `nix://`
    /// source in a later change, does real work), so it belongs in `discover`'s
    /// error path — which the run loop reports — not in a constructor that
    /// would have to abort the whole runner. `run` reuses whatever `discover`
    /// resolved, so the resolution (and its `cargo nextest`/staging side
    /// effects) happens exactly once.
    backend: OnceCell<CargoBackend>,
}

impl CargoManager {
    /// `nextest`/`nextest_archive` are the configured `[managers.cargo]`
    /// preferences. Construction is infallible; the backend — the `cargo
    /// nextest` `PATH` probe and any archive staging — is resolved lazily in
    /// [`backend`](Self::backend). An archive forces the nextest backend; the
    /// caller is responsible for rejecting the contradictory `nextest = false`
    /// pairing.
    pub(crate) fn new(
        manifest_path: String,
        nextest: Option<bool>,
        nextest_archive: Option<String>,
    ) -> Self {
        Self {
            manifest_path,
            nextest,
            nextest_archive,
            backend: OnceCell::new(),
        }
    }

    /// Resolves the execution backend once and caches it. An archive is staged
    /// here — lazily — so a bad archive surfaces as a discovery error the run
    /// loop can report, rather than aborting construction. Repeated calls (from
    /// `discover` then `run`) reuse the first result and never re-stage.
    async fn backend(&self) -> Result<&CargoBackend, CargoError> {
        self.backend
            .get_or_try_init(|| async {
                match self.nextest_archive.as_deref() {
                    Some(path) => {
                        if !detect::detect_nextest() {
                            return Err(CargoError::NextestArchiveNoNextest);
                        }
                        let source =
                            prepare_archive(path, workspace_root(&self.manifest_path)).await?;
                        Ok(CargoBackend::Nextest(source))
                    }
                    None => Ok(decide_backend(
                        self.nextest,
                        detect::detect_nextest(),
                        &self.manifest_path,
                    )),
                }
            })
            .await
    }
}

#[async_trait]
impl TestManager for CargoManager {
    fn identity(&self) -> api::TestManager {
        manager_identity()
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        // Resolve the backend first so a broken archive fails fast — before we
        // spend a `cargo metadata` call — and surfaces as this manager's
        // discovery error.
        let backend = self.backend().await?;

        let metadata = discover_metadata(&self.manifest_path).await?;

        let workspace_packages: HashMap<String, MetadataPackage> = metadata
            .packages
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect();

        // Unit/integration tests come from the configured backend. Both
        // backends emit identical `{package}::{test_name}` names and `Unit`
        // payloads, so a run is fully interchangeable between them on the wire.
        let mut tests = match backend {
            CargoBackend::CargoTest => {
                discover_unit_cargo(&self.manifest_path, &workspace_packages).await?
            }
            CargoBackend::Nextest(source) => discover_unit_nextest(source).await?,
        };

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

        // `discover` resolves and caches the backend before any `run`, so this
        // is a cached hit. Stay defensive rather than panic if that invariant
        // ever breaks: surface it as a per-test failure.
        let backend = match self.backend().await {
            Ok(backend) => backend.clone(),
            Err(e) => {
                let reason = format!("cargo backend unavailable: {}", e);
                tokio::spawn(async move {
                    for test in tests {
                        let _ = tx
                            .send(TestEvent::Started {
                                name: test.name.clone(),
                                manager: manager_identity(),
                            })
                            .await;
                        let _ = tx
                            .send(TestEvent::Finished {
                                name: test.name,
                                manager: manager_identity(),
                                outcome: TestOutcome {
                                    passed: false,
                                    duration_ms: 0,
                                    stderr: reason.clone(),
                                },
                            })
                            .await;
                    }
                });
                return Box::pin(ReceiverStream::new(rx));
            }
        };

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
                let backend = backend.clone();
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
                            Ok(payload) => run_one(&payload, &manifest_path, &backend).await,
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

/// Builds the wire `Test` for one unit/integration test. Centralised so both
/// backends produce byte-identical names and payloads.
fn unit_test(package: &str, test_name: String) -> Test {
    Test {
        name: format!("{}::{}", package, test_name),
        manager: manager_identity(),
        payload: serde_json::to_value(&CargoTestPayload {
            package: package.to_string(),
            test_name,
            kind: TestKind::Unit,
        })
        .expect("CargoTestPayload serialise is infallible"),
    }
}

/// Discovers unit/integration tests by compiling the test binaries with
/// `cargo test --no-run` and asking each one to `--list` its tests.
async fn discover_unit_cargo(
    manifest_path: &str,
    workspace_packages: &HashMap<String, MetadataPackage>,
) -> Result<Vec<Test>, anyhow::Error> {
    let artifacts = compile_test_binaries(manifest_path).await?;

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

        for test_name in list_tests_in_binary(&exe).await? {
            tests.push(unit_test(&pkg.name, test_name));
        }
    }
    Ok(tests)
}

/// Discovers unit/integration tests via `cargo nextest list`.
async fn discover_unit_nextest(source: &NextestSource) -> Result<Vec<Test>, anyhow::Error> {
    let cases = nextest_list(source).await?;
    Ok(cases
        .into_iter()
        .map(|c| unit_test(&c.package, c.test_name))
        .collect())
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

async fn run_one(
    payload: &CargoTestPayload,
    manifest_path: &str,
    backend: &CargoBackend,
) -> TestOutcome {
    let start = Instant::now();
    let (passed, stderr) = match payload.kind {
        // Unit/integration tests follow the backend; doctests never can —
        // nextest doesn't run them — so they always go through `cargo test`.
        TestKind::Unit => match backend {
            CargoBackend::CargoTest => {
                run_unit_test(&payload.package, &payload.test_name, manifest_path).await
            }
            CargoBackend::Nextest(source) => {
                nextest_run_one(source, &payload.package, &payload.test_name).await
            }
        },
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

    fn is_nextest(backend: &CargoBackend) -> bool {
        matches!(backend, CargoBackend::Nextest(_))
    }

    /// A path guaranteed not to exist, so archive staging fails deterministically
    /// regardless of whether `cargo-nextest` is on `PATH` in the test env.
    const MISSING_ARCHIVE: &str = "/nonexistent/testquorum/archive.tar.zst";

    #[tokio::test]
    async fn construction_is_infallible_and_defers_archive_resolution() {
        // `new` must not touch the filesystem or probe `PATH`: a bad archive is
        // accepted here and only fails later, in `discover`. If this ever became
        // fallible again the type would change; this pins the contract.
        let manager = CargoManager::new(
            "Cargo.toml".to_string(),
            None,
            Some(MISSING_ARCHIVE.to_string()),
        );
        assert!(
            manager.backend.get().is_none(),
            "backend must stay unresolved until first use"
        );
    }

    #[tokio::test]
    async fn discover_reports_unresolvable_archive_as_error() {
        // A configured-but-unstageable archive is surfaced as a discovery error
        // (which the run loop reports and exits non-zero on), never a panic and
        // never a silently-empty test set.
        let manager = CargoManager::new(
            "Cargo.toml".to_string(),
            None,
            Some(MISSING_ARCHIVE.to_string()),
        );
        let err = manager
            .discover()
            .await
            .expect_err("an unstageable archive must be a discovery error");
        assert!(!err.to_string().is_empty());
    }

    #[tokio::test]
    async fn run_emits_failures_when_backend_unavailable() {
        use futures::StreamExt;

        // `run` after an unresolvable backend must still drive every test to a
        // Finished(failed) rather than hang or panic.
        let manager = CargoManager::new(
            "Cargo.toml".to_string(),
            None,
            Some(MISSING_ARCHIVE.to_string()),
        );
        let test = unit_test("pkg", "some_test".to_string());
        let name = test.name.clone();

        let mut stream = manager.run(vec![test]).await;
        let mut started = false;
        let mut finished_failed = false;
        while let Some(event) = stream.next().await {
            match event {
                TestEvent::Started { name: n, .. } if n == name => started = true,
                TestEvent::Finished {
                    name: n, outcome, ..
                } if n == name => {
                    assert!(!outcome.passed, "unavailable backend must fail the test");
                    finished_failed = true;
                }
                _ => {}
            }
        }
        assert!(started, "expected a Started event");
        assert!(finished_failed, "expected a failed Finished event");
    }

    #[test]
    fn decide_backend_auto_uses_nextest_when_available() {
        assert!(is_nextest(&decide_backend(None, true, "Cargo.toml")));
    }

    #[test]
    fn decide_backend_auto_falls_back_without_nextest() {
        assert!(!is_nextest(&decide_backend(None, false, "Cargo.toml")));
    }

    #[test]
    fn decide_backend_opt_out_forces_cargo_test_even_when_available() {
        assert!(!is_nextest(&decide_backend(
            Some(false),
            true,
            "Cargo.toml"
        )));
    }

    #[test]
    fn decide_backend_force_uses_nextest_when_available() {
        assert!(is_nextest(&decide_backend(Some(true), true, "Cargo.toml")));
    }

    #[test]
    fn decide_backend_force_falls_back_when_missing() {
        // Warns (to stderr) but must not fail to produce a usable backend.
        assert!(!is_nextest(&decide_backend(
            Some(true),
            false,
            "Cargo.toml"
        )));
    }

    #[test]
    fn workspace_root_defaults_to_cwd_for_bare_manifest() {
        assert_eq!(workspace_root("Cargo.toml"), ".");
    }

    #[test]
    fn workspace_root_uses_manifest_parent() {
        assert_eq!(workspace_root("subdir/Cargo.toml"), "subdir");
    }

    #[test]
    fn decide_backend_threads_manifest_path_into_local_source() {
        match decide_backend(None, true, "subdir/Cargo.toml") {
            CargoBackend::Nextest(NextestSource::Local { manifest_path }) => {
                assert_eq!(manifest_path, "subdir/Cargo.toml");
            }
            _ => panic!("expected local nextest backend"),
        }
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
