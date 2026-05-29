use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum TreefmtError {
    #[error("treefmt not found on PATH")]
    TreefmtNotFound,
    #[error("treefmt version {found} is too old (minimum {minimum})")]
    TreefmtTooOld { minimum: String, found: String },
}
