//! Integration tests for the M3 ClientConfigReconciler. These exercise the
//! plan / apply / drift round-trip across a temp $HOME, with real file I/O.

use std::fs;
use std::path::PathBuf;

use mcp_gateway::client_config::{
    ClientConfigReconciler, ClientDriftStatus, ClientKind, ReconcilerOptions,
};

fn fixture(home: &std::path::Path) -> ReconcilerOptions {
    ReconcilerOptions {
        clients: vec![ClientKind::ClaudeCode, ClientKind::Vscode],
        servers: vec!["alpha-tools".to_string(), "beta-tools".to_string()],
        bridge_bin: home.join(".mcp-gateway/bin/mcp-gateway-bridge"),
        mcp_dir: home.join(".mcp-gateway/mcps"),
        state_file: home.join(".mcp-gateway/run/state.json"),
        dedupe: vec!["mcp-gateway".to_string()],
        home: home.to_path_buf(),
        manifest_path: home.join(".mcp-gateway/state/client-manifest.json"),
        backups_dir: home.join(".mcp-gateway/backups"),
    }
}

#[test]
fn plan_reports_added_entries_for_a_fresh_install() {
    let home = tempfile::tempdir().expect("home");
    let reconciler = ClientConfigReconciler::new(fixture(home.path()));
    let diff = reconciler.plan().expect("plan");

    let claude = diff
        .per_client
        .iter()
        .find(|c| c.client == "claude-code")
        .expect("claude entry");
    assert!(!claude.exists, "claude config does not exist yet");
    assert_eq!(claude.added, vec!["alpha-tools", "beta-tools"]);
    assert!(claude.modified.is_empty());
    assert!(claude.removed.is_empty());
    assert!(claude.drift.is_empty());
}

#[test]
fn apply_creates_named_backup_dir_when_overwriting_existing_config() {
    let home = tempfile::tempdir().expect("home");
    let claude_path = home.path().join(".claude.json");
    fs::write(
        &claude_path,
        r#"{"mcpServers":{"my-custom":{"command":"/usr/bin/foo","args":[]}}}"#,
    )
    .unwrap();
    fs::create_dir_all(home.path().join("Library/Application Support/Code/User")).unwrap();
    fs::write(
        home.path()
            .join("Library/Application Support/Code/User/mcp.json"),
        "{}",
    )
    .unwrap();

    let reconciler = ClientConfigReconciler::new(fixture(home.path()));
    let outcome = reconciler.apply("apply-clients").expect("apply");

    // A backup directory must exist under ~/.mcp-gateway/backups/
    let backup_dir = outcome.backup_dir.expect("backup dir created");
    assert!(backup_dir.exists(), "backup dir missing: {backup_dir:?}");
    assert!(
        backup_dir.to_string_lossy().contains("apply-clients"),
        "named with reason: {backup_dir:?}"
    );

    // The pre-write claude.json bytes should be inside that backup dir.
    let backed_up = fs::read_dir(&backup_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect::<Vec<_>>();
    assert!(
        backed_up.iter().any(|p| {
            let bytes = fs::read_to_string(p).unwrap_or_default();
            bytes.contains("my-custom")
        }),
        "backup did not capture pre-apply claude content: {backed_up:?}"
    );
}

#[test]
#[cfg(unix)]
fn backups_are_owner_only_when_existing_config_contains_secrets() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().expect("home");
    let claude_path = home.path().join(".claude.json");
    fs::write(
        &claude_path,
        r#"{"mcpServers":{"my-custom":{"command":"/usr/bin/foo","env":{"API_TOKEN":"secret"}}}}"#,
    )
    .unwrap();
    fs::create_dir_all(home.path().join("Library/Application Support/Code/User")).unwrap();
    fs::write(
        home.path()
            .join("Library/Application Support/Code/User/mcp.json"),
        "{}",
    )
    .unwrap();

    let reconciler = ClientConfigReconciler::new(fixture(home.path()));
    let outcome = reconciler.apply("apply-clients").expect("apply");
    let backup_dir = outcome.backup_dir.expect("backup dir created");
    let secret_backup = fs::read_dir(&backup_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            fs::read_to_string(path)
                .map(|text| text.contains("API_TOKEN"))
                .unwrap_or(false)
        })
        .expect("secret-bearing backup");

    let dir_mode = fs::metadata(&backup_dir).unwrap().permissions().mode() & 0o777;
    let file_mode = fs::metadata(&secret_backup).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "backup directory should be owner-only");
    assert_eq!(file_mode, 0o600, "backup file should be owner-only");
}

