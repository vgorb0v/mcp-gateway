use std::path::Path;

use toml_edit::{value, Array, DocumentMut, Item, Table};

use crate::error::{GatewayError, Result};

pub fn render_codex_config(
    existing: Option<&str>,
    mcp_dir: &Path,
    server_names: &[String],
    dedupe: &[String],
    client_id: Option<&str>,
) -> Result<String> {
    let mut doc = match existing {
        Some(text) if !text.trim().is_empty() => text.parse::<DocumentMut>()?,
        _ => DocumentMut::new(),
    };

    if !doc.as_table().contains_key("mcp_servers") {
        doc["mcp_servers"] = Item::Table(Table::new());
    }
    let Some(servers) = doc["mcp_servers"].as_table_mut() else {
        return Err(GatewayError::Config(
            "Codex mcp_servers must be a TOML table".to_string(),
        ));
    };

    servers.remove("mcp-gateway");
    for name in dedupe {
        servers.remove(name);
    }

    for name in server_names {
        let mut table = Table::new();
        table["command"] = value(mcp_dir.join(name).to_string_lossy().to_string());
        let mut args = Array::new();
        if let Some(client) = client_id {
            args.push("--client");
            args.push(client);
        }
        table["args"] = value(args);
        servers.insert(name, Item::Table(table));
    }

    Ok(doc.to_string())
}
