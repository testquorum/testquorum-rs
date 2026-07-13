use super::errors::ComposerError;

pub(crate) fn detect_composer(composer_json_path: Option<&str>) -> Result<String, ComposerError> {
    let php_path = which::which("php").map_err(|_| ComposerError::PhpNotFound)?;

    let output = std::process::Command::new(&php_path)
        .arg("--version")
        .output()
        .map_err(|_| ComposerError::PhpNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version = parse_php_version(&version_str).ok_or_else(|| ComposerError::PhpTooOld {
        minimum: "8.0".to_string(),
        found: version_str.lines().next().unwrap_or("").trim().to_string(),
    })?;

    if version < (8, 0) {
        return Err(ComposerError::PhpTooOld {
            minimum: "8.0".to_string(),
            found: format!("{}.{}", version.0, version.1),
        });
    }

    which::which("composer").map_err(|_| ComposerError::ComposerNotFound)?;

    let resolved = composer_json_path.unwrap_or("composer.json");
    if !std::path::Path::new(resolved).exists() {
        return Err(ComposerError::ComposerJsonNotFound {
            path: resolved.to_string(),
        });
    }

    let content =
        std::fs::read_to_string(resolved).map_err(|_| ComposerError::ComposerJsonNotFound {
            path: resolved.to_string(),
        })?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|_| ComposerError::NoTestScript)?;

    if json["scripts"]["test"].as_str().is_none() {
        return Err(ComposerError::NoTestScript);
    }

    Ok(resolved.to_string())
}

/// Parse the PHP version string into `(major, minor)`.
/// Input: `"PHP 8.3.0 (cli) (built: ...)\n..."` → `Some((8, 3))`
fn parse_php_version(output: &str) -> Option<(u32, u32)> {
    // First line, second token is the version number, e.g. "8.3.0"
    let first_line = output.lines().next()?;
    let version_token = first_line.split_whitespace().nth(1)?;
    let mut parts = version_token.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_php_version_standard() {
        assert_eq!(
            parse_php_version("PHP 8.3.0 (cli) (built: Jan  1 2024 00:00:00)\nCopyright ..."),
            Some((8, 3))
        );
    }

    #[test]
    fn parse_php_version_older_minor() {
        assert_eq!(parse_php_version("PHP 8.1.27 (cli) ..."), Some((8, 1)));
    }

    #[test]
    fn parse_php_version_old_major() {
        assert_eq!(parse_php_version("PHP 7.4.33 (cli) ..."), Some((7, 4)));
    }

    #[test]
    fn parse_php_version_invalid() {
        assert_eq!(parse_php_version("not a version"), None);
    }
}
