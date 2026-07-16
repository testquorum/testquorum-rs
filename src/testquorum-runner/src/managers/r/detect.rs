use super::RError;

pub(crate) fn detect_r(description_path: Option<&str>) -> Result<String, RError> {
    let rscript_path = which::which("Rscript").map_err(|_| RError::NotFound)?;

    let output = std::process::Command::new(&rscript_path)
        .arg("--version")
        .output()
        .map_err(|_| RError::NotFound)?;

    // Rscript --version writes to stderr
    let version_str = String::from_utf8_lossy(&output.stderr).to_string()
        + &String::from_utf8_lossy(&output.stdout);

    let (major, minor) = parse_version(&version_str).ok_or_else(|| RError::TooOld {
        minimum: "4.0".to_string(),
        found: version_str.trim().lines().next().unwrap_or("").trim().to_string(),
    })?;

    if (major, minor) < (4, 0) {
        return Err(RError::TooOld {
            minimum: "4.0".to_string(),
            found: format!("{}.{}", major, minor),
        });
    }

    let resolved = description_path.unwrap_or("DESCRIPTION");
    if !std::path::Path::new(resolved).exists() {
        return Err(RError::DescriptionNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // "R scripting front-end version 4.3.1 (2023-06-16)"
    let prefix = "R scripting front-end version ";
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
            parse_version("R scripting front-end version 4.3.1 (2023-06-16)"),
            Some((4, 3))
        );
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(
            parse_version("R scripting front-end version 3.6.3 (2020-02-29)"),
            Some((3, 6))
        );
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
