use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum CargoError {
    #[error("cargo not found on PATH")]
    CargoNotFound,
    #[error("cargo version {found} is too old (minimum {minimum})")]
    CargoTooOld { minimum: String, found: String },
    #[error("cargo metadata failed: {stderr}")]
    MetadataFailed { stderr: String },
    #[error("cargo test --no-run failed: {stderr}")]
    CompilationFailed { stderr: String },
    #[error("cargo nextest failed: {stderr}")]
    NextestFailed { stderr: String },
    #[error("nextest_archive is set but cargo-nextest was not found on PATH")]
    NextestArchiveNoNextest,
    #[error("failed to prepare nextest archive {path}: {reason}")]
    ArchivePrepFailed { path: String, reason: String },
    #[error("cargo manifest not found at {path}")]
    ManifestNotFound { path: String },
}
