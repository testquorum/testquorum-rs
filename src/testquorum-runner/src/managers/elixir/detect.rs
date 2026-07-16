use super::ElixirError;

pub(crate) fn detect_elixir(mix_exs_path: Option<&str>) -> Result<String, ElixirError> {
    which::which("mix").map_err(|_| ElixirError::NotFound)?;

    let output = std::process::Command::new("elixir")
        .arg("--version")
        .output()
        .map_err(|_| ElixirError::NotFound)?;

    let version_str = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);

    let (major, minor) = parse_version(&version_str).ok_or_else(|| ElixirError::TooOld {
        minimum: "1.14".to_string(),
        found: version_str
            .trim()
            .lines()
            .last()
            .unwrap_or("")
            .trim()
            .to_string(),
    })?;

    if (major, minor) < (1, 14) {
        return Err(ElixirError::TooOld {
            minimum: "1.14".to_string(),
            found: format!("{}.{}", major, minor),
        });
    }

    let resolved = mix_exs_path.unwrap_or("mix.exs");
    if !std::path::Path::new(resolved).exists() {
        return Err(ElixirError::MixExsNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}

fn parse_version(output: &str) -> Option<(u32, u32)> {
    // "Erlang/OTP 26 [...]\n\nElixir 1.15.7 (compiled with Erlang/OTP 26)"
    let prefix = "Elixir ";
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
            parse_version(
                "Erlang/OTP 26 [erts-14.1.1]\n\nElixir 1.15.7 (compiled with Erlang/OTP 26)"
            ),
            Some((1, 15))
        );
    }

    #[test]
    fn parse_version_old() {
        assert_eq!(
            parse_version("Erlang/OTP 24\n\nElixir 1.12.3 (compiled with Erlang/OTP 24)"),
            Some((1, 12))
        );
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