#[test]
fn unmanaged_entries_survive_across_applies_and_are_reported_in_plan() {
    let home = tempfile::tempdir().expect("home");
    let claude = home.path().join(".claude.json");
    fs::write(
        &claude,
        r#"{"mcpServers":{"my-custom":{"command":"/usr/bin/foo","args":[]}}}"#,
    )
    .unwrap();
    fs::create_dir_all(home.path().join("Library/Application Support/Code/User")).unwrap();
    fs::write(
        home.path()
            .join("Library/Application Support/Code/User/mcp.json"),
        "{}",
    )
    .unwrap();

    let reconciler = ClientConfigReconciler::new(fixture(home.path()));

    // Plan: my-custom should be flagged as unmanaged, not removed.
    let diff = reconciler.plan().expect("plan");
    let claude_diff = diff
        .per_client
        .iter()
        .find(|c| c.client == "claude-code")
        .unwrap();
    assert!(
        claude_diff.unmanaged.contains(&"my-custom".to_string()),
        "expected my-custom in unmanaged: {claude_diff:?}"
    );

    // Apply, then re-read — the user's entry must still be there.
    reconciler.apply("apply").expect("apply");
    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&claude).unwrap()).unwrap();
    assert!(
        after["mcpServers"]["my-custom"].is_object(),
        "unmanaged my-custom must survive apply: {after}"
    );
    assert!(after["mcpServers"]["alpha-tools"].is_object());
}

#[test]
fn drift_is_detected_when_a_managed_entry_is_hand_edited() {
    let home = tempfile::tempdir().expect("home");
    fs::create_dir_all(home.path().join("Library/Application Support/Code/User")).unwrap();
    fs::write(
        home.path()
            .join("Library/Application Support/Code/User/mcp.json"),
        "{}",
    )
    .unwrap();
    let reconciler = ClientConfigReconciler::new(fixture(home.path()));

    // First apply seeds the manifest.
    reconciler.apply("apply").expect("apply");

    // Hand-edit a managed entry — change its command to something we'd never write.
    let claude = home.path().join(".claude.json");
    let mut root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&claude).unwrap()).unwrap();
    root["mcpServers"]["alpha-tools"]["command"] = serde_json::json!("/tmp/hand-rolled-binary");
    fs::write(&claude, serde_json::to_string_pretty(&root).unwrap()).unwrap();

    let statuses = reconciler.status().expect("status");
    let claude_status = statuses
        .iter()
        .find(|s| s.client == "claude-code")
        .expect("claude status");
    assert_eq!(claude_status.drift, ClientDriftStatus::Drifted);

    let diff = reconciler.plan().expect("plan after edit");
    let claude_diff = diff
        .per_client
        .iter()
        .find(|c| c.client == "claude-code")
        .unwrap();
    assert!(
        claude_diff.drift.contains(&"alpha-tools".to_string()),
        "drift on alpha-tools expected: {claude_diff:?}"
    );
    // The reconciler should also surface the change as a `modified` entry —
    // applying again would overwrite the hand-edit.
    assert!(
        claude_diff
            .modified
            .iter()
            .any(|m| m.name == "alpha-tools" && m.before["command"] == "/tmp/hand-rolled-binary"),
        "modified should describe the divergence: {claude_diff:?}"
    );
}

#[test]
fn status_reports_in_sync_immediately_after_apply() {
    let home = tempfile::tempdir().expect("home");
    fs::create_dir_all(home.path().join("Library/Application Support/Code/User")).unwrap();
    fs::write(
        home.path()
            .join("Library/Application Support/Code/User/mcp.json"),
        "{}",
    )
    .unwrap();
    let reconciler = ClientConfigReconciler::new(fixture(home.path()));

    let pre = reconciler.status().expect("status pre-apply");
    assert!(pre
        .iter()
        .all(|s| matches!(s.drift, ClientDriftStatus::Unmanaged)));

    reconciler.apply("apply").expect("apply");

    let post = reconciler.status().expect("status post-apply");
    assert!(post
        .iter()
        .all(|s| matches!(s.drift, ClientDriftStatus::InSync)));
    let manifest_path: PathBuf = fixture(home.path()).manifest_path;
    assert!(manifest_path.exists(), "manifest should land on disk");
}

#[test]
fn second_apply_with_unchanged_inputs_creates_no_backup() {
    let home = tempfile::tempdir().expect("home");
    fs::create_dir_all(home.path().join("Library/Application Support/Code/User")).unwrap();
    fs::write(
        home.path()
            .join("Library/Application Support/Code/User/mcp.json"),
        "{}",
    )
    .unwrap();
    let reconciler = ClientConfigReconciler::new(fixture(home.path()));

    reconciler.apply("first").expect("first apply");
    let outcome = reconciler.apply("second").expect("second apply");
    assert!(
        outcome.backup_dir.is_none(),
        "idempotent re-apply must not create a backup: {outcome:?}"
    );
    assert!(outcome.changed_paths.is_empty());
}
