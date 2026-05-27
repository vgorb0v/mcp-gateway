use std::sync::OnceLock;

use regex::Regex;

pub fn is_sensitive_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    ["TOKEN", "SECRET", "KEY", "PASSWORD", "AUTH", "COOKIE"]
        .iter()
        .any(|needle| upper.contains(needle))
}

/// Regex matching common `KEY = value` / `TOKEN: value` style assignments in
/// log lines. Compiled once and reused for every redaction — the previous
/// implementation recompiled on every log line, which is the hottest path in
/// the gateway when backends are chatty.
fn assignment_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?i)(TOKEN|SECRET|KEY|PASSWORD|AUTH|COOKIE)([A-Z0-9_ -]*)(\s*[=:]\s*)([^,&\s}#]+)",
        )
        .expect("valid redaction regex")
    })
}

fn url_secret_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?i)([?&](?:access_token|auth_token|api_key|key|token|secret|password)=)([^&#\s]+)",
        )
        .expect("valid URL secret regex")
    })
}

#[derive(Debug, Clone)]
pub struct Redactor {
    secret_values: Vec<String>,
}

impl Redactor {
    pub fn new(secret_values: impl IntoIterator<Item = String>) -> Self {
        let mut secret_values: Vec<_> = secret_values
            .into_iter()
            .filter(|value| value.len() > 2)
            .collect();
        secret_values.sort();
        secret_values.dedup();
        Self { secret_values }
    }

    pub fn redact(&self, line: &str) -> String {
        let mut redacted = line.to_string();
        for secret in &self.secret_values {
            redacted = redacted.replace(secret, "[REDACTED]");
        }
        let redacted = assignment_regex()
            .replace_all(&redacted, "$1$2$3[REDACTED]")
            .to_string();
        url_secret_regex()
            .replace_all(&redacted, "$1[REDACTED]")
            .to_string()
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new(Vec::<String>::new())
    }
}
