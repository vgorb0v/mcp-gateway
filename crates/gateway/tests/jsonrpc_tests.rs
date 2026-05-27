use mcp_gateway::jsonrpc::{
    JsonRpcError, JsonRpcId, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};
use serde_json::json;

#[test]
fn parses_request_notification_response_and_batch() {
    let request: JsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/list",
        "params": { "cursor": "abc" }
    }))
    .expect("request");
    assert!(matches!(request, JsonRpcMessage::Request(_)));

    let notification: JsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "method": "initialized"
    }))
    .expect("notification");
    assert!(matches!(notification, JsonRpcMessage::Notification(_)));

    let response: JsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": "abc",
        "result": { "ok": true }
    }))
    .expect("response");
    assert!(matches!(response, JsonRpcMessage::Response(_)));

    let batch = JsonRpcMessage::parse_batch_or_single(json!([
        { "jsonrpc": "2.0", "id": 1, "method": "initialize" },
        { "jsonrpc": "2.0", "method": "initialized" }
    ]))
    .expect("batch");
    assert_eq!(batch.len(), 2);
}

#[test]
fn response_helpers_preserve_id_and_error_shape() {
    let req = JsonRpcRequest::new(JsonRpcId::Number(42), "tools/call", Some(json!({})));
    let response = JsonRpcResponse::success(req.id.clone(), json!({ "ok": true }));
    assert_eq!(response.id, JsonRpcId::Number(42));
    assert!(response.error.is_none());

    let error = JsonRpcResponse::error(
        JsonRpcId::String("client-id".to_string()),
        JsonRpcError::invalid_params("bad prefix"),
    );
    assert_eq!(error.id, JsonRpcId::String("client-id".to_string()));
    assert_eq!(error.error.expect("error").code, -32602);
}

#[test]
fn notification_has_no_id_when_serialized() {
    let notification = JsonRpcNotification::new("notifications/tools/list_changed", None);
    let value = serde_json::to_value(notification).expect("json");
    assert!(value.get("id").is_none());
}

#[test]
fn batch_parsing_enforces_item_limit() {
    let err = JsonRpcMessage::parse_batch_or_single_with_limit(
        json!([
            { "jsonrpc": "2.0", "id": 1, "method": "initialize" },
            { "jsonrpc": "2.0", "id": 2, "method": "tools/list" },
            { "jsonrpc": "2.0", "method": "initialized" }
        ]),
        2,
    )
    .expect_err("oversized batch should fail");

    assert!(err.to_string().contains("batch"));
}

#[test]
fn batch_parsing_preserves_mixed_message_order() {
    let messages = JsonRpcMessage::parse_batch_or_single_with_limit(
        json!([
            { "jsonrpc": "2.0", "id": 1, "method": "initialize" },
            { "jsonrpc": "2.0", "method": "initialized" },
            { "jsonrpc": "2.0", "id": "done", "result": { "ok": true } }
        ]),
        3,
    )
    .expect("mixed batch");

    assert!(matches!(
        &messages[0],
        JsonRpcMessage::Request(JsonRpcRequest { method, .. }) if method == "initialize"
    ));
    assert!(matches!(
        &messages[1],
        JsonRpcMessage::Notification(JsonRpcNotification { method, .. }) if method == "initialized"
    ));
    assert!(matches!(
        &messages[2],
        JsonRpcMessage::Response(JsonRpcResponse { id: JsonRpcId::String(id), .. }) if id == "done"
    ));
}

#[test]
fn batch_parser_consumes_array_items_without_cloning_values() {
    let source = include_str!("../src/jsonrpc.rs");

    assert!(
        source.contains("Value::Array(items)"),
        "batch parser should destructure the consumed Value"
    );
    assert!(
        source.contains(".into_iter()"),
        "batch parser should consume array items by value"
    );
    assert!(
        !source.contains(".iter()\n                .cloned()"),
        "batch parser must not clone every JSON Value"
    );
}
