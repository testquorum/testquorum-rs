use super::MavenError;

pub(crate) fn detect_maven(pom_path: Option<&str>) -> Result<String, MavenError> {
    let java_path = which::which("java").map_err(|_| MavenError::JavaNotFound)?;
    which::which("mvn").map_err(|_| MavenError::MvnNotFound)?;

    // java prints version to stderr, not stdout
    let output = std::process::Command::new(&java_path)
        .arg("-version")
        .output()
        .map_err(|_| MavenError::JavaNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stderr);
    let version = parse_version(&version_str).ok_or_else(|| MavenError::JavaTooOld {
        minimum: "17".to_string(),
        found: version_str.trim().to_string(),
    })?;

    if version < 17 {
        return Err(MavenError::JavaTooOld {
            minimum: "17".to_string(),
            found: format!("{}", version),
        });
    }

    let resolved = pom_path.unwrap_or("pom.xml");
    if !std::path::Path::new(resolved).exists() {
        return Err(MavenError::PomNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<u32> {
    // Find the quoted version string in output like:
    //   openjdk version "21.0.1" 2023-10-17
    //   java version "17.0.1" 2021-10-19 LTS
    //   java version "1.8.0_392"
    let start = output.find('"')?;
    let rest = &output[start + 1..];
    let end = rest.find('"')?;
    let version_str = &rest[..end];

    let mut parts = version_str.split('.');
    let first = parts.next()?;

    if first == "1" {
        // Legacy format: "1.8.0_392" → major 8
        parts.next()?.parse().ok()
    } else {
        first.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_modern_21() {
        assert_eq!(
            parse_version("openjdk version \"21.0.1\" 2023-10-17\n..."),
            Some(21)
        );
    }

    #[test]
    fn parse_version_modern_17() {
        assert_eq!(
            parse_version("java version \"17.0.1\" 2021-10-19 LTS\n..."),
            Some(17)
        );
    }

    #[test]
    fn parse_version_legacy_8() {
        assert_eq!(
            parse_version("java version \"1.8.0_392\"\n..."),
            Some(8)
        );
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
