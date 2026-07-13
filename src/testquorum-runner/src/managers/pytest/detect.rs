use super::PytestError;

pub(crate) fn detect_pytest(config_path: Option<&str>) -> Result<String, PytestError> {
    let python_path = which::which("python3").map_err(|_| PytestError::PythonNotFound)?;

    let output = std::process::Command::new(&python_path)
        .arg("--version")
        .output()
        .map_err(|_| PytestError::PythonNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version_str = if version_str.trim().is_empty() {
        // python3 --version writes to stderr on some older versions
        String::from_utf8_lossy(&output.stderr).to_string()
    } else {
        version_str.to_string()
    };

    let (major, minor) = parse_version(&version_str).ok_or_else(|| PytestError::PythonTooOld {
        minimum: "3.8".to_string(),
        found: version_str.trim().to_string(),
    })?;

    if (major, minor) < (3, 8) {
        return Err(PytestError::PythonTooOld {
            minimum: "3.8".to_string(),
            found: format!("{}.{}", major, minor),
        });
    }

    let import_ok = std::process::Command::new(&python_path)
        .args(["-c", "import pytest"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !import_ok {
        return Err(PytestError::PytestNotFound);
    }

    if let Some(explicit) = config_path {
        if !std::path::Path::new(explicit).exists() {
            return Err(PytestError::PyprojectNotFound {
                path: explicit.to_string(),
            });
        }
        return Ok(explicit.to_string());
    }

    let candidates = ["pyproject.toml", "setup.py", "setup.cfg"];
    for candidate in &candidates {
        if std::path::Path::new(candidate).exists() {
            return Ok((*candidate).to_string());
        }
    }

    Err(PytestError::PyprojectNotFound {
        path: "pyproject.toml / setup.py / setup.cfg".to_string(),
    })
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // Parse "Python 3.11.2" -> (3, 11)
    let trimmed = output.trim();
    let version_part = trimmed.strip_prefix("Python ")?;
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
        assert_eq!(parse_version("Python 3.11.2\n"), Some((3, 11)));
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(parse_version("Python 2.7.18"), Some((2, 7)));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }

    #[test]
    fn parse_version_minimum_boundary() {
        assert_eq!(parse_version("Python 3.8.0"), Some((3, 8)));
    }

    #[test]
    fn parse_version_below_minimum() {
        let v = parse_version("Python 3.7.12").unwrap();
        assert!(v < (3, 8));
    }
}
