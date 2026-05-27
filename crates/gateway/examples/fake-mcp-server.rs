use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::process::{Child, Command};

use serde_json::{json, Value};

fn main() {
    let name = std::env::var("FAKE_MCP_NAME").unwrap_or_else(|_| "fake".to_string());
    if let Ok(path) = std::env::var("FAKE_MCP_START_MARKER") {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{}", std::process::id());
        }
    }
    let delay_ms: u64 = std::env::var("FAKE_MCP_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let crash_after_init = std::env::var("FAKE_MCP_CRASH_AFTER_INIT").ok().as_deref() == Some("1");
    let child: Option<Child> = std::env::var("FAKE_MCP_CHILD_SLEEP_SECS")
        .ok()
        .and_then(|seconds| Command::new("sleep").arg(seconds).spawn().ok());
    eprintln!("fake server starting TOKEN=FAKE_TEST_TOKEN_DO_NOT_USE");
    if let Ok(line) = std::env::var("FAKE_MCP_STDERR_LINE") {
        eprintln!("{line}");
    }

    for line in io::stdin().lock().lines() {
        let line = line.expect("stdin line");
        if line.trim().is_empty() {
            continue;
        }
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        let msg: Value = serde_json::from_str(&line).expect("json");
        if msg.get("id").is_none() {
            continue;
        }
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = match method {
            "initialize" => {
                if crash_after_init {
                    std::process::exit(42);
                }
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "tools": { "listChanged": true },
                            "prompts": { "listChanged": true },
                            "resources": { "listChanged": true }
                        },
                        "serverInfo": { "name": name, "version": "0.1.0" }
                    }
                })
            }
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        { "name": "echo", "description": "Echo arguments", "inputSchema": { "type": "object" }, "execution": { "taskSupport": "forbidden" } },
                        { "name": "search_repositories", "description": "Search repos", "inputSchema": { "type": "object" } }
                    ]
                }
            }),
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": format!("{name}:{params}") }]
                    }
                })
            }
            "prompts/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "prompts": [{ "name": "explain" }] }
            }),
            "resources/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "resources": [{ "uri": format!("fake://{name}/one"), "name": "one" }] }
            }),
            "fake/emit_notification" => {
                println!(
                    "{}",
                    json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/tools/list_changed"
                    })
                );
                json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": true } })
            }
            "fake/child_pid" => {
                if let Some(child) = child.as_ref() {
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "pid": child.id() } })
                } else {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32000, "message": "child process not configured" }
                    })
                }
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unknown method {method}") }
            }),
        };
        println!("{response}");
        io::stdout().flush().ok();
    }
}
