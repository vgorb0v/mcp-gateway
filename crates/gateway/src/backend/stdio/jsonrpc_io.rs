use std::time::Duration;

use crate::config::ServerConfig;
use crate::error::{GatewayError, Result};

pub(super) fn browser_debug_url(config: &ServerConfig) -> Option<String> {
    config.args.iter().find_map(|arg| {
        arg.strip_prefix("--browser-url=")
            .map(|value| value.to_string())
    })
}

pub(super) async fn ensure_browser_debug_url(url: &str) -> Result<()> {
    let version_url = format!("{}/json/version", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .map_err(|err| GatewayError::Backend(err.to_string()))?;
    let response = client.get(&version_url).send().await;
    match response {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(GatewayError::Backend(format!(
            "Browser remote debugging is not reachable at {url} (GET /json/version returned {})",
            response.status()
        ))),
        Err(err) => Err(GatewayError::Backend(format!(
            "Browser remote debugging is not reachable at {url}: {err}. Check the explicit browser attach endpoint, or remove the attach flag to let the MCP server launch its own isolated browser."
        ))),
    }
}
