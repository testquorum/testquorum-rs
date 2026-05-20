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
}
