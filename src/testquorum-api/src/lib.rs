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
}
