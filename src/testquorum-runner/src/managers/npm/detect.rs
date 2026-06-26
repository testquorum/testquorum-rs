use super::NpmError;

pub(crate) fn detect_npm(package_json_path: Option<&str>) -> Result<String, NpmError> {
    let node_path = which::which("node").map_err(|_| NpmError::NodeNotFound)?;
    which::which("npm").map_err(|_| NpmError::NpmNotFound)?;

    let output = std::process::Command::new(&node_path)
        .arg("--version")
        .output()
        .map_err(|_| NpmError::NodeNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version = parse_version(&version_str).ok_or_else(|| NpmError::NodeTooOld {
        minimum: "18".to_string(),
        found: version_str.trim().to_string(),
    })?;

    if version < 18 {
        return Err(NpmError::NodeTooOld {
            minimum: "18".to_string(),
            found: format!("{}", version),
        });
    }

    let resolved = package_json_path.unwrap_or("package.json");
    if !std::path::Path::new(resolved).exists() {
        return Err(NpmError::PackageJsonNotFound {
            path: resolved.to_string(),
        });
    }

    let content = std::fs::read_to_string(resolved).map_err(|_| NpmError::PackageJsonNotFound {
        path: resolved.to_string(),
    })?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|_| NpmError::NoTestScript)?;

    if json["scripts"]["test"].as_str().is_none() {
        return Err(NpmError::NoTestScript);
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<u32> {
    // Parse "v18.19.0" -> 18.
    let trimmed = output.trim().trim_start_matches('v');
    trimmed.split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_standard() {
        assert_eq!(parse_version("v18.19.0\n"), Some(18));
    }

    #[test]
    fn parse_version_major_only() {
        assert_eq!(parse_version("v20"), Some(20));
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(parse_version("v16.20.1"), Some(16));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
