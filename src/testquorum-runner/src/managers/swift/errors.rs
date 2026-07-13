use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum SwiftError {
    #[error("swift not found on PATH")]
    SwiftNotFound,
    #[error("swift version {found} is too old (minimum {minimum})")]
    SwiftTooOld { minimum: String, found: String },
    #[error("Package.swift not found at {path}")]
    PackageNotFound { path: String },
}
