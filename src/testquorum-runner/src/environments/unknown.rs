use async_trait::async_trait;

use super::Environment;
use super::client;

pub(crate) struct UnknownEnvironment;

impl UnknownEnvironment {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Environment for UnknownEnvironment {
    fn name(&self) -> &'static str {
        "unknown"
    }

    async fn authenticated_client(&self) -> Result<Option<testquorum_api::Client>, anyhow::Error> {
        let token = match std::env::var("TQ_AUTH_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => return Ok(None),
        };
        Ok(Some(client::with_bearer(&token)?))
    }
}
