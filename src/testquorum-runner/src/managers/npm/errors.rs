use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum NpmError {
    #[error("node not found on PATH")]
    NodeNotFound,
    #[error("npm not found on PATH")]
    NpmNotFound,
    #[error("node version {found} is too old (minimum {minimum})")]
    NodeTooOld { minimum: String, found: String },
    #[error("package.json not found at {path}")]
    PackageJsonNotFound { path: String },
    #[error("package.json has no test script")]
    NoTestScript,
}
