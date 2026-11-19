use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum Buck2Error {
    #[error("buck2 not found on PATH")]
    NotFound,
    #[error(".buckconfig not found at {path}")]
    BuckconfigNotFound { path: String },
}
