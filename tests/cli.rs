use std::process::{Command, Output};

use serde_json::Value;

fn repomap(root: &std::path::Path, db: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_repomap"))
        .arg("--root")
        .arg(root)
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .unwrap()
}

fn json_lines(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn jsonl_is_versioned_structured_and_uses_explicit_no_match_exit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let db = root.join("index.db");
    std::fs::write(root.join("lib.rs"), "pub fn target() {}\n").unwrap();

    let found = repomap(root, &db, &["--format", "jsonl", "find", "target"]);
    assert!(
        found.status.success(),
        "{}",
        String::from_utf8_lossy(&found.stderr)
    );
    let stdout = json_lines(&found.stdout);
    assert_eq!(stdout.len(), 1);
    assert_eq!(stdout[0]["schema_version"], 1);
    assert_eq!(stdout[0]["command"], "find");
    assert_eq!(stdout[0]["type"], "symbol");
    assert_eq!(stdout[0]["data"]["qualified_name"], "target");
    assert_eq!(stdout[0]["data"]["relationship"], "match");
    let refresh = json_lines(&found.stderr);
    assert!(refresh
        .iter()
        .any(|v| { v["type"] == "diagnostic" && v["data"]["code"] == "index_refreshed" }));

    let missing = repomap(root, &db, &["--format", "jsonl", "find", "absent"]);
    assert_eq!(missing.status.code(), Some(3));
    let diagnostics = json_lines(&missing.stderr);
    assert!(diagnostics.iter().any(|v| v["data"]["code"] == "no_match"));
}

#[test]
fn invalid_root_fails_without_opening_or_erasing_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("lib.rs"), "pub fn kept() {}\n").unwrap();
    let db = dir.path().join("shared.db");

    let indexed = repomap(&root, &db, &["index"]);
    assert!(indexed.status.success());
    let missing = dir.path().join("missing");
    let failed = repomap(&missing, &db, &["--format", "jsonl", "index"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(json_lines(&failed.stderr)
        .iter()
        .any(|v| v["data"]["message"]
            .as_str()
            .unwrap()
            .contains("does not exist")));

    let found = repomap(&root, &db, &["--no-refresh", "find", "kept"]);
    assert!(found.status.success());
    assert!(String::from_utf8_lossy(&found.stdout).contains("fn kept"));
}

#[test]
fn allow_stale_returns_results_with_a_distinct_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let db = root.join("index.db");
    std::fs::write(root.join("lib.rs"), "pub fn target() {}\n").unwrap();
    assert!(repomap(root, &db, &["index"]).status.success());
    std::fs::write(root.join("repomap.toml"), "this is not valid toml = [").unwrap();

    let stale = repomap(
        root,
        &db,
        &["--format", "jsonl", "--allow-stale", "find", "target"],
    );
    assert_eq!(stale.status.code(), Some(4));
    assert!(json_lines(&stale.stdout)
        .iter()
        .any(|v| v["type"] == "symbol" && v["data"]["name"] == "target"));
    assert!(json_lines(&stale.stderr)
        .iter()
        .any(|v| v["data"]["code"] == "stale_index"));
}
