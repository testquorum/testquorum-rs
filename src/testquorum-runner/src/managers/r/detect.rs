use super::RError;

pub(crate) fn detect_r(description_path: Option<&str>) -> Result<String, RError> {
    which::which("Rscript").map_err(|_| RError::NotFound)?;

    let resolved = description_path.unwrap_or("DESCRIPTION");
    if !std::path::Path::new(resolved).exists() {
        return Err(RError::DescriptionNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}
