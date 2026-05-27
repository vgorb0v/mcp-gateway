use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{GatewayError, Result};
use crate::jsonrpc::{JsonRpcMessage, JsonRpcNotification, JsonRpcResponse};
use crate::registry::BackendRegistry;
use crate::sessions::{SessionManager, SessionTarget};
use crate::Config;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub registry: BackendRegistry,
    pub sessions: SessionManager,
}

pub fn app(
    config: impl Into<Arc<Config>>,
    registry: BackendRegistry,
    sessions: SessionManager,
) -> Router {
    let config = config.into();
    let max_message_bytes = config.limits.max_message_bytes;
    let state = AppState {
        config,
        registry,
        sessions,
    };
    Router::new()
        .route("/health", get(health))
        .route("/servers", get(servers))
        .route("/servers/:name/health", get(server_health))
        .route("/servers/:name/mcp", post(server_mcp))
        .route("/servers/:name/restart", post(server_restart))
        .route("/servers/:name/stop", post(server_stop))
        .route(
            "/servers/:name/capabilities/refresh",
            post(refresh_server_capabilities),
        )
        .route(
            "/capabilities",
            get(capabilities).post(refresh_capabilities),
        )
        .route("/groups/:group/mcp", post(group_mcp))
        .route("/sessions", post(create_session))
        .route("/sessions/:id", delete(delete_session))
        .route("/sessions/:id/mcp", post(session_mcp))
        .route("/sessions/:id/events", get(session_events))
        .route("/logs/:server", get(logs))
        .with_state(state)
        .layer(middleware::from_fn(validate_local_http_request))
        .layer(DefaultBodyLimit::max(max_message_bytes))
}

async fn validate_local_http_request(
    request: Request<Body>,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    if !is_allowed_host(request.headers().get(axum::http::header::HOST)) {
        return Err(StatusCode::FORBIDDEN);
    }
    if !is_allowed_origin(request.headers().get(axum::http::header::ORIGIN)) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}

fn is_allowed_host(value: Option<&axum::http::HeaderValue>) -> bool {
    let Some(value) = value else {
        return true;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    is_loopback_authority(value)
}

fn is_allowed_origin(value: Option<&axum::http::HeaderValue>) -> bool {
    let Some(value) = value else {
        return true;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    if value.eq_ignore_ascii_case("null") {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1")
    )
}

fn is_loopback_authority(value: &str) -> bool {
    let host = value
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']').map(|(host, _)| host))
        .unwrap_or_else(|| value.split(':').next().unwrap_or(value));
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

async fn health() -> Json<Value> {
    Json(serde_json::json!({
        "status": "healthy",
        "service": "mcp-gateway"
    }))
}

async fn servers(State(state): State<AppState>) -> Json<Value> {
    Json(serde_json::json!({
        "servers": state.registry.server_status().await
    }))
}

async fn server_health(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(serde_json::to_value(
        state.registry.server_health(&name).await?,
    )?))
}

async fn server_restart(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(serde_json::to_value(
        state.registry.restart(&name).await?,
    )?))
}

async fn server_stop(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(serde_json::to_value(
        state.registry.stop(&name).await?,
    )?))
}

async fn logs(State(state): State<AppState>, Path(server): Path<String>) -> Json<Value> {
    Json(serde_json::json!({
        "server": server,
        "entries": state.registry.logs(&server)
    }))
}

async fn capabilities(State(state): State<AppState>) -> Json<Value> {
    Json(serde_json::to_value(state.registry.capability_cache()).unwrap_or_else(|_| json!({})))
}

async fn refresh_server_capabilities(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let refreshed = state.registry.refresh_server_capabilities(&name).await?;
    let changed_client_configs = state.registry.sync_managed_client_configs()?;
    Ok(Json(serde_json::json!({
        "server": name,
        "capabilities": refreshed,
        "changedClientConfigs": changed_client_configs
    })))
}

#[derive(Debug, Deserialize)]
struct RefreshCapabilitiesRequest {
    group: Option<String>,
}

async fn refresh_capabilities(
    State(state): State<AppState>,
    Json(request): Json<RefreshCapabilitiesRequest>,
) -> Result<Json<Value>> {
    let group = request
        .group
        .unwrap_or_else(|| state.config.clients.default_group.clone());
    let refreshed = state.registry.refresh_group_capabilities(&group).await?;
    let changed_client_configs = state.registry.sync_managed_client_configs()?;
    Ok(Json(serde_json::json!({
        "group": group,
        "servers": refreshed,
        "changedClientConfigs": changed_client_configs
    })))
}

async fn server_mcp(
    State(state): State<AppState>,
    Path(server): Path<String>,
    Json(value): Json<Value>,
) -> Result<Json<Value>> {
    let output = process_mcp_value(
        value,
        |message| {
            let state = state.clone();
            let server = server.clone();
            async move {
                match message {
                    JsonRpcMessage::Request(request) => Ok(Some(
                        state
                            .registry
                            .handle_server_request(&server, request)
                            .await?,
                    )),
                    JsonRpcMessage::Notification(notification) => {
                        state
                            .registry
                            .handle_server_notification(&server, notification)
                            .await?;
                        Ok(None)
                    }
                    JsonRpcMessage::Response(_) => Ok(None),
                }
            }
        },
        state.config.limits.max_batch_items,
    )
    .await?;
    Ok(Json(output))
}

