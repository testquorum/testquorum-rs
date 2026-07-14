use super::DartError;

pub(crate) fn detect_dart(pubspec_path: Option<&str>) -> Result<String, DartError> {
    which::which("dart").map_err(|_| DartError::NotFound)?;

    let resolved = pubspec_path.unwrap_or("pubspec.yaml");
    if !std::path::Path::new(resolved).exists() {
        return Err(DartError::PubspecNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}
