use super::KotlinError;

pub(crate) fn detect_kotlin(build_file_path: Option<&str>) -> Result<String, KotlinError> {
    which::which("java").map_err(|_| KotlinError::JavaNotFound)?;

    if !std::path::Path::new("./gradlew").exists() {
        return Err(KotlinError::GradlewNotFound);
    }

    if let Some(path) = build_file_path {
        if !std::path::Path::new(path).exists() {
            return Err(KotlinError::BuildFileNotFound {
                path: path.to_string(),
            });
        }
        return Ok(path.to_string());
    }

    if std::path::Path::new("build.gradle.kts").exists() {
        return Ok("build.gradle.kts".to_string());
    }

    if std::path::Path::new("build.gradle").exists() {
        return Ok("build.gradle".to_string());
    }

    Err(KotlinError::BuildFileNotFound {
        path: "build.gradle.kts".to_string(),
    })
}
