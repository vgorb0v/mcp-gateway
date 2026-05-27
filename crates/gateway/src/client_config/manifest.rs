//! Persists the gateway's view of "which entries in each client config did *we*
//! write" so future runs can detect drift (a managed entry edited by hand) and
//! distinguish managed entries from ones the user added themselves.
//!
//! Lives at `~/.mcp-gateway/state/client-manifest.json`. Atomically rewritten
//! after every successful `apply()`.

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::Result;

const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClientManifest {
    pub version: u32,
    #[serde(default)]
    pub last_applied_ts: Option<u64>,
    #[serde(default)]
    pub clients: BTreeMap<String, ClientManifestEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClientManifestEntry {
    /// Absolute path to the client config file we wrote (e.g. `~/.claude.json`).
    pub path: PathBuf,
    /// Names of the MCP servers we wrote into that file's `mcpServers`/etc.
    /// section last time `apply()` succeeded.
    pub managed_servers: Vec<String>,
    /// Stable hash of the managed-entry payload last persisted. Drift is
    /// detected by re-reading the file, recomputing this hash, and comparing.
    pub entries_hash: String,
    pub last_applied_ts: u64,
}

impl Default for ClientManifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            last_applied_ts: None,
            clients: BTreeMap::new(),
        }
    }
}

impl ClientManifest {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) if text.trim().is_empty() => Ok(Self::default()),
            Ok(text) => {
                let manifest: Self = serde_json::from_str(&text)?;
                if manifest.version != MANIFEST_VERSION {
                    // Future versions can branch on this; for now, reset.
                    return Ok(Self::default());
                }
                Ok(manifest)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = format!("{}\n", serde_json::to_string_pretty(self)?);
        // Atomic swap so a crash mid-write never leaves a truncated manifest.
        let tmp = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            now_seconds().unwrap_or_default(),
        ));
        fs::write(&tmp, content)?;
        fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn entry(&self, client_id: &str) -> Option<&ClientManifestEntry> {
        self.clients.get(client_id)
    }

    pub fn upsert(&mut self, client_id: impl Into<String>, entry: ClientManifestEntry) {
        let now = entry.last_applied_ts;
        self.clients.insert(client_id.into(), entry);
        self.last_applied_ts = Some(now);
    }
}

/// Stable hash of a JSON payload representing the managed slice of a client
/// config. Uses `DefaultHasher` (FxHash family) — drift detection only needs
/// "did the bytes change?" not cryptographic strength, so we avoid pulling
/// SHA into the workspace just for this.
pub fn hash_managed_entries(payload: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(payload).unwrap_or_else(|_| "null".to_string());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub fn now_seconds() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn manifest_round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("client-manifest.json");
        let mut manifest = ClientManifest::default();
        manifest.upsert(
            "claude-code",
            ClientManifestEntry {
                path: tmp.path().join(".claude.json"),
                managed_servers: vec!["alpha-tools".into(), "beta-tools".into()],
                entries_hash: hash_managed_entries(&json!({"alpha-tools": "x"})),
                last_applied_ts: 1_700_000_000,
            },
        );
        manifest.save(&path).unwrap();
        let loaded = ClientManifest::load(&path).unwrap();
        assert_eq!(loaded, manifest);
        assert_eq!(loaded.last_applied_ts, Some(1_700_000_000));
    }

    #[test]
    fn loading_missing_manifest_returns_empty_default() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = ClientManifest::load(&tmp.path().join("does-not-exist.json")).unwrap();
        assert!(manifest.clients.is_empty());
        assert!(manifest.last_applied_ts.is_none());
    }

    #[test]
    fn hash_is_stable_across_calls_and_different_for_different_payloads() {
        let a = json!({"alpha-tools": {"command": "x", "args": []}});
        let b = json!({"alpha-tools": {"command": "x", "args": []}});
        let c = json!({"alpha-tools": {"command": "y", "args": []}});
        assert_eq!(hash_managed_entries(&a), hash_managed_entries(&b));
        assert_ne!(hash_managed_entries(&a), hash_managed_entries(&c));
    }
}
