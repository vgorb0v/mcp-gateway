use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use axum::{routing::get, Json, Router};
use serde_json::json;
use tokio::net::TcpListener;

#[tokio::test(flavor = "multi_thread")]
async fn doctor_reports_healthy_temp_install() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/health",
        get(|| async { Json(json!({ "status": "healthy", "service": "mcp-gateway" })) }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let home = tempfile::tempdir().expect("home");
    seed_install(home.path(), addr);

    let output = Command::new(env!("CARGO_BIN_EXE_mcpgateway"))
        .args([
            "doctor",
            "--home",
            home.path().to_str().unwrap(),
            "--gateway",
            &format!("http://{addr}"),
            "--skip-launchctl",
        ])
        .output()
        .expect("run doctor");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("MCP Gateway doctor"));
    assert!(stdout.contains("config parses"));
    assert!(stdout.contains("daemon healthy"));
    assert!(stdout.contains("launchd plist"));
    assert!(stdout.contains("capability cache"));
}

#[test]
fn doctor_exits_nonzero_when_required_binary_is_missing() {
    let home = tempfile::tempdir().expect("home");
    let paths = mcp_gateway::native::NativeInstallPaths::for_home(home.path());
    fs::create_dir_all(&paths.config_dir).unwrap();
    fs::write(&paths.config_file, "listen: \"127.0.0.1:0\"\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_mcpgateway"))
        .args([
            "doctor",
            "--home",
            home.path().to_str().unwrap(),
            "--skip-launchctl",
        ])
        .output()
        .expect("run doctor");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("bridge binary missing") || stdout.contains("binary missing"));
}

fn seed_install(home: &std::path::Path, addr: SocketAddr) {
    let paths = mcp_gateway::native::NativeInstallPaths::for_home(home);
    fs::create_dir_all(&paths.bin_dir).unwrap();
    fs::create_dir_all(&paths.config_dir).unwrap();
    fs::create_dir_all(&paths.cache_dir).unwrap();
    fs::create_dir_all(&paths.mcp_dir).unwrap();
    fs::create_dir_all(paths.launch_agent_file.parent().unwrap()).unwrap();
    fs::write(
        &paths.config_file,
        r#"
listen: "127.0.0.1:0"
servers: {}
groups:
  coding:
    servers: []
clients:
  default_group: "coding"
  managed_clients: []
"#,
    )
    .unwrap();
    fs::write(
        &paths.capability_cache_file,
        r#"{"version":1,"servers":{}}"#,
    )
    .unwrap();
    fs::create_dir_all(paths.state_file.parent().unwrap()).unwrap();
    fs::write(
        &paths.state_file,
        json!({ "base_url": format!("http://{addr}"), "pid": 123_u32 }).to_string(),
    )
    .unwrap();
    fs::set_permissions(&paths.state_file, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(
        &paths.launch_agent_file,
        "<plist><dict><key>Label</key><string>io.github.mcpgateway.daemon</string></dict></plist>",
    )
    .unwrap();
    for binary in ["mcp-gateway", "mcp-gateway-bridge", "mcpgateway"] {
        let path = paths.bin_dir.join(binary);
        fs::write(&path, format!("#!/usr/bin/env sh\necho '{binary} 0.1.0'\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
