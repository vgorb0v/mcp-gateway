//! Higher-level orchestration over [`apply_configs`]: produces a structured
//! diff before writing anything, preserves entries the user added manually,
//! creates named backups under `~/.mcp-gateway/backups/`, and tracks managed
//! ownership in a sibling manifest so drift (a hand-edited managed entry) is
//! detectable on the next plan.
//!
//! `apply_configs` itself still exists as a thin convenience wrapper for
//! callers that just want the legacy "write everything, return changed
//! paths" behavior — it routes through the reconciler internally so all
//! callers benefit from named backups and manifest updates.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use toml_edit::DocumentMut;

use super::manifest::{hash_managed_entries, now_seconds, ClientManifest, ClientManifestEntry};
use super::{client_args, client_path, vscode_settings_path, ClientKind};
use crate::error::{GatewayError, Result};
use crate::native::NativeInstallPaths;

/// Configuration for a single reconciler run. Mirrors `ApplyConfigOptions`
/// but adds the manifest + backup directories so apply() can update them.
#[derive(Debug, Clone)]
pub struct ReconcilerOptions {
    pub clients: Vec<ClientKind>,
    pub servers: Vec<String>,
    pub bridge_bin: PathBuf,
    pub mcp_dir: PathBuf,
    pub state_file: PathBuf,
    /// Server names to *remove* from managed entries even if not in `servers`.
    /// The legacy contract dedupes `mcp-gateway` here so old wrapper entries
    /// are reaped; the reconciler keeps that behavior for backward compat.
    pub dedupe: Vec<String>,
    pub home: PathBuf,
    pub manifest_path: PathBuf,
    pub backups_dir: PathBuf,
}

