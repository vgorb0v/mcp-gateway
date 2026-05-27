use std::net::SocketAddr;

use mcp_gateway::native::{
    read_state_file, render_launch_agent_plist, write_owner_only_file, write_state_file,
    NativeInstallPaths, LAUNCH_AGENT_LABEL, LEGACY_LAUNCH_AGENT_LABEL,
};

#[test]
fn state_file_round_trips_ephemeral_listener_address() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = dir.path().join("run/state.json");
    let addr: SocketAddr = "127.0.0.1:49152".parse().unwrap();

    write_state_file(&state, addr).expect("write state");
    let loaded = read_state_file(&state).expect("read state");

    assert_eq!(loaded.base_url, "http://127.0.0.1:49152");
    assert_eq!(loaded.pid, std::process::id());
}

#[test]
fn state_file_has_owner_only_permissions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = dir.path().join("run/state.json");
    let addr: SocketAddr = "127.0.0.1:49152".parse().unwrap();

    write_state_file(&state, addr).expect("write state");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&state).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "state.json must be 0o600 to protect token");
    }
}

#[test]
fn owner_only_file_writer_sets_private_permissions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let env_file = dir.path().join(".mcp-gateway/env");

    write_owner_only_file(&env_file, "FAKE_TEST_TOKEN=FAKE_TEST_TOKEN_DO_NOT_USE\n")
        .expect("write env file");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&env_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "env files can contain secrets");
    }
}

#[test]
fn state_file_reader_ignores_legacy_extra_fields() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = dir.path().join("run/state.json");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(
        &state,
        r#"{
  "base_url": "http://127.0.0.1:49152",
  "pid": 123,
  "removed_legacy_field": "legacy-value"
}"#,
    )
    .unwrap();

    let loaded = read_state_file(&state).expect("read state");

    assert_eq!(loaded.base_url, "http://127.0.0.1:49152");
    assert_eq!(loaded.pid, 123);
}

#[test]
fn native_paths_include_servers_d_and_backups() {
    let home = tempfile::tempdir().expect("home");
    let paths = NativeInstallPaths::for_home(home.path());

    assert_eq!(
        paths.config_servers_d_dir,
        home.path().join(".mcp-gateway/config/servers.d")
    );
    assert_eq!(paths.backups_dir, home.path().join(".mcp-gateway/backups"));
    assert_eq!(
        paths.audit_log_file,
        home.path().join(".mcp-gateway/audit.log")
    );
}

#[test]
fn native_paths_use_neutral_launchd_label_and_track_legacy_plist() {
    let home = tempfile::tempdir().expect("home");
    let paths = NativeInstallPaths::for_home(home.path());

    assert_eq!(LAUNCH_AGENT_LABEL, "io.github.mcpgateway.daemon");
    assert_eq!(
        paths.launch_agent_file,
        home.path()
            .join("Library/LaunchAgents/io.github.mcpgateway.daemon.plist")
    );
    assert_eq!(
        paths.legacy_launch_agent_file,
        home.path().join(format!(
            "Library/LaunchAgents/{LEGACY_LAUNCH_AGENT_LABEL}.plist"
        ))
    );
}

#[test]
fn launch_agent_plist_uses_user_paths_run_at_load_and_no_docker() {
    let home = tempfile::tempdir().expect("home");
    let paths = NativeInstallPaths::for_home(home.path());
    let plist = render_launch_agent_plist(&paths);

    assert!(plist.contains("<key>RunAtLoad</key>"));
    assert!(plist.contains("<true/>"));
    assert!(plist.contains("io.github.mcpgateway.daemon"));
    assert!(!plist.contains(LEGACY_LAUNCH_AGENT_LABEL));
    assert!(plist.contains("mcp-gateway"));
    assert!(plist.contains("--state-file"));
    // Background resource policy: don't compete with interactive apps.
    assert!(plist.contains("<key>ProcessType</key>"));
    assert!(plist.contains("<string>Background</string>"));
    assert!(plist.contains("<key>LowPriorityIO</key>"));
    assert!(plist.contains("<key>Nice</key>"));
    assert!(plist.contains("<integer>5</integer>"));
    assert!(plist.contains(paths.state_file.to_string_lossy().as_ref()));
    assert!(plist.contains(
        paths
            .logs_dir
            .join("gateway.out.log")
            .to_string_lossy()
            .as_ref()
    ));
    assert!(!plist.contains("docker"));
    assert!(!plist.contains("LaunchDaemons"));
    assert!(!plist.contains("<key>UserName</key>"));
}

#[test]
fn native_paths_include_chrome_for_testing_browser_bundle_root() {
    let home = tempfile::tempdir().expect("home");
    let paths = NativeInstallPaths::for_home(home.path());

    assert_eq!(
        paths.browsers_dir,
        home.path().join(".mcp-gateway/browsers")
    );
    assert_eq!(
        paths.chrome_for_testing_dir,
        home.path().join(".mcp-gateway/browsers/chrome-for-testing")
    );
    assert_eq!(
        paths.capability_cache_file,
        home.path()
            .join(".mcp-gateway/cache/mcp_manifest_cache.json")
    );
}
