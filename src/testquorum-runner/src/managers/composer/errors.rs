use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum ComposerError {
    #[error("php not found on PATH")]
    PhpNotFound,
    #[error("php version {found} is too old (minimum {minimum})")]
    PhpTooOld { minimum: String, found: String },
    #[error("composer not found on PATH")]
    ComposerNotFound,
    #[error("composer.json not found at {path}")]
    ComposerJsonNotFound { path: String },
    #[error("composer.json has no test script")]
    NoTestScript,
}
