use super::TreefmtError;

pub(crate) fn detect_treefmt() -> Result<(), TreefmtError> {
    let treefmt_path = which::which("treefmt").map_err(|_| TreefmtError::TreefmtNotFound)?;

    let output = std::process::Command::new(&treefmt_path)
        .arg("--version")
        .output()
        .map_err(|_| TreefmtError::TreefmtNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version = parse_version(&version_str).ok_or_else(|| TreefmtError::TreefmtTooOld {
        minimum: "2.0".to_string(),
        found: version_str.trim().to_string(),
    })?;

    if version < (2, 0) {
        return Err(TreefmtError::TreefmtTooOld {
            minimum: "2.0".to_string(),
            found: format!("{}.{}", version.0, version.1),
        });
    }

    Ok(())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // Parse "treefmt v2.5.0" -> (2, 5).
    let version_part = output.split_whitespace().nth(1)?;
    let version_part = version_part.strip_prefix('v').unwrap_or(version_part);
    let mut parts = version_part.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_standard() {
        assert_eq!(parse_version("treefmt v2.5.0"), Some((2, 5)));
    }

    #[test]
    fn parse_version_short() {
        assert_eq!(parse_version("treefmt v2.0"), Some((2, 0)));
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(parse_version("treefmt v1.99.0"), Some((1, 99)));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
