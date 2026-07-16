use super::DartError;

pub(crate) fn detect_dart(pubspec_path: Option<&str>) -> Result<String, DartError> {
    let dart_path = which::which("dart").map_err(|_| DartError::NotFound)?;

    let output = std::process::Command::new(&dart_path)
        .arg("--version")
        .output()
        .map_err(|_| DartError::NotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);

    let (major, minor) = parse_version(&version_str).ok_or_else(|| DartError::TooOld {
        minimum: "3.0".to_string(),
        found: version_str
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string(),
    })?;

    if (major, minor) < (3, 0) {
        return Err(DartError::TooOld {
            minimum: "3.0".to_string(),
            found: format!("{}.{}", major, minor),
        });
    }

    let resolved = pubspec_path.unwrap_or("pubspec.yaml");
    if !std::path::Path::new(resolved).exists() {
        return Err(DartError::PubspecNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // "Dart SDK version: 3.1.0 (stable) ..."
    let prefix = "Dart SDK version: ";
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
                "Dart SDK version: 3.1.0 (stable) (Tue Jul 25 21:39:02 2023 +0000) on \"macos_arm64\""
            ),
            Some((3, 1))
        );
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(
            parse_version("Dart SDK version: 2.19.6 (stable) ..."),
            Some((2, 19))
        );
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
