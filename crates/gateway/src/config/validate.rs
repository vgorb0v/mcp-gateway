use std::collections::BTreeSet;
use std::net::SocketAddr;

use regex::Regex;

use crate::error::{GatewayError, Result};

pub fn validate_identifier(kind: &str, value: &str) -> Result<()> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(GatewayError::Config(format!("{kind} id must not be empty")));
    };
    let valid = first.is_ascii_alphanumeric()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if !valid {
        return Err(GatewayError::Config(format!(
            "{kind} id '{value}' is not path-safe; use ASCII letters, digits, '-' or '_'"
        )));
    }
    Ok(())
}

pub(super) fn validate_loopback_listen_addr(listen: &str) -> Result<()> {
    let addr: SocketAddr = listen.parse().map_err(|err| {
        GatewayError::Config(format!(
            "listen address '{listen}' must be an IP socket address: {err}"
        ))
    })?;
    if !addr.ip().is_loopback() {
        return Err(GatewayError::Config(format!(
            "listen address '{listen}' must be loopback-only; use 127.0.0.1 or ::1"
        )));
    }
    Ok(())
}

pub(super) fn expand_env_placeholders(text: &str) -> Result<String> {
    let re = Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid env regex");
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in re.captures_iter(text) {
        let m = caps.get(0).expect("whole match");
        out.push_str(&text[last..m.start()]);
        let key = &caps[1];
        let value = std::env::var(key).map_err(|_| {
            GatewayError::Config(format!(
                "environment variable '{key}' is required by config"
            ))
        })?;
        out.push_str(&value);
        last = m.end();
    }
    out.push_str(&text[last..]);
    Ok(out)
}

pub(super) fn reject_duplicate_keys(text: &str, section: &str) -> Result<()> {
    let mut in_section = false;
    let mut section_indent = 0usize;
    let mut seen = BTreeSet::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if !in_section {
            if trimmed == format!("{section}:") {
                in_section = true;
                section_indent = indent;
            }
            continue;
        }
        if indent <= section_indent {
            break;
        }
        if indent == section_indent + 2 && trimmed.ends_with(':') {
            let key = trimmed
                .trim_end_matches(':')
                .trim_matches('"')
                .trim_matches('\'');
            if !seen.insert(key.to_string()) {
                return Err(GatewayError::Config(format!(
                    "duplicate {section} entry '{key}'"
                )));
            }
        }
    }

    Ok(())
}
