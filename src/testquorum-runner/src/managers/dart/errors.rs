use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum DartError {
    #[error("dart not found on PATH")]
    NotFound,
    #[error("pubspec.yaml not found at {path}")]
    PubspecNotFound { path: String },
}
