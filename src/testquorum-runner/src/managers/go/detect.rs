use super::GoError;

pub(crate) fn detect_go(go_mod_path: Option<&str>) -> Result<String, GoError> {
    which::which("go").map_err(|_| GoError::GoNotFound)?;

    let output = std::process::Command::new("go")
        .arg("version")
        .output()
        .map_err(|_| GoError::GoNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version = parse_version(&version_str).ok_or_else(|| GoError::GoTooOld {
        minimum: "1.21".to_string(),
        found: version_str.trim().to_string(),
    })?;

    if version < (1, 21) {
        return Err(GoError::GoTooOld {
            minimum: "1.21".to_string(),
            found: format!("{}.{}", version.0, version.1),
        });
    }

    let resolved = go_mod_path.unwrap_or("go.mod");
    if !std::path::Path::new(resolved).exists() {
        return Err(GoError::GoModNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // Parse "go version go1.21.0 darwin/amd64" -> (1, 21)
    let trimmed = output.trim();
    let go_part = trimmed.strip_prefix("go version go")?;
    let version_part = go_part.split_whitespace().next()?;
    let mut parts = version_part.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_linux_amd64() {
        assert_eq!(
            parse_version("go version go1.21.0 linux/amd64"),
            Some((1, 21))
        );
    }

    #[test]
    fn parse_version_darwin_arm64() {
        assert_eq!(
            parse_version("go version go1.22.3 darwin/arm64"),
            Some((1, 22))
        );
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(
            parse_version("go version go1.20.14 linux/amd64"),
            Some((1, 20))
        );
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