async fn group_mcp(
    State(state): State<AppState>,
    Path(group): Path<String>,
    Json(value): Json<Value>,
) -> Result<Json<Value>> {
    let output = process_mcp_value(
        value,
        |message| {
            let state = state.clone();
            let group = group.clone();
            async move {
                match message {
                    JsonRpcMessage::Request(request) => Ok(Some(
                        state.registry.handle_group_request(&group, request).await?,
                    )),
                    JsonRpcMessage::Notification(notification) => {
                        state
                            .registry
                            .handle_group_notification(&group, notification)
                            .await?;
                        Ok(None)
                    }
                    JsonRpcMessage::Response(_) => Ok(None),
                }
            }
        },
        state.config.limits.max_batch_items,
    )
    .await?;
    Ok(Json(output))
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    group: Option<String>,
    server: Option<String>,
    client: Option<String>,
    jsonrpc: Option<Value>,
    method: Option<String>,
    id: Option<Value>,
}

async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<Value>> {
    if request.jsonrpc.is_some() || request.method.is_some() || request.id.is_some() {
        return Err(GatewayError::Config(
            "/sessions creates bridge sessions; send JSON-RPC MCP messages to /sessions/{id}/mcp"
                .to_string(),
        ));
    }
    let target = match (request.group, request.server) {
        (Some(_), Some(_)) => {
            return Err(GatewayError::Config(
                "session request must specify either group or server, not both".to_string(),
            ));
        }
        (Some(group), None) => SessionTarget::Group(group),
        (None, Some(server)) => SessionTarget::Server(server),
        (None, None) => SessionTarget::Group(state.config.clients.default_group.clone()),
    };
    let session = state.sessions.create_with_client(target, request.client)?;
    Ok(Json(serde_json::to_value(session)?))
}

async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    state.sessions.delete(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn session_mcp(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(value): Json<Value>,
) -> Result<Json<Value>> {
    let target = state.sessions.target_for_session(&id)?;
    let output = process_mcp_value(
        value,
        |message| {
            let state = state.clone();
            let target = target.clone();
            let id = id.clone();
            async move {
                match message {
                    JsonRpcMessage::Request(request) => match &target {
                        SessionTarget::Group(group) => Ok(Some(
                            state.registry.handle_group_request(group, request).await?,
                        )),
                        SessionTarget::Server(server) => Ok(Some(
                            state
                                .registry
                                .handle_server_request(server, request)
                                .await?,
                        )),
                    },
                    JsonRpcMessage::Notification(notification) => {
                        state
                            .sessions
                            .record_client_notification(&id, notification.clone())?;
                        match &target {
                            SessionTarget::Group(group) => {
                                state
                                    .registry
                                    .handle_group_notification(group, notification)
                                    .await?;
                            }
                            SessionTarget::Server(server) => {
                                state
                                    .registry
                                    .handle_server_notification(server, notification)
                                    .await?;
                            }
                        }
                        Ok(None)
                    }
                    JsonRpcMessage::Response(_) => Ok(None),
                }
            }
        },
        state.config.limits.max_batch_items,
    )
    .await?;
    Ok(Json(output))
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    timeout_ms: Option<u64>,
}

async fn session_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<JsonRpcNotification>>> {
    let timeout = Duration::from_millis(query.timeout_ms.unwrap_or(25_000));
    Ok(Json(state.sessions.poll(&id, timeout).await?))
}

async fn process_mcp_value<F, Fut>(
    value: Value,
    mut handler: F,
    max_batch_items: usize,
) -> Result<Value>
where
    F: FnMut(JsonRpcMessage) -> Fut,
    Fut: std::future::Future<Output = Result<Option<JsonRpcResponse>>>,
{
    let is_batch = value.is_array();
    let messages = match JsonRpcMessage::parse_batch_or_single_with_limit(value, max_batch_items) {
        Ok(messages) => messages,
        Err(err) if err.to_string().contains("batch") => {
            let error = serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": -32600,
                    "message": err.to_string()
                }
            });
            return if is_batch {
                Ok(Value::Array(vec![error]))
            } else {
                Ok(error)
            };
        }
        Err(err) => return Err(err),
    };
    let mut responses = Vec::new();
    for message in messages {
        if let Some(response) = handler(message).await? {
            responses.push(serde_json::to_value(response)?);
        }
    }
    if is_batch {
        Ok(Value::Array(responses))
    } else {
        Ok(responses.into_iter().next().unwrap_or(Value::Null))
    }
}

impl From<crate::jsonrpc::JsonRpcError> for GatewayError {
    fn from(error: crate::jsonrpc::JsonRpcError) -> Self {
        GatewayError::JsonRpc(error.message)
    }
}
