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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_r_fails_when_description_missing() {
        // A path that will never exist — confirm the error variant is correct.
        let result = detect_r(Some("/nonexistent/DESCRIPTION"));
        assert!(matches!(result, Err(RError::DescriptionNotFound { .. })));
    }
}
