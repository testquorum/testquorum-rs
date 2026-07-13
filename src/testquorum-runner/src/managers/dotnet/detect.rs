use super::DotnetError;

pub(crate) fn detect_dotnet(project_path: Option<&str>) -> Result<String, DotnetError> {
    which::which("dotnet").map_err(|_| DotnetError::DotnetNotFound)?;

    let output = std::process::Command::new("dotnet")
        .arg("--version")
        .output()
        .map_err(|_| DotnetError::DotnetNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version = parse_major_version(&version_str).ok_or_else(|| DotnetError::DotnetTooOld {
        minimum: "6".to_string(),
        found: version_str.trim().to_string(),
    })?;

    if version < 6 {
        return Err(DotnetError::DotnetTooOld {
            minimum: "6".to_string(),
            found: format!("{}", version),
        });
    }

    if let Some(path) = project_path {
        if !std::path::Path::new(path).exists() {
            return Err(DotnetError::ProjectFileNotFound {
                path: path.to_string(),
            });
        }
        return Ok(path.to_string());
    }

    let entries = std::fs::read_dir(".").map_err(|_| DotnetError::ProjectNotFound)?;
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.ends_with(".sln") || s.ends_with(".csproj") {
            return Ok(s.into_owned());
        }
    }

    Err(DotnetError::ProjectNotFound)
}

fn parse_major_version(output: &str) -> Option<u32> {
    // Parse "8.0.100\n" -> 8.
    output.trim().split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_standard() {
        assert_eq!(parse_major_version("8.0.100\n"), Some(8));
    }

    #[test]
    fn parse_version_no_trailing_newline() {
        assert_eq!(parse_major_version("6.0.419"), Some(6));
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(parse_major_version("3.1.32"), Some(3));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_major_version("not a version"), None);
    }
}
