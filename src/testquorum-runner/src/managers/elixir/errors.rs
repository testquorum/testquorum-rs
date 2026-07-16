use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum ElixirError {
    #[error("mix not found on PATH")]
    NotFound,
    #[error("Elixir version {found} is too old (minimum {minimum})")]
    TooOld { minimum: String, found: String },
    #[error("mix.exs not found at {path}")]
    MixExsNotFound { path: String },
}
