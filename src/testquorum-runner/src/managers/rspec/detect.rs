use super::RspecError;

pub(crate) fn detect_rspec(gemfile_path: Option<&str>) -> Result<String, RspecError> {
    let ruby_path = which::which("ruby").map_err(|_| RspecError::RubyNotFound)?;

    let output = std::process::Command::new(&ruby_path)
        .arg("--version")
        .output()
        .map_err(|_| RspecError::RubyNotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version = parse_version(&version_str).ok_or_else(|| RspecError::RubyTooOld {
        minimum: "3.0".to_string(),
        found: version_str.trim().to_string(),
    })?;

    if version < (3, 0) {
        return Err(RspecError::RubyTooOld {
            minimum: "3.0".to_string(),
            found: format!("{}.{}", version.0, version.1),
        });
    }

    which::which("bundle").map_err(|_| RspecError::BundleNotFound)?;

    let resolved = gemfile_path.unwrap_or("Gemfile");
    if !std::path::Path::new(resolved).exists() {
        return Err(RspecError::GemfileNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // Parse "ruby 3.2.0 (2022-12-25 revision a528908271) [arm64-darwin22]"
    // → second token "3.2.0" or "3.0.6p216" → (3, 2) or (3, 0)
    let token = output.split_whitespace().nth(1)?;
    // Strip any trailing suffix like "p216"
    let version_part: String = token
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
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
        assert_eq!(
            parse_version("ruby 3.2.0 (2022-12-25 revision a528908271) [arm64-darwin22]\n"),
            Some((3, 2))
        );
    }

    #[test]
    fn parse_version_patch_suffix() {
        assert_eq!(parse_version("ruby 3.0.6p216 ..."), Some((3, 0)));
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(parse_version("ruby 2.7.8p225 ..."), Some((2, 7)));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
