use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum RError {
    #[error("Rscript not found on PATH")]
    NotFound,
    #[error("DESCRIPTION file not found at {path}")]
    DescriptionNotFound { path: String },
}
