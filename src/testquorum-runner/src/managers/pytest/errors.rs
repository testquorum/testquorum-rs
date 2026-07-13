use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum PytestError {
    #[error("python3 not found on PATH")]
    PythonNotFound,
    #[error("python3 version {found} is too old (minimum {minimum})")]
    PythonTooOld { minimum: String, found: String },
    #[error("pytest not importable by python3")]
    PytestNotFound,
    #[error("no Python project config found at {path}")]
    PyprojectNotFound { path: String },
}
