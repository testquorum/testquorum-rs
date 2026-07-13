use super::errors::SwiftError;

pub(crate) fn detect_swift(package_path: Option<&str>) -> Result<String, SwiftError> {
    which::which("swift").map_err(|_| SwiftError::SwiftNotFound)?;

    let output = std::process::Command::new("swift")
        .arg("--version")
        .output()
        .map_err(|_| SwiftError::SwiftNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);

    let (major, minor) = parse_version(&version_str).ok_or_else(|| SwiftError::SwiftTooOld {
        minimum: "5.9".to_string(),
        found: version_str.trim().to_string(),
    })?;

    if (major, minor) < (5, 9) {
        return Err(SwiftError::SwiftTooOld {
            minimum: "5.9".to_string(),
            found: format!("{}.{}", major, minor),
        });
    }

    let resolved = package_path.unwrap_or("Package.swift");
    if !std::path::Path::new(resolved).exists() {
        return Err(SwiftError::PackageNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // Look for "Swift version X.Y" in the output.
    let prefix = "Swift version ";
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
    fn parse_version_standard() {
        assert_eq!(
            parse_version("Apple Swift version 5.9.2 (swiftlang-...)"),
            Some((5, 9))
        );
    }

    #[test]
    fn parse_version_minor_two_digits() {
        assert_eq!(
            parse_version("Apple Swift version 5.10 (swiftlang-...)"),
            Some((5, 10))
        );
    }

    #[test]
    fn parse_version_major_six() {
        assert_eq!(
            parse_version("Apple Swift version 6.0 (swiftlang-...)"),
            Some((6, 0))
        );
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
