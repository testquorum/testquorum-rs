//! A shared handle for resolving and building Nix flake outputs.
//!
//! Cargo's `nix://` nextest archive is a build the Nix manager *also* tracks
//! (it's an attribute under `[managers.nix] attrset`). Routing Cargo's
//! resolution through this handle lets one flake eval answer the question Cargo
//! must settle before it depends on a build: *is this a target the Nix manager
//! tracks and will report?* If so, a failed build is the Nix manager's failed
//! test — not Cargo's problem — so Cargo can ignore it. If not, it's a
//! misconfiguration and the caller fails loudly.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;

use serde::Deserialize;
use testquorum_api::types as api;
use tokio::process::Command;
use tokio::sync::OnceCell;

use super::errors::NixError;
use super::manager_identity;

/// Placeholder expanded to the current Nix system inside a `nix://` spec, since
/// flake outputs are per-system.
const SYSTEM_PLACEHOLDER: &str = "${SYSTEM}";

/// The `--apply` expression for the tracked-set eval: a single eval returning
/// the host system and, per system, a map of attribute name -> `drvPath` (null
/// for non-derivations). `--impure` is required for `builtins.currentSystem`.
const APPLY_EXPR: &str = "x: { \
    currentSystem = builtins.currentSystem; \
    perSystem = builtins.mapAttrs \
        (system: attrs: builtins.mapAttrs (name: drv: drv.drvPath or null) attrs) \
        x; \
}";

#[derive(Deserialize)]
struct EvalOutput {
    #[serde(rename = "currentSystem")]
    current_system: String,
    #[serde(rename = "perSystem")]
    per_system: HashMap<String, HashMap<String, Option<String>>>,
}

/// The Nix manager's tracked build targets for the current system, from one
/// flake eval.
pub(crate) struct Tracked {
    current_system: String,
    /// `drvPath` -> attribute name, so a built target maps back to the test the
    /// Nix manager will report. Only derivations (non-null `drvPath`) appear.
    drv_to_name: HashMap<String, String>,
}

/// Why resolving a `nix://` archive didn't yield a usable store path.
pub(crate) enum ArchiveError {
    /// The installable is not a target the Nix manager tracks — a bad attr, the
    /// wrong attrset, or a derivation outside `[managers.nix] attrset`. A
    /// misconfiguration: the caller should surface it as a discovery error.
    Untracked(String),
    /// The installable is tracked but its build failed. The Nix manager reports
    /// the failure as its own test (see [`NixBuilder::take_pre_failed`]); the
    /// caller can ignore it and discover nothing from the archive.
    BuildFailed,
    /// The eval that answers "is this tracked?" itself failed.
    Eval(NixError),
}

/// A tracked build target whose Nix build failed while a dependent (Cargo) was
/// resolving it. Recorded so it can be submitted as a terminal failure *before*
/// ranking — otherwise ranking may move the unrun test to `Skipped` and the
/// failure would go unreported.
pub(crate) struct PreFailed {
    pub(crate) name: String,
    pub(crate) manager: api::TestManager,
    pub(crate) stderr: String,
}

pub(crate) struct NixBuilder {
    attrset: String,
    tracked: OnceCell<Tracked>,
    pre_failed: Mutex<Vec<PreFailed>>,
}

impl NixBuilder {
    pub(crate) fn new(attrset: String) -> Self {
        Self {
            attrset,
            tracked: OnceCell::new(),
            pre_failed: Mutex::new(Vec::new()),
        }
    }

    /// Evaluates the tracked target set once and caches it. `OnceCell` makes the
    /// eval race-safe: concurrent callers share the single eval rather than
    /// racing to run it, so a build never observes a half-populated set.
    async fn tracked(&self) -> Result<&Tracked, NixError> {
        self.tracked
            .get_or_try_init(|| eval_tracked(&self.attrset))
            .await
    }

    /// Resolves a `nix://` installable to the store path of its build output.
    ///
    /// Membership is decided on the eval, by derivation identity, *before*
    /// building — so a tracked target that then fails to build is unambiguously
    /// a build failure, never confused with a missing attribute.
    pub(crate) async fn resolve_archive(&self, installable: &str) -> Result<String, ArchiveError> {
        let tracked = self.tracked().await.map_err(ArchiveError::Eval)?;
        let installable = installable.replace(SYSTEM_PLACEHOLDER, &tracked.current_system);

        // Not a derivation (bad attr / wrong attrset): untracked. A broken Nix
        // would already have failed `tracked()` above, so this is a config
        // error, not a transient failure.
        let Some(drv) = eval_drv_path(&installable).await else {
            return Err(ArchiveError::Untracked(installable));
        };
        let Some(name) = tracked.drv_to_name.get(&drv).cloned() else {
            return Err(ArchiveError::Untracked(installable));
        };

        match nix_build_out_path(&format!("{drv}^*")).await {
            Ok(out) => Ok(out),
            Err(stderr) => {
                self.pre_failed.lock().unwrap().push(PreFailed {
                    name,
                    manager: manager_identity(),
                    stderr,
                });
                Err(ArchiveError::BuildFailed)
            }
        }
    }

