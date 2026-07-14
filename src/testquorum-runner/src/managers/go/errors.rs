use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum GoError {
    #[error("go not found on PATH")]
    NotFound,
    #[error("go version {found} is too old (minimum {minimum})")]
    TooOld { minimum: String, found: String },
    #[error("go.mod not found at {path}")]
    ModNotFound { path: String },
}
