use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum DartError {
    #[error("dart not found on PATH")]
    NotFound,
    #[error("dart version {found} is too old (minimum {minimum})")]
    TooOld { minimum: String, found: String },
    #[error("pubspec.yaml not found at {path}")]
    PubspecNotFound { path: String },
}
