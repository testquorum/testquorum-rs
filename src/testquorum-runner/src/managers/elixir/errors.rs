use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum ElixirError {
    #[error("mix not found on PATH")]
    NotFound,
    #[error("mix.exs not found at {path}")]
    MixExsNotFound { path: String },
}
