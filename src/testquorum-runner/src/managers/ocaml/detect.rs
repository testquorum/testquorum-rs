use super::OcamlError;

pub(crate) fn detect_ocaml(dune_project_path: Option<&str>) -> Result<String, OcamlError> {
    let opam_path = which::which("opam").map_err(|_| OcamlError::OpamNotFound)?;

    let output = std::process::Command::new(&opam_path)
        .arg("--version")
        .output()
        .map_err(|_| OcamlError::OpamNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);

    let (major, minor) = parse_version(&version_str).ok_or_else(|| OcamlError::TooOld {
        minimum: "2.0".to_string(),
        found: version_str
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string(),
    })?;

    if (major, minor) < (2, 0) {
        return Err(OcamlError::TooOld {
            minimum: "2.0".to_string(),
            found: format!("{}.{}", major, minor),
        });
    }

    let resolved = dune_project_path.unwrap_or("dune-project");
    if !std::path::Path::new(resolved).exists() {
        return Err(OcamlError::DuneProjectNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // "opam --version" outputs "2.1.5\n"
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
        assert_eq!(parse_version("2.1.5\n"), Some((2, 1)));
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(parse_version("1.2.2\n"), Some((1, 2)));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
