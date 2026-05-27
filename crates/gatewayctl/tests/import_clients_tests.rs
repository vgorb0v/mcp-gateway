//! Verifies `mcpgateway import-clients` finds manually-added MCP entries
//! and (with --write) drops scaffolded stubs into ~/.mcp-gateway/config/servers.d/.

use std::fs;
use std::process::Command;

#[test]
fn import_clients_lists_unmanaged_entries_without_write() {
    let home = tempfile::tempdir().expect("home");
    fs::create_dir_all(home.path().join(".claude")).ok();
    // The managed entry must point at the *actual* gateway mcps dir for this
    // install (under the temp HOME), so import-clients correctly skips it.
    let managed_command = home.path().join(".mcp-gateway/mcps/example-tools");
    let fixture = serde_json::json!({
        "mcpServers": {
            "user-added": { "command": "/usr/local/bin/foo", "args": ["--bar"] },
            "managed":    { "command": managed_command.to_string_lossy(), "args": [] }
        }
    });
    fs::write(
        home.path().join(".claude.json"),
        serde_json::to_string_pretty(&fixture).unwrap(),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_mcpgateway");
    let output = Command::new(bin)
        .args([
            "import-clients",
            "--home",
            home.path().to_str().unwrap(),
            "--clients",
            "claude-code",
        ])
        .output()
        .expect("run import-clients");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("user-added"), "stdout: {stdout}");
    assert!(stdout.contains("/usr/local/bin/foo"), "stdout: {stdout}");
    // The managed entry must NOT be flagged as unmanaged.
    assert!(!stdout.contains("managed   "), "stdout: {stdout}");
    // Without --write we should not have created servers.d
    assert!(
        !home
            .path()
            .join(".mcp-gateway/config/servers.d/user-added.yaml")
            .exists(),
        "servers.d stub should not exist without --write"
    );
}

#[test]
fn import_clients_with_write_drops_stub_into_servers_d() {
    let home = tempfile::tempdir().expect("home");
    fs::write(
        home.path().join(".claude.json"),
        r#"{
            "mcpServers": {
                "my-custom": { "command": "/usr/local/bin/foo", "args": ["--bar"] }
            }
        }"#,
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_mcpgateway");
    let output = Command::new(bin)
        .args([
            "import-clients",
            "--write",
            "--home",
            home.path().to_str().unwrap(),
            "--clients",
            "claude-code",
        ])
        .output()
        .expect("run import-clients --write");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");

    let stub = home
        .path()
        .join(".mcp-gateway/config/servers.d/my-custom.yaml");
    assert!(stub.exists(), "stub should exist at {stub:?}");
    let body = fs::read_to_string(&stub).expect("read stub");
    assert!(body.contains("id: my-custom"));
    assert!(body.contains("/usr/local/bin/foo"));
    assert!(body.contains("--bar"));
    assert!(body.contains("enabled: false"));
}

#[test]
fn import_clients_write_rejects_unsafe_entry_name_without_path_escape() {
    let home = tempfile::tempdir().expect("home");
    fs::write(
        home.path().join(".claude.json"),
        r#"{
            "mcpServers": {
                "../../../escaped": { "command": "/usr/local/bin/foo", "args": [] }
            }
        }"#,
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_mcpgateway");
    let output = Command::new(bin)
        .args([
            "import-clients",
            "--write",
            "--home",
            home.path().to_str().unwrap(),
            "--clients",
            "claude-code",
        ])
        .output()
        .expect("run import-clients --write");

    assert!(
        !output.status.success(),
        "unsafe imported names must fail instead of escaping servers.d"
    );
    assert!(
        !home.path().join("escaped.yaml").exists(),
        "import should not write outside servers.d"
    );
}

#[test]
fn import_clients_prints_friendly_message_when_nothing_to_import() {
    let home = tempfile::tempdir().expect("home");
    let bin = env!("CARGO_BIN_EXE_mcpgateway");
    let output = Command::new(bin)
        .args([
            "import-clients",
            "--home",
            home.path().to_str().unwrap(),
            "--clients",
            "claude-code,codex",
        ])
        .output()
        .expect("run import-clients");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        stdout.contains("No unmanaged MCP entries"),
        "stdout: {stdout}"
    );
}
