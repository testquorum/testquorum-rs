use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum NixError {
    #[error("nix not found on PATH")]
    NixNotFound,
    #[error("nix version {found} is too old (minimum {minimum})")]
    NixTooOld { minimum: String, found: String },
    #[error("evaluation failed: {stderr}")]
    EvaluationFailed { stderr: String },
}
