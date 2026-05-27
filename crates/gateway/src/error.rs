use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("json-rpc error: {0}")]
    JsonRpc(String),
    #[error("backend error: {0}")]
    Backend(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml_edit::TomlError),
}

pub type Result<T> = std::result::Result<T, GatewayError>;

impl GatewayError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            GatewayError::Config(_) | GatewayError::JsonRpc(_) => StatusCode::BAD_REQUEST,
            GatewayError::NotFound(_) => StatusCode::NOT_FOUND,
            GatewayError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            GatewayError::Backend(_)
            | GatewayError::Io(_)
            | GatewayError::Yaml(_)
            | GatewayError::Json(_)
            | GatewayError::Toml(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = Json(json!({
            "error": {
                "message": self.to_string()
            }
        }));
        (status, body).into_response()
    }
}
