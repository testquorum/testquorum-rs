use std::collections::HashMap;
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

pub(crate) use detect::detect_nix;
pub(crate) use errors::NixError;

const MANAGER_NAME: &str = "nix";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NixTestPayload {
    pub(crate) build_target: String,
}

#[derive(Deserialize)]
struct DiscoverOutput {
    #[serde(rename = "currentSystem")]
    current_system: String,
    #[serde(rename = "perSystem")]
    per_system: HashMap<String, HashMap<String, Option<String>>>,
}

pub(crate) struct NixManager {
    attrset: String,
}

impl NixManager {
    pub(crate) fn new(attrset: String) -> Self {
        Self { attrset }
    }
}

#[async_trait]
impl TestManager for NixManager {
    fn name(&self) -> &'static str {
        MANAGER_NAME
    }

    async fn discover(&self) -> Result<Vec<Test>, anyhow::Error> {
        let flake_ref = format!(".#{}", self.attrset);
        // Single eval: returns both the host system and a per-system map of
        // attribute name → drvPath. Forcing drvPath here is the whole point —
        // it lets `run()` build via `<drv>^*` without re-evaluating the flake
        // per test. `--impure` is required for builtins.currentSystem.
        let apply_expr = "x: { \
            currentSystem = builtins.currentSystem; \
            perSystem = builtins.mapAttrs \
                (system: attrs: builtins.mapAttrs (name: drv: drv.drvPath or null) attrs) \
                x; \
        }";

        let output = Command::new("nix")
            .args([
                "eval", &flake_ref, "--impure", "--json", "--apply", apply_expr,
            ])
            .output()
            .await
            .map_err(|e| NixError::EvaluationFailed {
                stderr: e.to_string(),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(NixError::EvaluationFailed { stderr }.into());
        }

        let parsed: DiscoverOutput =
            serde_json::from_slice(&output.stdout).map_err(|e| NixError::EvaluationFailed {
                stderr: e.to_string(),
            })?;

        let names = match parsed.per_system.get(&parsed.current_system) {
            Some(map) => map,
            None => return Ok(Vec::new()),
        };

        let tests = names
            .iter()
            .map(|(name, drv)| {
                let payload = NixTestPayload {
                    build_target: resolve_build_target(
                        &self.attrset,
                        &parsed.current_system,
                        name,
                        drv.as_deref(),
                    ),
                };
                Test {
                    name: name.clone(),
                    manager: MANAGER_NAME.to_string(),
                    payload: serde_json::to_value(&payload)
                        .expect("NixTestPayload serialise is infallible"),
                }
            })
            .collect();

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

                let outcome = match serde_json::from_value::<NixTestPayload>(test.payload) {
                    Ok(payload) => run_one(&payload).await,
                    Err(e) => TestOutcome {
                        passed: false,
                        duration_ms: 0,
                        stderr: format!("malformed nix payload: {}", e),
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

fn resolve_build_target(attrset: &str, system: &str, name: &str, drv_path: Option<&str>) -> String {
    // Prefer the pre-evaluated derivation path so `nix build` skips
    // re-evaluation; fall back to the flake attribute when the discoverer
    // didn't get one. The `^*` selector is required on drv paths — without it,
    // `nix build` is a silent no-op that prints the drv path back.
    match drv_path {
        Some(drv) => format!("{}^*", drv),
        None => format!(".#{}.{}.{}", attrset, system, name),
    }
}

async fn run_one(payload: &NixTestPayload) -> TestOutcome {
    let start = Instant::now();
    let (passed, stderr) = nix_build(&payload.build_target).await;
    TestOutcome {
        passed,
        duration_ms: start.elapsed().as_millis() as u64,
        stderr,
    }
}

async fn nix_build(target: &str) -> (bool, String) {
    let output = Command::new("nix")
        .args(["build", target, "--no-link", "--print-out-paths"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match output {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ),
        Err(e) => (false, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_discover_output() {
        let json = r#"
        {
            "currentSystem": "x86_64-linux",
            "perSystem": {
                "x86_64-linux": {
                    "testquorum-clippy": "/nix/store/aaa-clippy.drv",
                    "testquorum-nextest": null
                },
                "aarch64-linux": {
                    "testquorum-clippy": "/nix/store/bbb-clippy.drv"
                }
            }
        }
        "#;

        let parsed: DiscoverOutput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.current_system, "x86_64-linux");
        let host = parsed.per_system.get("x86_64-linux").unwrap();
        assert_eq!(
            host.get("testquorum-clippy").unwrap().as_deref(),
            Some("/nix/store/aaa-clippy.drv"),
        );
        assert!(host.get("testquorum-nextest").unwrap().is_none());
    }

    #[test]
    fn resolve_build_target_prefers_drv_path() {
        // `^*` selector is required — bare-drv `nix build` doesn't realise outputs.
        assert_eq!(
            resolve_build_target("ci", "x86_64-linux", "foo", Some("/nix/store/abc-foo.drv")),
            "/nix/store/abc-foo.drv^*",
        );
    }

    #[test]
    fn resolve_build_target_falls_back_to_flake_attribute() {
        assert_eq!(
            resolve_build_target("ci", "x86_64-linux", "foo", None),
            ".#ci.x86_64-linux.foo",
        );
    }

    #[test]
    fn discover_emits_tests_with_nix_payload() {
        // Hand-build the Test shape the discoverer produces to lock in the
        // contract: manager tag is "nix" and the payload deserialises into
        // NixTestPayload with the resolved build target.
        let payload = NixTestPayload {
            build_target: "/nix/store/abc-foo.drv^*".to_string(),
        };
        let test = Test {
            name: "foo".to_string(),
            manager: MANAGER_NAME.to_string(),
            payload: serde_json::to_value(&payload).unwrap(),
        };

        assert_eq!(test.manager, "nix");
        let parsed: NixTestPayload = serde_json::from_value(test.payload).unwrap();
        assert_eq!(parsed.build_target, "/nix/store/abc-foo.drv^*");
    }
}
