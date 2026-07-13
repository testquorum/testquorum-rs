use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum RspecError {
    #[error("ruby not found on PATH")]
    RubyNotFound,
    #[error("ruby version {found} is too old (minimum {minimum})")]
    RubyTooOld { minimum: String, found: String },
    #[error("bundle not found on PATH")]
    BundleNotFound,
    #[error("Gemfile not found at {path}")]
    GemfileNotFound { path: String },
}
