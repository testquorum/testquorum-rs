use super::SbtError;

pub(crate) fn detect_sbt(build_sbt_path: Option<&str>) -> Result<String, SbtError> {
    let sbt_path = which::which("sbt").map_err(|_| SbtError::NotFound)?;

    let output = std::process::Command::new(&sbt_path)
        .arg("--numeric-version")
        .output()
        .map_err(|_| SbtError::NotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);

    let (major, minor) = parse_version(&version_str).ok_or_else(|| SbtError::TooOld {
        minimum: "1.0".to_string(),
        found: version_str
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string(),
    })?;

    if (major, minor) < (1, 0) {
        return Err(SbtError::TooOld {
            minimum: "1.0".to_string(),
            found: format!("{}.{}", major, minor),
        });
    }

    let resolved = build_sbt_path.unwrap_or("build.sbt");
    if !std::path::Path::new(resolved).exists() {
        return Err(SbtError::BuildFileNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // "--numeric-version" outputs "1.9.6\n"
    let version_str = output.trim().lines().next()?.trim();
    let mut parts = version_str.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_modern() {
        assert_eq!(parse_version("1.9.6\n"), Some((1, 9)));
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(parse_version("0.13.18\n"), Some((0, 13)));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
