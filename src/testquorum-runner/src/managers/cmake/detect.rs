use super::CmakeError;

pub(crate) fn detect_cmake(cmake_lists_path: Option<&str>) -> Result<String, CmakeError> {
    let cmake_path = which::which("cmake").map_err(|_| CmakeError::NotFound)?;

    let output = std::process::Command::new(&cmake_path)
        .arg("--version")
        .output()
        .map_err(|_| CmakeError::NotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);

    let (major, minor) = parse_version(&version_str).ok_or_else(|| CmakeError::TooOld {
        minimum: "3.13".to_string(),
        found: version_str.lines().next().unwrap_or("").trim().to_string(),
    })?;

    if (major, minor) < (3, 13) {
        return Err(CmakeError::TooOld {
            minimum: "3.13".to_string(),
            found: format!("{}.{}", major, minor),
        });
    }

    let resolved = cmake_lists_path.unwrap_or("CMakeLists.txt");
    if !std::path::Path::new(resolved).exists() {
        return Err(CmakeError::CmakeListsNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // "cmake version 3.20.0"
    let prefix = "cmake version ";
    let idx = output.find(prefix)?;
    let rest = &output[idx + prefix.len()..];
    let version_part = rest.split_whitespace().next()?;
    let mut parts = version_part.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_modern() {
        assert_eq!(
            parse_version(
                "cmake version 3.20.0\n\nCMake suite maintained and supported by Kitware (kitware.com/cmake)."
            ),
            Some((3, 20))
        );
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(parse_version("cmake version 3.10.2"), Some((3, 10)));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a cmake version"), None);
    }
}
