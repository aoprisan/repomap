//! Temporal coupling mined from git history: files that change in the same
//! commits as a target file. Static edges can't see this coupling (a schema
//! file and its DAO, a config and the code reading it, mirrored client/server
//! types) — commit history can, and it's exactly the "don't forget to also
//! update…" signal an agent needs before editing.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Result};
use rusqlite::Connection;

/// Commits touching more than this many files are skipped: mass renames and
/// formatting sweeps assert coupling between everything and everything, which
/// is noise, not signal. (The same cutoff is standard in the change-coupling
/// literature.)
const MAX_COMMIT_FILES: usize = 30;

/// Marker separating commits in the mined log. \x01 never appears in paths.
const COMMIT_MARK: &str = "\x01";

pub fn cochange(conn: &Connection, root: &Path, file: &str, commits: usize, k: usize) -> Result<()> {
    let target = resolve_target(conn, root, file)?;
    let log = git_log(root, commits)?;
    let parsed = parse_log(&log);
    let (target_commits, coupled) = couple(&parsed, &target);

    if target_commits == 0 {
        eprintln!(
            "no commits touching '{target}' in the last {} mined ({} requested)",
            parsed.len(),
            commits
        );
        return Ok(());
    }

    // Drop partners that no longer exist in the working tree — coupling to a
    // deleted file is history trivia, not a pending edit.
    let mut rows: Vec<(String, usize)> = coupled
        .into_iter()
        .filter(|(p, _)| root.join(p).exists())
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    if rows.is_empty() {
        eprintln!("no co-change partners for '{target}' ({target_commits} commits mined)");
        return Ok(());
    }
    for (path, together) in rows.iter().take(k) {
        let pct = *together as f64 / target_commits as f64 * 100.0;
        println!("{path}  {together}/{target_commits} commits ({pct:.0}%)");
    }
    Ok(())
}

/// Resolve the user's file argument to one repo-relative path: exact indexed
/// path, unique indexed suffix, or — for unindexed files like `.sql`/`.yml`
/// that co-change is still useful for — a path that simply exists on disk.
fn resolve_target(conn: &Connection, root: &Path, file: &str) -> Result<String> {
    let exact: Option<String> = conn
        .query_row("SELECT path FROM files WHERE path = ?1", [file], |r| r.get(0))
        .ok();
    if let Some(p) = exact {
        return Ok(p);
    }
    let mut stmt = conn.prepare("SELECT path FROM files WHERE path LIKE '%' || ?1 ORDER BY path")?;
    let matches: Vec<String> = stmt
        .query_map([file], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap()),
        0 => {
            if root.join(file).is_file() {
                Ok(file.trim_start_matches("./").to_string())
            } else {
                bail!("no such file '{file}' (not indexed, and not on disk under the root)")
            }
        }
        _ => bail!("ambiguous file '{file}': matches {}", matches.join(", ")),
    }
}

/// Mine the last `commits` non-merge commits as `\x01`-separated blocks of
/// changed paths. Shells out to `git` — history is not derivable from the
/// working tree, and reimplementing pack/delta reading is not worth it.
fn git_log(root: &Path, commits: usize) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "log",
            "--name-only",
            "--no-merges",
            &format!("--pretty=format:{COMMIT_MARK}"),
            "-n",
            &commits.to_string(),
        ])
        .output();
    let out = match out {
        Ok(o) => o,
        Err(_) => bail!("git not found on PATH (cochange mines commit history)"),
    };
    if !out.status.success() {
        bail!(
            "git log failed under '{}' — is it a git repository? ({})",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Split the mined log into one Vec of changed paths per commit.
fn parse_log(log: &str) -> Vec<Vec<String>> {
    log.split(COMMIT_MARK)
        .map(|block| {
            block
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|files| !files.is_empty())
        .collect()
}

/// Count, over all mined commits, how often `target` was changed and how
/// often each other path was changed alongside it. Bulk commits are skipped
/// entirely (see `MAX_COMMIT_FILES`).
fn couple(commits: &[Vec<String>], target: &str) -> (usize, HashMap<String, usize>) {
    let mut target_commits = 0usize;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for files in commits {
        if files.len() > MAX_COMMIT_FILES {
            continue;
        }
        if !files.iter().any(|f| f == target) {
            continue;
        }
        target_commits += 1;
        for f in files {
            if f != target {
                *counts.entry(f.clone()).or_insert(0) += 1;
            }
        }
    }
    (target_commits, counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commits(spec: &[&[&str]]) -> Vec<Vec<String>> {
        spec.iter()
            .map(|c| c.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn parse_log_splits_commits_on_the_marker() {
        let log = "\x01\na.rs\nb.rs\n\n\x01\na.rs\n";
        assert_eq!(
            parse_log(log),
            vec![vec!["a.rs".to_string(), "b.rs".to_string()], vec!["a.rs".to_string()]]
        );
    }

    #[test]
    fn parse_log_of_empty_history_is_empty() {
        assert!(parse_log("").is_empty());
        assert!(parse_log("\x01\n").is_empty());
    }

    #[test]
    fn couple_counts_co_occurrences_with_the_target() {
        let cs = commits(&[
            &["a.rs", "b.rs"],
            &["a.rs", "b.rs", "c.rs"],
            &["a.rs"],
            &["b.rs", "c.rs"], // no target: contributes nothing
        ]);
        let (n, counts) = couple(&cs, "a.rs");
        assert_eq!(n, 3);
        assert_eq!(counts.get("b.rs"), Some(&2));
        assert_eq!(counts.get("c.rs"), Some(&1));
        assert_eq!(counts.get("a.rs"), None, "target never couples to itself");
    }

    #[test]
    fn couple_skips_bulk_commits() {
        let big: Vec<String> = (0..MAX_COMMIT_FILES + 1).map(|i| format!("f{i}.rs")).collect();
        let mut cs = vec![big.clone()];
        cs[0].push("a.rs".into());
        cs.push(vec!["a.rs".into(), "b.rs".into()]);
        let (n, counts) = couple(&cs, "a.rs");
        assert_eq!(n, 1, "the bulk commit is ignored even though it touches the target");
        assert_eq!(counts.get("f0.rs"), None);
        assert_eq!(counts.get("b.rs"), Some(&1));
    }

    /// End-to-end against a real (freshly created) git repository. Skips
    /// quietly when git isn't available in the environment.
    #[test]
    fn mines_a_real_git_repository() {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("git unavailable; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
        };
        git(&["init", "-q"]);
        // Commit 1: a + b together. Commit 2: a alone. Commit 3: b alone.
        std::fs::write(root.join("a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(root.join("b.rs"), "pub fn b() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        std::fs::write(root.join("a.rs"), "pub fn a() { /*2*/ }\n").unwrap();
        git(&["commit", "-aqm", "two"]);
        std::fs::write(root.join("b.rs"), "pub fn b() { /*3*/ }\n").unwrap();
        git(&["commit", "-aqm", "three"]);

        let log = git_log(root, 100).unwrap();
        let parsed = parse_log(&log);
        assert_eq!(parsed.len(), 3);
        let (n, counts) = couple(&parsed, "a.rs");
        assert_eq!(n, 2);
        assert_eq!(counts.get("b.rs"), Some(&1));
    }

    #[test]
    fn git_log_fails_helpfully_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let err = git_log(dir.path(), 10);
        // Either git is missing (first branch) or the dir is not a repo; both
        // must surface as a real error, not empty output.
        assert!(err.is_err());
    }
}