impl ReconcilerOptions {
    /// Convenience constructor that derives the manifest + backups paths from
    /// the canonical `NativeInstallPaths` layout under `$HOME`.
    pub fn from_native(
        clients: Vec<ClientKind>,
        servers: Vec<String>,
        paths: &NativeInstallPaths,
        dedupe: Vec<String>,
        home: PathBuf,
    ) -> Self {
        Self {
            clients,
            servers,
            bridge_bin: paths.bin_dir.join("mcp-gateway-bridge"),
            mcp_dir: paths.mcp_dir.clone(),
            state_file: paths.state_file.clone(),
            dedupe,
            home,
            manifest_path: paths.root.join("state/client-manifest.json"),
            backups_dir: paths.backups_dir.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientConfigDiff {
    pub per_client: Vec<ClientDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientDiff {
    pub client: String,
    pub path: PathBuf,
    pub exists: bool,
    /// Managed entries that will be added by the next apply.
    pub added: Vec<String>,
    /// Managed entries we previously wrote that will be removed.
    pub removed: Vec<String>,
    /// Managed entries whose value/args will change.
    pub modified: Vec<EntryChange>,
    /// Names of entries we will leave untouched because the user added them.
    pub unmanaged: Vec<String>,
    /// Names of managed entries that were edited outside the gateway.
    pub drift: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryChange {
    pub name: String,
    pub before: Value,
    pub after: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientStatus {
    pub client: String,
    pub path: PathBuf,
    pub exists: bool,
    pub managed: bool,
    pub last_applied_ts: Option<u64>,
    pub managed_servers: Vec<String>,
    pub drift: ClientDriftStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientDriftStatus {
    /// We've never written this client config, so there's nothing to drift from.
    Unmanaged,
    /// The managed slice matches what we last wrote.
    InSync,
    /// The managed slice was edited externally since our last apply.
    Drifted,
}

/// The set of (key, value) pairs the gateway *wants* to write for a given
/// client. Computed once per client during `plan()` so both diff and apply
/// stay in sync.
#[derive(Debug, Clone)]
pub struct ManagedEntry {
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub changed_paths: Vec<PathBuf>,
    /// If a backup directory was created (i.e. at least one file changed),
    /// its absolute path is returned so callers can surface it.
    pub backup_dir: Option<PathBuf>,
    pub last_applied_ts: u64,
}

pub struct ClientConfigReconciler {
    options: ReconcilerOptions,
}

impl ClientConfigReconciler {
    pub fn new(options: ReconcilerOptions) -> Self {
        Self { options }
    }

    pub fn options(&self) -> &ReconcilerOptions {
        &self.options
    }

    /// Inspect every client config, reporting managed/in-sync/drift status.
    pub fn status(&self) -> Result<Vec<ClientStatus>> {
        let manifest = ClientManifest::load(&self.options.manifest_path)?;
        let mut out = Vec::with_capacity(self.options.clients.len());
        for client in &self.options.clients {
            let id = client.config_name().to_string();
            let path = client_path(*client, &self.options.home);
            let exists = path.exists();
            let manifest_entry = manifest.entry(&id);
            let (drift, last_applied_ts, managed_servers) = match manifest_entry {
                None => (ClientDriftStatus::Unmanaged, None, Vec::new()),
                Some(entry) => {
                    let drift_status = if exists {
                        let managed_payload = read_managed_payload(*client, &path, entry)?;
                        if hash_managed_entries(&managed_payload) == entry.entries_hash {
                            ClientDriftStatus::InSync
                        } else {
                            ClientDriftStatus::Drifted
                        }
                    } else {
                        ClientDriftStatus::Drifted
                    };
                    (
                        drift_status,
                        Some(entry.last_applied_ts),
                        entry.managed_servers.clone(),
                    )
                }
            };
            out.push(ClientStatus {
                client: id,
                path,
                exists,
                managed: manifest_entry.is_some(),
                last_applied_ts,
                managed_servers,
                drift,
            });
        }
        Ok(out)
    }

    /// Produce a structured diff without touching disk. Suitable for previews
    /// before applying client configuration changes.
    pub fn plan(&self) -> Result<ClientConfigDiff> {
        super::validate_server_names(&self.options.servers)?;
        let manifest = ClientManifest::load(&self.options.manifest_path)?;
        let desired = self.desired_entries_per_client();
        let mut per_client = Vec::with_capacity(self.options.clients.len());
        for client in &self.options.clients {
            let id = client.config_name().to_string();
            let path = client_path(*client, &self.options.home);
            let exists = path.exists();
            let desired_for_client = desired.get(client).cloned().unwrap_or_default();
            let manifest_entry = manifest.entry(&id);

            let current = if exists {
                load_servers_object(*client, &path)?
            } else {
                BTreeMap::new()
            };

            // Names we wrote last time, if any. Used to classify
            // current entries into managed vs unmanaged buckets.
            let previously_managed: Vec<String> = manifest_entry
                .map(|e| e.managed_servers.clone())
                .unwrap_or_default();
            let previously_managed_set: std::collections::HashSet<&String> =
                previously_managed.iter().collect();

            let desired_names: std::collections::HashSet<&String> =
                desired_for_client.iter().map(|e| &e.name).collect();

            // ADDED = in desired, not present in current.
            // REMOVED = previously managed, no longer in desired.
            // MODIFIED = in both, value differs.
            // UNMANAGED = in current, never managed by us.
            // DRIFT = managed by us but the on-disk value doesn't match what we wrote.
            let mut added = Vec::new();
            let mut modified = Vec::new();
            let mut removed = Vec::new();
            let mut unmanaged: Vec<String> = current
                .keys()
                .filter(|k| {
                    !desired_names.contains(*k)
                        && !previously_managed_set.contains(*k)
                        && *k != "mcp-gateway"
                        && !self.options.dedupe.iter().any(|d| d == *k)
                })
                .cloned()
                .collect();
            unmanaged.sort();

            for entry in &desired_for_client {
                match current.get(&entry.name) {
                    None => added.push(entry.name.clone()),
                    Some(existing) if existing == &entry.value => {}
                    Some(existing) => modified.push(EntryChange {
                        name: entry.name.clone(),
                        before: existing.clone(),
                        after: entry.value.clone(),
                    }),
                }
            }

            for prev in &previously_managed {
                if !desired_names.contains(prev) {
                    removed.push(prev.clone());
                }
            }

            // Drift: previously-managed entries that exist in current but
            // whose hash diverges from the recorded one. We surface them as a
            // separate bucket so the UI can warn before clobbering them.
            let drift = if let Some(entry) = manifest_entry {
                if exists {
                    let managed_payload = read_managed_payload(*client, &path, entry)?;
                    if hash_managed_entries(&managed_payload) != entry.entries_hash {
                        // Identify which names actually drifted by hashing per-entry.
                        previously_managed
                            .iter()
                            .filter(|name| {
                                current
                                    .get(*name)
                                    .map(|v| {
                                        !desired_for_client
                                            .iter()
                                            .any(|d| &d.name == *name && &d.value == v)
                                    })
                                    .unwrap_or(false)
                            })
                            .cloned()
                            .collect()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };

            per_client.push(ClientDiff {
                client: id,
                path,
                exists,
                added,
                removed,
                modified,
                unmanaged,
                drift,
            });
        }
        Ok(ClientConfigDiff { per_client })
    }

    /// Apply pending changes, creating a single named backup directory at
    /// `~/.mcp-gateway/backups/<UTC>-<reason>/` containing every file we
    /// overwrote, and update the manifest with the new managed hashes.
    pub fn apply(&self, reason: &str) -> Result<ApplyOutcome> {
        super::validate_server_names(&self.options.servers)?;
        let now = now_seconds().unwrap_or_default();
        let backup_dir = self.options.backups_dir.join(format!(
            "{}-{}",
            timestamp_compact(now),
            sanitize(reason)
        ));

        let mut changed_paths = Vec::new();
        let mut manifest = ClientManifest::load(&self.options.manifest_path)?;
        let mut backup_created = false;

        // Write the per-server shims first; if any changed, snapshot them too.
        for server in &self.options.servers {
            let path = self.options.mcp_dir.join(server);
            let content = super::render_server_shim(
                &self.options.bridge_bin,
                &self.options.state_file,
                server,
            );
            if write_with_backup(&path, &content, &backup_dir, &mut backup_created)? {
                super::make_executable(&path)?;
                changed_paths.push(path);
            }
        }

        let desired = self.desired_entries_per_client();

        for client in &self.options.clients {
            let id = client.config_name().to_string();
            let path = client_path(*client, &self.options.home);
            let desired_for_client = desired.get(client).cloned().unwrap_or_default();
            let rendered = render_client_config(
                *client,
                &path,
                &self.options.mcp_dir,
                &self.options.servers,
                &self.options.dedupe,
            )?;
            if write_with_backup(&path, &rendered, &backup_dir, &mut backup_created)? {
                changed_paths.push(path.clone());
            }
            super::validate_client_file(*client, &path, &rendered, false)?;

            // VSCode also writes an auxiliary settings.json that enables MCP autostart.
            if matches!(client, ClientKind::Vscode) {
                let settings_path = vscode_settings_path(&self.options.home);
                let settings = super::render_vscode_settings(
                    super::read_optional(&settings_path)?.as_deref(),
                )?;
                if write_with_backup(&settings_path, &settings, &backup_dir, &mut backup_created)? {
                    super::validate_json_file(&settings_path, &settings, false)?;
                    changed_paths.push(settings_path);
                }
            }
            if matches!(client, ClientKind::Antigravity) {
                changed_paths.extend(super::remove_antigravity_gateway_cache(
                    &self.options.home,
                    false,
                )?);
            }

            let managed_payload = json!(desired_for_client
                .iter()
                .map(|e| (e.name.clone(), e.value.clone()))
                .collect::<BTreeMap<_, _>>());
            manifest.upsert(
                id.clone(),
                ClientManifestEntry {
                    path: path.clone(),
                    managed_servers: desired_for_client.iter().map(|e| e.name.clone()).collect(),
                    entries_hash: hash_managed_entries(&managed_payload),
                    last_applied_ts: now,
                },
            );
        }

        manifest.save(&self.options.manifest_path)?;

        Ok(ApplyOutcome {
            changed_paths,
            backup_dir: backup_created.then_some(backup_dir),
            last_applied_ts: now,
        })
    }

    fn desired_entries_per_client(&self) -> BTreeMap<ClientKind, Vec<ManagedEntry>> {
        let mut by_client = BTreeMap::new();
        for client in &self.options.clients {
            let entries = self
                .options
                .servers
                .iter()
                .map(|server| ManagedEntry {
                    name: server.clone(),
                    value: managed_entry_value(*client, &self.options.mcp_dir, server),
                })
                .collect();
            by_client.insert(*client, entries);
        }
        by_client
    }
}

fn managed_entry_value(client: ClientKind, mcp_dir: &Path, server: &str) -> Value {
    let command = mcp_dir.join(server).to_string_lossy().to_string();
    let args = client_args(Some(client.config_name()));
    match client {
        ClientKind::Codex => json!({ "command": command, "args": args }),
        ClientKind::ClaudeCode | ClientKind::ClaudeDesktop | ClientKind::Antigravity => {
            json!({ "command": command, "args": args })
        }
        ClientKind::Vscode => json!({ "type": "stdio", "command": command, "args": args }),
    }
}

fn render_client_config(
    client: ClientKind,
    path: &Path,
    mcp_dir: &Path,
    servers: &[String],
    dedupe: &[String],
) -> Result<String> {
    let existing = super::read_optional(path)?;
    let id = Some(client.config_name());
    match client {
        ClientKind::Codex => {
            super::codex::render_codex_config(existing.as_deref(), mcp_dir, servers, dedupe, id)
        }
        ClientKind::ClaudeCode | ClientKind::ClaudeDesktop | ClientKind::Antigravity => {
            super::render_json_config(existing.as_deref(), mcp_dir, servers, dedupe, id)
        }
        ClientKind::Vscode => {
            super::render_vscode_mcp_config(existing.as_deref(), mcp_dir, servers, dedupe, id)
        }
    }
}

/// Walks the managed slice of `path` and returns a stable JSON object
/// containing only the entries the gateway is responsible for (per
/// `manifest.managed_servers`). Used to compute drift hashes against what we
/// last wrote.
fn read_managed_payload(
    client: ClientKind,
    path: &Path,
    entry: &ClientManifestEntry,
) -> Result<Value> {
    let object = load_servers_object(client, path)?;
    let mut filtered = BTreeMap::new();
    for name in &entry.managed_servers {
        if let Some(value) = object.get(name) {
            filtered.insert(name.clone(), value.clone());
        }
    }
    Ok(json!(filtered))
}

/// Load the `mcpServers` / `mcp_servers` / `servers` object from each config
/// format as a plain map of (name -> JSON value). Returns an empty map when
/// the section is missing.
fn load_servers_object(client: ClientKind, path: &Path) -> Result<BTreeMap<String, Value>> {
    let Some(text) = super::read_optional(path)? else {
        return Ok(BTreeMap::new());
    };
    match client {
        ClientKind::Codex => {
            if text.trim().is_empty() {
                return Ok(BTreeMap::new());
            }
            let doc: DocumentMut = text.parse()?;
            let Some(table) = doc.get("mcp_servers").and_then(|item| item.as_table()) else {
                return Ok(BTreeMap::new());
            };
            let mut out = BTreeMap::new();
            for (name, item) in table.iter() {
                let value = toml_item_to_json(item);
                out.insert(name.to_string(), value);
            }
            Ok(out)
        }
        ClientKind::Vscode => extract_json_servers(&text, "servers"),
        ClientKind::ClaudeCode | ClientKind::ClaudeDesktop | ClientKind::Antigravity => {
            extract_json_servers(&text, "mcpServers")
        }
    }
}

fn extract_json_servers(text: &str, key: &str) -> Result<BTreeMap<String, Value>> {
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let root: Value = serde_json::from_str(text)?;
    let Some(servers) = root.get(key).and_then(Value::as_object) else {
        return Ok(BTreeMap::new());
    };
    Ok(servers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect())
}

fn toml_item_to_json(item: &toml_edit::Item) -> Value {
    // toml_edit -> serde_json via the stringified TOML; we want a comparable
    // JSON value but exact field-by-field equality matters less than the
    // hash, which is stable as long as the serialization is.
    match item {
        toml_edit::Item::Value(value) => toml_value_to_json(value),
        toml_edit::Item::Table(table) => {
            let mut map = serde_json::Map::new();
            for (key, item) in table.iter() {
                map.insert(key.to_string(), toml_item_to_json(item));
            }
            Value::Object(map)
        }
        toml_edit::Item::ArrayOfTables(tables) => {
            let arr: Vec<Value> = tables
                .iter()
                .map(|table| {
                    let mut map = serde_json::Map::new();
                    for (key, item) in table.iter() {
                        map.insert(key.to_string(), toml_item_to_json(item));
                    }
                    Value::Object(map)
                })
                .collect();
            Value::Array(arr)
        }
        toml_edit::Item::None => Value::Null,
    }
}

fn toml_value_to_json(value: &toml_edit::Value) -> Value {
    use toml_edit::Value as Tv;
    match value {
        Tv::String(s) => Value::String(s.value().clone()),
        Tv::Integer(i) => Value::Number((*i.value()).into()),
        Tv::Float(f) => serde_json::Number::from_f64(*f.value())
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Tv::Boolean(b) => Value::Bool(*b.value()),
        Tv::Datetime(dt) => Value::String(dt.to_string()),
        Tv::Array(arr) => Value::Array(arr.iter().map(toml_value_to_json).collect()),
        Tv::InlineTable(table) => {
            let mut map = serde_json::Map::new();
            for (key, item) in table.iter() {
                map.insert(key.to_string(), toml_value_to_json(item));
            }
            Value::Object(map)
        }
    }
}

fn write_with_backup(
    path: &Path,
    content: &str,
    backup_dir: &Path,
    backup_created: &mut bool,
) -> Result<bool> {
    let existing = super::read_optional(path)?;
    if existing.as_deref() == Some(content) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(existing_bytes) = existing.as_deref() {
        ensure_backup_dir(backup_dir, backup_created)?;
        let backup_file = backup_dir.join(backup_relative_name(path));
        if let Some(parent) = backup_file.parent() {
            fs::create_dir_all(parent)?;
        }
        write_owner_only_file(&backup_file, existing_bytes.as_bytes())?;
    }
    fs::write(path, content)?;
    Ok(true)
}

fn ensure_backup_dir(backup_dir: &Path, backup_created: &mut bool) -> Result<()> {
    if *backup_created {
        return Ok(());
    }
    fs::create_dir_all(backup_dir)?;
    set_owner_only_dir(backup_dir)?;
    if let Some(parent) = backup_dir.parent() {
        set_owner_only_dir(parent)?;
    }
    *backup_created = true;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn write_owner_only_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_owner_only_file(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes)?;
    Ok(())
}

fn backup_relative_name(path: &Path) -> String {
    // Hash the absolute path into a short prefix so two files with the same
    // basename (e.g. multiple .claude.json across machines/tests) don't
    // collide inside the backup dir.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    let prefix = format!("{:08x}", hasher.finish() as u32);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config".to_string());
    format!("{prefix}-{name}")
}

/// Public alias for callers that need timestamps to sort lexicographically
/// next to the matching backup directory.
pub fn timestamp_for_audit(secs: u64) -> String {
    timestamp_compact(secs)
}

fn timestamp_compact(secs: u64) -> String {
    // Produce a sortable UTC stamp without pulling chrono. Format:
    //   YYYY-MM-DDTHH-MM-SSZ
    let (year, month, day, hour, min, sec) = epoch_to_components(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}-{min:02}-{sec:02}Z")
}

/// Convert a UNIX epoch seconds value to (year, month, day, hour, minute, second).
/// Accurate for the Gregorian calendar, leap years included; sufficient for
/// filesystem timestamps in user-facing backup names.
fn epoch_to_components(secs: u64) -> (i64, u8, u8, u8, u8, u8) {
    const SECS_PER_DAY: u64 = 86_400;
    let days = (secs / SECS_PER_DAY) as i64;
    let sod = secs % SECS_PER_DAY;
    let hour = (sod / 3600) as u8;
    let min = ((sod % 3600) / 60) as u8;
    let sec = (sod % 60) as u8;
    let (year, month, day) = civil_from_days(days);
    (year, month, day, hour, min, sec)
}

/// Howard Hinnant's date algorithm — days since the UNIX epoch -> civil date.
fn civil_from_days(z: i64) -> (i64, u8, u8) {
    let z = z + 719_468; // shift to 0000-03-01 epoch
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

fn sanitize(reason: &str) -> String {
    let mut out = String::with_capacity(reason.len());
    for ch in reason.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch == ' ' || ch == '/' {
            out.push('-');
        }
    }
    if out.is_empty() {
        out.push_str("apply");
    }
    out
}

#[allow(dead_code)]
fn _gateway_error_unused(_: GatewayError) {} // keep import path stable

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_compact_known_values() {
        // 0 → 1970-01-01T00:00:00Z
        assert_eq!(timestamp_compact(0), "1970-01-01T00-00-00Z");
        // 1_700_000_000 → 2023-11-14T22:13:20Z (known epoch)
        assert_eq!(timestamp_compact(1_700_000_000), "2023-11-14T22-13-20Z");
    }

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        assert_eq!(sanitize("apply clients"), "apply-clients");
        assert_eq!(sanitize("install/context7"), "install-context7");
        // `.` is dropped entirely (no path traversal can leak into the
        // backup folder name); `/` becomes `-`. "../sneaky" -> "-sneaky".
        assert_eq!(sanitize("../sneaky"), "-sneaky");
        // Pure path-traversal collapses to a single "-", which is non-empty
        // but still safe; the timestamp prefix carries the uniqueness.
        assert!(!sanitize("../").is_empty());
        // Empty input still produces a fallback so backup dirs never collide.
        assert_eq!(sanitize(""), "apply");
    }
}
