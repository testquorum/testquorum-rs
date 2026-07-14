use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum SbtError {
    #[error("sbt not found on PATH")]
    NotFound,
    #[error("build.sbt not found at {path}")]
    BuildFileNotFound { path: String },
}
