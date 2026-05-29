use super::CargoError;

pub(crate) fn detect_cargo(manifest_path: Option<&str>) -> Result<String, CargoError> {
    let cargo_path = which::which("cargo").map_err(|_| CargoError::CargoNotFound)?;

    let output = std::process::Command::new(&cargo_path)
        .arg("--version")
        .output()
        .map_err(|_| CargoError::CargoNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version = parse_version(&version_str).ok_or_else(|| CargoError::CargoTooOld {
        minimum: "1.70".to_string(),
        found: version_str.trim().to_string(),
    })?;

    // Minimum version 1.70 (arbitrary reasonable baseline).
    if version < (1, 70) {
        return Err(CargoError::CargoTooOld {
            minimum: "1.70".to_string(),
            found: format!("{}.{}", version.0, version.1),
        });
    }

    let resolved = manifest_path.unwrap_or("Cargo.toml");
    if manifest_path.is_none() && !std::path::Path::new(resolved).exists() {
        return Err(CargoError::ManifestNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // Parse "cargo 1.84.0 (66221abde 2024-11-19)" -> (1, 84).
    let version_part = output.split_whitespace().nth(1)?;
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
        assert_eq!(
            parse_version("cargo 1.84.0 (66221abde 2024-11-19)"),
            Some((1, 84))
        );
    }

    #[test]
    fn parse_version_short() {
        assert_eq!(parse_version("cargo 1.70.0"), Some((1, 70)));
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(
            parse_version("cargo 1.69.0 (84c898d65 2023-04-11)"),
            Some((1, 69))
        );
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
