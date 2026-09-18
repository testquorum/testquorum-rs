use super::Buck2Error;

pub(crate) fn detect_buck2(buckconfig_path: Option<&str>) -> Result<String, Buck2Error> {
    which::which("buck2").map_err(|_| Buck2Error::NotFound)?;

    let resolved = buckconfig_path.unwrap_or(".buckconfig");
    if !std::path::Path::new(resolved).exists() {
        return Err(Buck2Error::BuckconfigNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}
