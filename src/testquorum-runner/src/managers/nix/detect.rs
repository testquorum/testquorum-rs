use super::NixError;

pub(crate) fn detect_nix() -> Result<(), NixError> {
    let nix_path = which::which("nix").map_err(|_| NixError::NixNotFound)?;

    let output = std::process::Command::new(&nix_path)
        .arg("--version")
        .output()
        .map_err(|_| NixError::NixNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version = parse_version(&version_str).ok_or_else(|| NixError::NixTooOld {
        minimum: "2.4".to_string(),
        found: version_str.trim().to_string(),
    })?;

    if version < (2, 4) {
        return Err(NixError::NixTooOld {
            minimum: "2.4".to_string(),
            found: format!("{}.{}", version.0, version.1),
        });
    }

    Ok(())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // Parse "nix (Nix) 2.24.9" -> (2, 24).
    let version_part = output.split_whitespace().last()?;
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
        assert_eq!(parse_version("nix (Nix) 2.24.9"), Some((2, 24)));
    }

    #[test]
    fn parse_version_short() {
        assert_eq!(parse_version("nix (Nix) 2.4"), Some((2, 4)));
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(parse_version("nix (Nix) 2.3.16"), Some((2, 3)));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
