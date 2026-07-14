use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum KotlinError {
    #[error("java not found on PATH")]
    JavaNotFound,
    #[error("./gradlew not found — is this a Gradle project?")]
    GradlewNotFound,
    #[error("build.gradle.kts (or build.gradle) not found at {path}")]
    BuildFileMissing { path: String },
}
