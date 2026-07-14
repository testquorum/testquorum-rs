use super::SbtError;

pub(crate) fn detect_sbt(build_sbt_path: Option<&str>) -> Result<String, SbtError> {
    which::which("sbt").map_err(|_| SbtError::NotFound)?;

    let resolved = build_sbt_path.unwrap_or("build.sbt");
    if !std::path::Path::new(resolved).exists() {
        return Err(SbtError::BuildFileNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}
