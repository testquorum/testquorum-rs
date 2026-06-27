//! TestQuorum API client library.
//!
//! This crate provides a type-safe Rust interface to the TestQuorum API.
//! All types and the client are generated from the OpenAPI specification
//! using progenitor.

use progenitor::generate_api;

// Generate the API client and types from the OpenAPI specification.
// The base URL is hardcoded to api.testquorum.dev.
generate_api! {
    spec = "openapi.json",
    inner_type = reqwest::Client,
    patch = {
        TestManager = { derives = [PartialEq, Eq, Hash] },
    },
}

impl From<std::time::SystemTime> for types::EpochSecs {
    fn from(t: std::time::SystemTime) -> Self {
        let secs = t
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self(secs)
    }
}

const CUSTOM_PREFIX: &str = "custom:";

impl types::TestManager {
    /// Builds the identity for an in-house manager from its bare name (e.g.
    /// `"npm"`), encoding it as `custom:<name>` on the wire.
    ///
    /// Prefer `WellKnownTestManager::into()` for built-in managers. Fails only
    /// if `name` is empty, since the wire form must match `^custom:.+$`.
    pub fn custom(name: &str) -> Result<Self, types::error::ConversionError> {
        let encoded = format!("{CUSTOM_PREFIX}{name}");
        Ok(Self::CustomTestManager(types::CustomTestManager::try_from(
            encoded,
        )?))
    }
}

impl std::fmt::Display for types::TestManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WellKnownTestManager(m) => write!(f, "{m}"),
            Self::CustomTestManager(c) => f.write_str(c.as_str()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::UNIX_EPOCH;

    use super::types::EpochSecs;

    #[test]
    fn epoch_secs_from_system_time_after_epoch() {
        let t = UNIX_EPOCH + Duration::from_secs(1_234_567_890);
        assert_eq!(EpochSecs::from(t).0, 1_234_567_890);
    }

    #[test]
    fn epoch_secs_from_system_time_before_epoch_clamps_to_zero() {
        let t = UNIX_EPOCH - Duration::from_secs(10);
        assert_eq!(EpochSecs::from(t).0, 0);
    }

    use super::types::TestManager;
    use super::types::WellKnownTestManager;

    #[test]
    fn well_known_serializes_to_bare_name() {
        let m: TestManager = WellKnownTestManager::Cargo.into();
        assert_eq!(m.to_string(), "cargo");
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            serde_json::json!("cargo")
        );
    }

    #[test]
    fn custom_encodes_prefix() {
        let m = TestManager::custom("npm").unwrap();
        assert_eq!(m.to_string(), "custom:npm");
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            serde_json::json!("custom:npm")
        );
    }

    #[test]
    fn custom_rejects_empty_name() {
        assert!(TestManager::custom("").is_err());
    }

    #[test]
    fn custom_cargo_is_distinct_from_well_known_cargo() {
        let custom = TestManager::custom("cargo").unwrap();
        let well_known: TestManager = WellKnownTestManager::Cargo.into();
        assert_ne!(custom, well_known);
        assert_eq!(custom.to_string(), "custom:cargo");
        assert_eq!(well_known.to_string(), "cargo");
    }

    #[test]
    fn display_matches_wire_form() {
        let well_known: TestManager = WellKnownTestManager::Treefmt.into();
        assert_eq!(well_known.to_string(), "treefmt");
        assert_eq!(
            TestManager::custom("npm").unwrap().to_string(),
            "custom:npm"
        );
    }
}
