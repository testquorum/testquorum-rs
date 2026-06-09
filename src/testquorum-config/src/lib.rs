use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default = "default_managers")]
    pub managers: Managers,
    #[serde(default)]
    pub cloud: Cloud,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Managers {
    #[serde(default = "default_true")]
    pub autodetect: bool,
    pub nix: Option<NixConfig>,
    pub cargo: Option<CargoConfig>,
    pub treefmt: Option<TreefmtConfig>,
}

fn default_managers() -> Managers {
    Managers {
        autodetect: true,
        nix: None,
        cargo: None,
        treefmt: None,
    }
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct NixConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_attrset")]
    pub attrset: String,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct CargoConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub manifest_path: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_attrset() -> String {
    "checks".to_string()
}

/// How the runner interacts with the TestQuorum cloud: whether we tolerate
/// failures and how long we're willing to wait on rate-limit hints.
///
/// By default a cloud failure (5xx, 402, network, or a 429 whose
/// `Retry-After` exceeds the cap) drops the affected flow to its local
/// fallback (currently: random test order). Set `required = true` to
/// surface those as hard errors instead — for users who specifically need
/// every run to reach the cloud.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Cloud {
    /// When `true`, an unrecoverable cloud failure exits the runner
    /// non-zero instead of dropping to the local fallback. Default `false`.
    #[serde(default)]
    pub required: bool,
    /// Upper bound on a single `Retry-After` sleep, in seconds. A 429 with
    /// a hint at or below this cap is always honored and retried; a hint
    /// above it is treated as a cloud failure (and behaves per `required`).
    #[serde(default = "default_max_wait_seconds")]
    pub max_wait_seconds: u64,
}

impl Default for Cloud {
    fn default() -> Self {
        Self {
            required: false,
            max_wait_seconds: default_max_wait_seconds(),
        }
    }
}

fn default_max_wait_seconds() -> u64 {
    10
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct TreefmtConfig {
    /// `None` (default) means auto-detect: run only if a treefmt config file exists.
    /// `Some(true)` means always run.
    /// `Some(false)` means disabled.
    pub enabled: Option<bool>,
}

pub fn from_str(s: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_config() {
        let toml = r#"
[managers]
autodetect = true

[managers.nix]
enabled = true
attrset = "checks"
"#;
        let config = from_str(toml).unwrap();
        assert!(config.managers.autodetect);
        assert_eq!(
            config.managers.nix,
            Some(NixConfig {
                enabled: true,
                attrset: "checks".to_string(),
            })
        );
    }

    #[test]
    fn test_default_config() {
        let config = from_str("").unwrap();
        assert!(config.managers.autodetect);
        assert_eq!(config.managers.nix, None);
    }

    #[test]
    fn test_enabled_false() {
        let toml = r#"
[managers]
autodetect = true

[managers.nix]
enabled = false
"#;
        let config = from_str(toml).unwrap();
        assert_eq!(
            config.managers.nix,
            Some(NixConfig {
                enabled: false,
                attrset: "checks".to_string(),
            })
        );
    }

    #[test]
    fn test_attrset_ci() {
        let toml = r#"
[managers]
autodetect = true

[managers.nix]
enabled = true
attrset = "ci"
"#;
        let config = from_str(toml).unwrap();
        assert_eq!(
            config.managers.nix,
            Some(NixConfig {
                enabled: true,
                attrset: "ci".to_string(),
            })
        );
    }

    #[test]
    fn test_attrset_default_when_omitted() {
        let toml = r#"
[managers]
autodetect = true

[managers.nix]
enabled = true
"#;
        let config = from_str(toml).unwrap();
        assert_eq!(
            config.managers.nix,
            Some(NixConfig {
                enabled: true,
                attrset: "checks".to_string(),
            })
        );
    }

    #[test]
    fn test_partial_config() {
        let toml = r#"
[managers]
autodetect = false
"#;
        let config = from_str(toml).unwrap();
        assert!(!config.managers.autodetect);
        assert_eq!(config.managers.nix, None);
    }

    #[test]
    fn test_cargo_config() {
        let toml = r#"
[managers]
autodetect = true

[managers.cargo]
enabled = true
"#;
        let config = from_str(toml).unwrap();
        assert_eq!(
            config.managers.cargo,
            Some(CargoConfig {
                enabled: true,
                manifest_path: None,
            })
        );
    }

    #[test]
    fn test_cargo_manifest_path() {
        let toml = r#"
[managers]
autodetect = true

[managers.cargo]
enabled = true
manifest_path = "subdir/Cargo.toml"
"#;
        let config = from_str(toml).unwrap();
        assert_eq!(
            config.managers.cargo,
            Some(CargoConfig {
                enabled: true,
                manifest_path: Some("subdir/Cargo.toml".to_string()),
            })
        );
    }

    #[test]
    fn test_cargo_default_when_omitted() {
        let toml = r#"
[managers]
autodetect = true
"#;
        let config = from_str(toml).unwrap();
        assert_eq!(config.managers.cargo, None);
    }

    #[test]
    fn test_treefmt_config_enabled() {
        let toml = r#"
[managers]
autodetect = true

[managers.treefmt]
enabled = true
"#;
        let config = from_str(toml).unwrap();
        assert_eq!(
            config.managers.treefmt,
            Some(TreefmtConfig {
                enabled: Some(true)
            })
        );
    }

    #[test]
    fn test_treefmt_config_disabled() {
        let toml = r#"
[managers]
autodetect = true

[managers.treefmt]
enabled = false
"#;
        let config = from_str(toml).unwrap();
        assert_eq!(
            config.managers.treefmt,
            Some(TreefmtConfig {
                enabled: Some(false)
            })
        );
    }

    #[test]
    fn test_treefmt_default_when_omitted() {
        let toml = r#"
[managers]
autodetect = true
"#;
        let config = from_str(toml).unwrap();
        assert_eq!(config.managers.treefmt, None);
    }

    #[test]
    fn test_invalid_toml() {
        let result = from_str("not valid toml {{{");
        assert!(result.is_err());
    }

    #[test]
    fn test_cloud_defaults_when_omitted() {
        let config = from_str("").unwrap();
        assert_eq!(config.cloud, Cloud::default());
        assert!(!config.cloud.required);
        assert_eq!(config.cloud.max_wait_seconds, 10);
    }

    #[test]
    fn test_cloud_overrides() {
        let toml = r#"
[cloud]
required = true
max_wait_seconds = 30
"#;
        let config = from_str(toml).unwrap();
        assert!(config.cloud.required);
        assert_eq!(config.cloud.max_wait_seconds, 30);
    }

    #[test]
    fn test_config_equality() {
        let config1 = Config {
            managers: Managers {
                autodetect: true,
                nix: Some(NixConfig {
                    enabled: true,
                    attrset: "checks".to_string(),
                }),
                cargo: None,
                treefmt: None,
            },
            cloud: Cloud::default(),
        };
        let toml = r#"
[managers]
autodetect = true

[managers.nix]
enabled = true
attrset = "checks"
"#;
        let config2 = from_str(toml).unwrap();
        assert_eq!(config1, config2);
    }
}
