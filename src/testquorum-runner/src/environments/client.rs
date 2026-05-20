use reqwest::header::AUTHORIZATION;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;

pub(super) const BASE_URL: &str = "https://api.testquorum.dev";

pub(super) fn unauthenticated() -> testquorum_api::Client {
    let http = reqwest::Client::new();
    testquorum_api::Client::new_with_client(BASE_URL, http.clone(), http)
}

pub(super) fn with_bearer(token: &str) -> Result<testquorum_api::Client, anyhow::Error> {
    let mut headers = HeaderMap::new();
    let mut value = HeaderValue::from_str(&format!("Bearer {}", token))
        .map_err(|_| anyhow::anyhow!("auth token contains invalid header characters"))?;
    value.set_sensitive(true);
    headers.insert(AUTHORIZATION, value);
    let http = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build http client: {}", e))?;
    Ok(testquorum_api::Client::new_with_client(
        BASE_URL,
        http.clone(),
        http,
    ))
}
