use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum OcamlError {
    #[error("opam not found on PATH")]
    OpamNotFound,
    #[error("dune-project not found at {path}")]
    DuneProjectNotFound { path: String },
}
