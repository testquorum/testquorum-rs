use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum MavenError {
    #[error("java not found on PATH")]
    JavaNotFound,
    #[error("java version {found} is too old (minimum {minimum})")]
    JavaTooOld { minimum: String, found: String },
    #[error("mvn not found on PATH")]
    MvnNotFound,
    #[error("pom.xml not found at {path}")]
    PomNotFound { path: String },
}
