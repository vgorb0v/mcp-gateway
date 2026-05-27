use mcp_gateway::logs::LogStore;
use mcp_gateway::security::Redactor;

#[test]
fn redactor_masks_key_value_secret_patterns() {
    let redactor = Redactor::default();

    let redacted = redactor.redact(
        "stderr: API_TOKEN=FAKE_TEST_TOKEN_DO_NOT_USE password: hunter2 cookie = session-cookie",
    );

    assert!(!redacted.contains("FAKE_TEST_TOKEN_DO_NOT_USE"));
    assert!(!redacted.contains("hunter2"));
    assert!(!redacted.contains("session-cookie"));
    assert!(redacted.matches("[REDACTED]").count() >= 3);
}

#[test]
fn redactor_masks_known_env_values_and_url_tokens() {
    let redactor = Redactor::new(["FAKE_TEST_TOKEN_DO_NOT_USE".to_string()]);

    let redacted = redactor.redact(
        "fetch https://api.example.test/v1?access_token=abc123&safe=ok with FAKE_TEST_TOKEN_DO_NOT_USE",
    );

    assert!(!redacted.contains("FAKE_TEST_TOKEN_DO_NOT_USE"));
    assert!(!redacted.contains("abc123"));
    assert!(redacted.contains("safe=ok"));
}

#[test]
fn log_store_serves_redacted_lines() {
    let logs = LogStore::new(Redactor::new(["FAKE_TEST_TOKEN_DO_NOT_USE".to_string()]));

    logs.append(
        "example",
        "stderr",
        "backend failed with token=FAKE_TEST_TOKEN_DO_NOT_USE",
    );

    let entries = logs.get("example");
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].line.contains("FAKE_TEST_TOKEN_DO_NOT_USE"));
    assert!(entries[0].line.contains("[REDACTED]"));
}
