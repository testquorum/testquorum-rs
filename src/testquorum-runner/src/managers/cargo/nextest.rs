//! The `cargo nextest` execution backend for the Cargo manager.
//!
//! nextest is wired in as an alternate backend rather than a separate manager:
//! discovery produces the exact same `{package}::{test_name}` names as the
//! plain `cargo test` path (verified against `cargo nextest list`'s
//! `package-name` + testcase key), so the two are fully interchangeable on the
//! wire. Only unit/integration tests run through here — nextest cannot execute
//! doctests, so those stay on `cargo test --doc` in the parent module.

use std::collections::HashMap;
use std::process::Stdio;

use serde::Deserialize;
use tokio::process::Command;

use super::CargoError;

/// Where nextest sources its test binaries from. The two variants differ only
/// in the CLI flags they contribute ([`source_args`]); discovery and execution
/// are otherwise identical, which is what lets the archive backend (added in a
/// later change) reuse this whole module.
///
/// [`source_args`]: NextestSource::source_args
#[derive(Clone)]
pub(crate) enum NextestSource {
    /// Build from the local workspace at the given `Cargo.toml`.
    Local { manifest_path: String },
}

impl NextestSource {
    /// Flags that point `cargo nextest list`/`run` at this source's binaries.
    fn source_args(&self) -> Vec<String> {
        match self {
            NextestSource::Local { manifest_path } => {
                vec!["--manifest-path".to_string(), manifest_path.clone()]
            }
        }
    }
}

/// A single discovered unit/integration test: its owning package and the
/// nextest test name (e.g. `tests::add_works`). The wire name is composed by
/// the caller as `{package}::{test_name}` to match the `cargo test` backend.
pub(crate) struct NextestCase {
    pub(crate) package: String,
    pub(crate) test_name: String,
}

#[derive(Deserialize)]
struct NextestList {
    #[serde(rename = "rust-suites")]
    rust_suites: HashMap<String, NextestSuite>,
}

#[derive(Deserialize)]
struct NextestSuite {
    #[serde(rename = "package-name")]
    package_name: String,
    testcases: HashMap<String, NextestTestcase>,
}

#[derive(Deserialize)]
struct NextestTestcase {}

/// Lists unit/integration tests via `cargo nextest list --message-format json`.
pub(crate) async fn nextest_list(source: &NextestSource) -> Result<Vec<NextestCase>, CargoError> {
    let mut args = vec![
        "nextest".to_string(),
        "list".to_string(),
        "--message-format".to_string(),
        "json".to_string(),
    ];
    args.extend(source.source_args());

    let output = Command::new("cargo")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| CargoError::NextestFailed {
            stderr: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(CargoError::NextestFailed { stderr });
    }

    let list: NextestList =
        serde_json::from_slice(&output.stdout).map_err(|e| CargoError::NextestFailed {
            stderr: format!("failed to parse `cargo nextest list` output: {}", e),
        })?;

    let mut cases = Vec::new();
    for suite in list.rust_suites.into_values() {
        for test_name in suite.testcases.into_keys() {
            cases.push(NextestCase {
                package: suite.package_name.clone(),
                test_name,
            });
        }
    }
    Ok(cases)
}

/// Runs a single test through nextest, selecting it with an exact-match filter
/// expression so the name maps back unambiguously. Returns `(passed, output)`.
pub(crate) async fn nextest_run_one(
    source: &NextestSource,
    package: &str,
    test_name: &str,
) -> (bool, String) {
    let filter = format!("package(={}) and test(={})", package, test_name);
    let mut args = vec!["nextest".to_string(), "run".to_string()];
    args.extend(source.source_args());
    args.push("-E".to_string());
    args.push(filter);

    let output = Command::new("cargo")
        .args(&args)
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
