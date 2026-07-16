use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum OcamlError {
    #[error("opam not found on PATH")]
    OpamNotFound,
    #[error("opam version {found} is too old (minimum {minimum})")]
    TooOld { minimum: String, found: String },
    #[error("dune-project not found at {path}")]
    DuneProjectNotFound { path: String },
}
