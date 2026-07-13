use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum DotnetError {
    #[error("dotnet not found on PATH")]
    DotnetNotFound,
    #[error("dotnet version {found} is too old (minimum {minimum})")]
    DotnetTooOld { minimum: String, found: String },
    #[error("no .sln or .csproj file found")]
    ProjectNotFound,
    #[error("project file not found at {path}")]
    ProjectFileNotFound { path: String },
}
