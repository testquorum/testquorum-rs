use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum CmakeError {
    #[error("cmake not found on PATH")]
    NotFound,
    #[error("CMakeLists.txt not found at {path}")]
    CmakeListsNotFound { path: String },
}
