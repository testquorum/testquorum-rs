use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum SbtError {
    #[error("sbt not found on PATH")]
    NotFound,
    #[error("sbt version {found} is too old (minimum {minimum})")]
    TooOld { minimum: String, found: String },
    #[error("build.sbt not found at {path}")]
    BuildFileNotFound { path: String },
}
