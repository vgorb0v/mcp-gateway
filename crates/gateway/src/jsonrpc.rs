use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{GatewayError, Result};

pub const JSONRPC_VERSION: &str = "2.0";
pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_INITIALIZED: &str = "initialized";
pub const METHOD_TOOLS_LIST: &str = "tools/list";
pub const METHOD_TOOLS_CALL: &str = "tools/call";
pub const METHOD_PROMPTS_LIST: &str = "prompts/list";
pub const METHOD_RESOURCES_LIST: &str = "resources/list";
pub const NOTIFICATION_TOOLS_LIST_CHANGED: &str = "notifications/tools/list_changed";
pub const NOTIFICATION_PROMPTS_LIST_CHANGED: &str = "notifications/prompts/list_changed";
pub const NOTIFICATION_RESOURCES_LIST_CHANGED: &str = "notifications/resources/list_changed";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    Number(i64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: JsonRpcId, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: JsonRpcId, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: JsonRpcId, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
            data: None,
        }
    }

    pub fn method_not_found(message: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }

    pub fn gateway_error(message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code: -32000,
            message: message.into(),
            data,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

impl JsonRpcMessage {
    pub fn parse_batch_or_single(value: Value) -> Result<Vec<Self>> {
        Self::parse_batch_or_single_with_limit(value, usize::MAX)
    }

    pub fn parse_batch_or_single_with_limit(
        value: Value,
        max_batch_items: usize,
    ) -> Result<Vec<Self>> {
        match value {
            Value::Array(items) => {
                if items.len() > max_batch_items {
                    return Err(GatewayError::JsonRpc(format!(
                        "JSON-RPC batch contains {} items, exceeding limit {max_batch_items}",
                        items.len()
                    )));
                }
                items
                    .into_iter()
                    .map(serde_json::from_value)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(GatewayError::from)
            }
            single => Ok(vec![serde_json::from_value(single)?]),
        }
    }
}
