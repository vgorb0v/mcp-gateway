use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

#[test]
fn source_tree_has_no_publication_blocking_placeholders_or_local_paths() {
    let root = repo_root();
    let files = collect_files(&root);
    let mut failures = Vec::new();

    for path in files {
        let rel = path.strip_prefix(&root).unwrap_or(&path);
        if should_skip(rel) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for (line_no, line) in text.lines().enumerate() {
            if line.contains(&blocked_placeholder_domain())
                || line.contains(&blocked_private_home_path())
            {
                failures.push(format!("{}:{}: {}", rel.display(), line_no + 1, line));
            }
            if line.contains(&blocked_private_identifier())
                && !allowed_legacy_launchd_reference(rel, line)
            {
                failures.push(format!("{}:{}: {}", rel.display(), line_no + 1, line));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "publication blockers found:\n{}",
        failures.join("\n")
    );
}

#[test]
fn source_tree_has_no_accidental_real_looking_secrets() {
    let root = repo_root();
    let files = collect_files(&root);
    let secret_patterns = [
        Regex::new(r"sk-[A-Za-z0-9]{20,}").unwrap(),
        Regex::new(r"ghp_[A-Za-z0-9_]{20,}").unwrap(),
        Regex::new(r"github_pat_[A-Za-z0-9_]{20,}").unwrap(),
        Regex::new(&format!("TOKEN={}", ["super", "secret", "token"].join("-"))).unwrap(),
    ];
    let mut failures = Vec::new();

    for path in files {
        let rel = path.strip_prefix(&root).unwrap_or(&path);
        if should_skip(rel) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for (line_no, line) in text.lines().enumerate() {
            if secret_patterns.iter().any(|pattern| pattern.is_match(line)) {
                failures.push(format!("{}:{}", rel.display(), line_no + 1));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "real-looking secrets found:\n{}",
        failures.join("\n")
    );
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|name| name.to_str());
                if matches!(name, Some("target" | ".git")) {
                    continue;
                }
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

fn should_skip(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("Cargo.lock" | "repository_hygiene_tests.rs")
    )
}

fn allowed_legacy_launchd_reference(path: &Path, line: &str) -> bool {
    line.contains(&legacy_launchd_label())
        && matches!(
            path.to_string_lossy().as_ref(),
            "crates/gateway/src/native.rs"
                | "crates/gateway/tests/native_tests.rs"
                | "crates/gatewayctl/src/main.rs"
                | "CHANGELOG.md"
        )
}

fn legacy_launchd_label() -> String {
    format!("com.{}{}.mcp-gateway", "vgor", "bov")
}

fn blocked_placeholder_domain() -> String {
    ["example", "invalid"].join(".")
}

fn blocked_private_home_path() -> String {
    ["/Users/", "vgor", "bov"].concat()
}

fn blocked_private_identifier() -> String {
    ["vgor", "bov"].concat()
}
