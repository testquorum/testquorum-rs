use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum RError {
    #[error("Rscript not found on PATH")]
    NotFound,
    #[error("R version {found} is too old (minimum {minimum})")]
    TooOld { minimum: String, found: String },
    #[error("DESCRIPTION file not found at {path}")]
    DescriptionNotFound { path: String },
}
