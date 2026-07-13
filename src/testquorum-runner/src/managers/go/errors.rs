use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum GoError {
    #[error("go not found on PATH")]
    GoNotFound,
    #[error("go version {found} is too old (minimum {minimum})")]
    GoTooOld { minimum: String, found: String },
    #[error("go.mod not found at {path}")]
    GoModNotFound { path: String },
}
