use super::KotlinError;

pub(crate) fn detect_kotlin(build_file_path: Option<&str>) -> Result<String, KotlinError> {
    let java_path = which::which("java").map_err(|_| KotlinError::JavaNotFound)?;

    let output = std::process::Command::new(&java_path)
        .arg("-version")
        .output()
        .map_err(|_| KotlinError::JavaNotFound)?;

    // java -version writes to stderr
    let version_str = String::from_utf8_lossy(&output.stderr);
    let version = parse_version(&version_str).ok_or_else(|| KotlinError::JavaTooOld {
        minimum: "11".to_string(),
        found: version_str
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string(),
    })?;

    if version < 11 {
        return Err(KotlinError::JavaTooOld {
            minimum: "11".to_string(),
            found: format!("{}", version),
        });
    }

    if !std::path::Path::new("./gradlew").exists() {
        return Err(KotlinError::GradlewNotFound);
    }

    if let Some(path) = build_file_path {
        if !std::path::Path::new(path).exists() {
            return Err(KotlinError::BuildFileMissing {
                path: path.to_string(),
            });
        }
        return Ok(path.to_string());
    }

    if std::path::Path::new("build.gradle.kts").exists() {
        return Ok("build.gradle.kts".to_string());
    }

    if std::path::Path::new("build.gradle").exists() {
        return Ok("build.gradle".to_string());
    }

    Err(KotlinError::BuildFileMissing {
        path: "build.gradle.kts".to_string(),
    })
}

fn parse_version(output: &str) -> Option<u32> {
    // Same format as Maven's java version detection:
    //   openjdk version "21.0.1" 2023-10-17
    //   java version "17.0.1" 2021-10-19 LTS
    //   java version "1.8.0_392"
    let start = output.find('"')?;
    let rest = &output[start + 1..];
    let end = rest.find('"')?;
    let version_str = &rest[..end];

    let mut parts = version_str.split('.');
    let first = parts.next()?;

    if first == "1" {
        // Legacy format: "1.8.0_392" → major 8
        parts.next()?.parse().ok()
    } else {
        first.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_modern_21() {
        assert_eq!(
            parse_version("openjdk version \"21.0.1\" 2023-10-17\n..."),
            Some(21)
        );
    }

    #[test]
    fn parse_version_modern_11() {
        assert_eq!(
            parse_version("openjdk version \"11.0.21\" 2023-10-17\n..."),
            Some(11)
        );
    }

    #[test]
    fn parse_version_legacy_8() {
        assert_eq!(parse_version("java version \"1.8.0_392\"\n..."), Some(8));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not a version"), None);
    }
}