    /// Drains build targets that failed during resolution so the caller can
    /// submit them as terminal failures before ranking.
    pub(crate) fn take_pre_failed(&self) -> Vec<PreFailed> {
        std::mem::take(&mut self.pre_failed.lock().unwrap())
    }
}

async fn eval_tracked(attrset: &str) -> Result<Tracked, NixError> {
    let flake_ref = format!(".#{attrset}");
    let output = Command::new("nix")
        .args([
            "eval", &flake_ref, "--impure", "--json", "--apply", APPLY_EXPR,
        ])
        .output()
        .await
        .map_err(|e| NixError::EvaluationFailed {
            stderr: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(NixError::EvaluationFailed {
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }
    let parsed: EvalOutput =
        serde_json::from_slice(&output.stdout).map_err(|e| NixError::EvaluationFailed {
            stderr: e.to_string(),
        })?;
    Ok(build_tracked(parsed))
}

/// Reduces the parsed eval to the current-system `drvPath` -> name map. Pure, so
/// the reduction is testable without invoking Nix.
fn build_tracked(mut parsed: EvalOutput) -> Tracked {
    let names = parsed
        .per_system
        .remove(&parsed.current_system)
        .unwrap_or_default();
    let drv_to_name = names
        .into_iter()
        .filter_map(|(name, drv)| drv.map(|drv| (drv, name)))
        .collect();
    Tracked {
        current_system: parsed.current_system,
        drv_to_name,
    }
}

/// Evaluates `<installable>.drvPath`. `None` means the installable doesn't
/// evaluate to a derivation, which the caller reads as "not tracked".
async fn eval_drv_path(installable: &str) -> Option<String> {
    let output = Command::new("nix")
        .args([
            "eval",
            "--impure",
            "--raw",
            &format!("{installable}.drvPath"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let drv = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!drv.is_empty()).then_some(drv)
}

async fn nix_build_out_path(target: &str) -> Result<String, String> {
    let output = Command::new("nix")
        .args(["build", target, "--no-link", "--print-out-paths"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("running nix build: {e} (is `nix` installed?)"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next_back()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "nix build produced no output path".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(current: &str, entries: &[(&str, &str, Option<&str>)]) -> EvalOutput {
        let mut per_system: HashMap<String, HashMap<String, Option<String>>> = HashMap::new();
        for (system, name, drv) in entries {
            per_system
                .entry(system.to_string())
                .or_default()
                .insert(name.to_string(), drv.map(str::to_string));
        }
        EvalOutput {
            current_system: current.to_string(),
            per_system,
        }
    }

    #[test]
    fn tracked_keeps_current_system_derivations_only() {
        let tracked = build_tracked(eval(
            "x86_64-linux",
            &[
                ("x86_64-linux", "archive", Some("/nix/store/a.drv")),
                // Non-derivation on the current system: excluded.
                ("x86_64-linux", "readme", None),
                // Other system: excluded.
                ("aarch64-linux", "archive", Some("/nix/store/b.drv")),
            ],
        ));
        assert_eq!(tracked.current_system, "x86_64-linux");
        assert_eq!(
            tracked
                .drv_to_name
                .get("/nix/store/a.drv")
                .map(String::as_str),
            Some("archive")
        );
        assert!(!tracked.drv_to_name.contains_key("/nix/store/b.drv"));
        assert_eq!(tracked.drv_to_name.len(), 1);
    }

    #[test]
    fn tracked_membership_is_by_drv_identity() {
        // Two attrs aliasing one derivation: last write wins for the name, but
        // the drv is tracked either way — which is what membership turns on.
        let tracked = build_tracked(eval(
            "x86_64-linux",
            &[("x86_64-linux", "archive", Some("/nix/store/a.drv"))],
        ));
        assert!(tracked.drv_to_name.contains_key("/nix/store/a.drv"));
        assert!(!tracked.drv_to_name.contains_key("/nix/store/missing.drv"));
    }
}
