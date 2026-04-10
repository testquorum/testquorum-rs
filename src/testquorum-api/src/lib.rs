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
