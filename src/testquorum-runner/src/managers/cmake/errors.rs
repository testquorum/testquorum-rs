use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum CmakeError {
    #[error("cmake not found on PATH")]
    NotFound,
    #[error("cmake version {found} is too old (minimum {minimum})")]
    TooOld { minimum: String, found: String },
    #[error("CMakeLists.txt not found at {path}")]
    CmakeListsNotFound { path: String },
}
