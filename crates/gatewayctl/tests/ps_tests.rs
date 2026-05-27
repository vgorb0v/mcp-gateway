use std::fs;
use std::net::SocketAddr;
use std::process::Command;

use std::sync::{Arc, Mutex};

use axum::{extract::Path, routing::get, routing::post, Json, Router};
use serde_json::json;
use tokio::net::TcpListener;

#[tokio::test(flavor = "multi_thread")]
async fn ps_prints_table_with_rss_column() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    let app = Router::new().route(
        "/servers",
        get(|| async {
            Json(json!({
                "servers": [
                    { "name": "browser-tools", "state": "running",
                      "pid": 1432, "uptime_seconds": 754, "rss_kb": 186368,
                      "initialized": true, "last_error": null,
                      "last_used_seconds": 12, "active_requests": 1, "pending_requests": 2 },
                    { "name": "example-tools", "state": "stopped",
                      "pid": null, "uptime_seconds": null,
                      "initialized": false, "last_error": null }
                ]
            }))
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let bin = env!("CARGO_BIN_EXE_mcpgateway");
    let state_dir = tempfile::tempdir().expect("state dir");
    let state_file = state_dir.path().join("state.json");
    fs::write(
        &state_file,
        serde_json::json!({
            "base_url": format!("http://{addr}"),
            "pid": 12345_u32
        })
        .to_string(),
    )
    .unwrap();

    let output = Command::new(bin)
        .args(["ps", "--state-file", state_file.to_str().unwrap()])
        .output()
        .expect("run mcpgateway ps");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("SERVER"), "missing header: {stdout}");
    assert!(stdout.contains("RSS"), "missing RSS column: {stdout}");
    assert!(stdout.contains("IDLE"), "missing IDLE column: {stdout}");
    assert!(stdout.contains("REQ"), "missing REQ column: {stdout}");
    assert!(stdout.contains("PEND"), "missing PEND column: {stdout}");
    assert!(stdout.contains("browser-tools"), "missing row: {stdout}");
    assert!(
        stdout.contains("182MB") || stdout.contains("181MB"),
        "rss not formatted: {stdout}"
    );
    // chrome row had last_used_seconds: 12 -> "12s", active: 1, pending: 2
    assert!(stdout.contains("12s"), "missing idle value: {stdout}");
    assert!(
        stdout.contains("example-tools"),
        "missing stopped row: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_posts_to_named_server_stop_route() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let stopped = Arc::new(Mutex::new(Vec::<String>::new()));
    let stopped_for_route = stopped.clone();

    let app = Router::new().route(
        "/servers/:name/stop",
        post(move |Path(name): Path<String>| {
            let stopped = stopped_for_route.clone();
            async move {
                stopped.lock().unwrap().push(name.clone());
                Json(json!({
                    "name": name,
                    "state": "stopped",
                    "pid": null,
                    "uptime_seconds": null,
                    "initialized": false,
                    "last_error": null
                }))
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let bin = env!("CARGO_BIN_EXE_mcpgateway");
    let state_dir = tempfile::tempdir().expect("state dir");
    let state_file = state_dir.path().join("state.json");
    fs::write(
        &state_file,
        serde_json::json!({
            "base_url": format!("http://{addr}"),
            "pid": 12345_u32
        })
        .to_string(),
    )
    .unwrap();

    let output = Command::new(bin)
        .args([
            "stop",
            "browser-tools",
            "--state-file",
            state_file.to_str().unwrap(),
        ])
        .output()
        .expect("run mcpgateway stop");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        stopped.lock().unwrap().as_slice(),
        ["browser-tools"],
        "wrong stop calls"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("browser-tools"));
    assert!(stdout.contains("stopped"));
}
