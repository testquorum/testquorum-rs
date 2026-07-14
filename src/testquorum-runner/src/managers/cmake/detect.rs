use super::CmakeError;

pub(crate) fn detect_cmake(cmake_lists_path: Option<&str>) -> Result<String, CmakeError> {
    which::which("cmake").map_err(|_| CmakeError::NotFound)?;

    let resolved = cmake_lists_path.unwrap_or("CMakeLists.txt");
    if !std::path::Path::new(resolved).exists() {
        return Err(CmakeError::CmakeListsNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}
