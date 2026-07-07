//! The `cargo nextest` execution backend for the Cargo manager.
//!
//! nextest is wired in as an alternate backend rather than a separate manager:
//! discovery produces the exact same `{package}::{test_name}` names as the
//! plain `cargo test` path (verified against `cargo nextest list`'s
//! `package-name` + testcase key), so the two are fully interchangeable on the
//! wire. Only unit/integration tests run through here — nextest cannot execute
//! doctests, so those stay on `cargo test --doc` in the parent module.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use serde::Deserialize;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
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
    /// Use a prebuilt nextest archive. `archive_file` is a `.tar.zst` ready
    /// for `--archive-file` (staged by [`prepare_archive`]); `workspace_remap`
    /// is the workspace root the archive's paths are remapped onto. `_guard`
    /// keeps the staging tempdir alive for as long as any clone of this source
    /// exists — it is cloned into each per-package run task.
    Archive {
        archive_file: PathBuf,
        workspace_remap: String,
        _guard: Option<Arc<TempDir>>,
    },
}

impl NextestSource {
    /// Flags that point `cargo nextest list`/`run` at this source's binaries.
    fn source_args(&self) -> Vec<String> {
        match self {
            NextestSource::Local { manifest_path } => {
                vec!["--manifest-path".to_string(), manifest_path.clone()]
            }
            NextestSource::Archive {
                archive_file,
                workspace_remap,
                ..
            } => vec![
                "--archive-file".to_string(),
                archive_file.to_string_lossy().into_owned(),
                "--workspace-remap".to_string(),
                workspace_remap.clone(),
            ],
        }
    }
}

/// Magic-byte offset/value identifying a POSIX (`ustar`) tar header. nextest
/// only accepts `.tar.zst`, so an uncompressed tar must be recompressed before
/// use; anything else is assumed to already be compressed.
const TAR_MAGIC_OFFSET: usize = 257;
const TAR_MAGIC: &[u8] = b"ustar";

/// Detects an uncompressed tar from its header bytes. Returns false for short
/// reads (an empty/truncated file isn't a usable tar anyway) and for compressed
/// archives, whose magic lives at offset 0.
fn is_uncompressed_tar(header: &[u8]) -> bool {
    header
        .get(TAR_MAGIC_OFFSET..TAR_MAGIC_OFFSET + TAR_MAGIC.len())
        .is_some_and(|m| m == TAR_MAGIC)
}

/// Stages a configured archive path into a form nextest accepts (a file named
/// `*.tar.zst`), detecting the input format from its contents:
///
/// - An uncompressed tar is recompressed with `zstd` into a tempdir.
/// - An already-compressed archive is used directly when it is already named
///   `*.tar.zst`, otherwise symlinked into a tempdir under that name.
///
/// The returned [`NextestSource::Archive`] owns any tempdir via its guard.
pub(crate) async fn prepare_archive(
    raw_path: &str,
    workspace_remap: String,
) -> Result<NextestSource, CargoError> {
    let prep_err = |reason: String| CargoError::ArchivePrepFailed {
        path: raw_path.to_string(),
        reason,
    };

    let mut header = [0u8; TAR_MAGIC_OFFSET + TAR_MAGIC.len()];
    let mut file = tokio::fs::File::open(raw_path)
        .await
        .map_err(|e| prep_err(e.to_string()))?;
    let n = file
        .read(&mut header)
        .await
        .map_err(|e| prep_err(e.to_string()))?;

    if is_uncompressed_tar(&header[..n]) {
        let dir = TempDir::new().map_err(|e| prep_err(e.to_string()))?;
        let out = dir.path().join("archive.tar.zst");
        let status = Command::new("zstd")
            .args(["-q", "-f", raw_path, "-o"])
            .arg(&out)
            .status()
            .await
            .map_err(|e| prep_err(format!("running zstd: {} (is `zstd` installed?)", e)))?;
        if !status.success() {
            return Err(prep_err(format!("zstd exited with {}", status)));
        }
        return Ok(NextestSource::Archive {
            archive_file: out,
            workspace_remap,
            _guard: Some(Arc::new(dir)),
        });
    }

    // Already compressed. nextest keys off the filename, so it must end in
    // `.tar.zst`; reuse the path as-is when it does, otherwise symlink it under
    // a conforming name so we never copy a large archive.
    if raw_path.ends_with(".tar.zst") {
        return Ok(NextestSource::Archive {
            archive_file: PathBuf::from(raw_path),
            workspace_remap,
            _guard: None,
        });
    }

    let dir = TempDir::new().map_err(|e| prep_err(e.to_string()))?;
    let link = dir.path().join("archive.tar.zst");
    let target = std::fs::canonicalize(raw_path).map_err(|e| prep_err(e.to_string()))?;
    symlink(&target, &link).map_err(|e| prep_err(e.to_string()))?;
    Ok(NextestSource::Archive {
        archive_file: link,
        workspace_remap,
        _guard: Some(Arc::new(dir)),
    })
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    // Non-unix: fall back to a copy so the staged name still ends in .tar.zst.
    std::fs::copy(target, link).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_with_magic_at(offset: usize) -> Vec<u8> {
        let mut h = vec![0u8; offset + TAR_MAGIC.len()];
        h[offset..offset + TAR_MAGIC.len()].copy_from_slice(TAR_MAGIC);
        h
    }

    #[test]
    fn detects_ustar_header() {
        assert!(is_uncompressed_tar(&header_with_magic_at(TAR_MAGIC_OFFSET)));
    }

    #[test]
    fn rejects_zstd_magic() {
        // zstd frame magic at offset 0, nothing at the tar offset.
        let mut h = vec![0u8; 512];
        h[..4].copy_from_slice(&[0x28, 0xb5, 0x2f, 0xfd]);
        assert!(!is_uncompressed_tar(&h));
    }

    #[test]
    fn rejects_short_read() {
        assert!(!is_uncompressed_tar(&[]));
        assert!(!is_uncompressed_tar(b"ustar"));
    }

    #[test]
    fn archive_source_args_pass_archive_and_remap() {
        let source = NextestSource::Archive {
            archive_file: PathBuf::from("/tmp/x/archive.tar.zst"),
            workspace_remap: ".".to_string(),
            _guard: None,
        };
        assert_eq!(
            source.source_args(),
            vec![
                "--archive-file".to_string(),
                "/tmp/x/archive.tar.zst".to_string(),
                "--workspace-remap".to_string(),
                ".".to_string(),
            ]
        );
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
