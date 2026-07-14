//! Change-aware code intelligence for `repomap changes`.
//!
//! A normal live index only knows the working tree, which means deleted
//! definitions vanish exactly when a reviewer most needs their blast radius.
//! This module compares extracted symbols against a Git base, then joins that
//! semantic diff to both resolved edges and unresolved `edge_raw` references.
//! The latter recovers direct callers of deleted symbols; traversal can then
//! continue through the current graph to select affected tests.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::json;

use crate::git;
use crate::lang::{self, Language, RawSymbol};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, PartialEq, Eq)]
struct ChangedFile {
    status: FileStatus,
    old_path: Option<String>,
    new_path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nature {
    Added,
    Deleted,
    Signature,
    Body,
    Documentation,
    File,
    Renamed,
}

impl Nature {
    fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Signature => "signature",
            Self::Body => "body",
            Self::Documentation => "docs",
            Self::File => "file",
            Self::Renamed => "renamed",
        }
    }

    fn base_risk(self) -> usize {
        match self {
            Self::Documentation => 0,
            Self::Added | Self::Body | Self::File | Self::Renamed => 1,
            Self::Signature => 3,
            Self::Deleted => 4,
        }
    }
}

#[derive(Clone)]
struct Snapshot {
    name: String,
    kind: String,
    start_line: usize,
    end_line: usize,
    signature: String,
    doc: Option<String>,
    body_hash: String,
    is_test: bool,
    public: bool,
    container: Option<String>,
}

struct ChangedSymbol {
    nature: Nature,
    path: String,
    old_path: Option<String>,
    line: usize,
    name: Option<String>,
    kind: Option<String>,
    container: Option<String>,
    signature: String,
    service: String,
    is_test: bool,
    public: bool,
    current: bool,
}

struct Affected {
    file: String,
    line: i64,
    signature: String,
    service: String,
    is_test: bool,
    depth: usize,
    via: String,
    rank: f64,
}

struct Report {
    files: usize,
    changed: Vec<ChangedSymbol>,
    affected: Vec<Affected>,
    direct_counts: HashMap<String, usize>,
}

pub fn run(conn: &Connection, root: &Path, base: &str, depth: usize, k: usize) -> Result<usize> {
    let report = analyze(conn, root, base, depth)?;
    if report.files == 0 {
        crate::output::note(
            "no_changes",
            format!("no working-tree changes against '{base}'"),
        );
        return Ok(0);
    }

    let tests: Vec<&Affected> = report.affected.iter().filter(|a| a.is_test).collect();
    let cross_services: HashMap<&str, bool> = report
        .changed
        .iter()
        .filter_map(|c| c.name.as_deref().map(|n| (n, &c.service)))
        .map(|(name, service)| {
            let crosses = report
                .affected
                .iter()
                .any(|a| a.via == name && a.service != *service);
            (name, crosses)
        })
        .collect();

    let mut order: Vec<(usize, usize)> = report
        .changed
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let direct = c
                .name
                .as_ref()
                .and_then(|n| report.direct_counts.get(n))
                .copied()
                .unwrap_or(0);
            let crosses = c
                .name
                .as_deref()
                .and_then(|n| cross_services.get(n))
                .copied()
                .unwrap_or(false);
            (i, risk(c, direct, crosses))
        })
        .collect();
    order.sort_by(|(ai, ar), (bi, br)| {
        br.cmp(ar).then_with(|| {
            let a = &report.changed[*ai];
            let b = &report.changed[*bi];
            (&a.path, a.line).cmp(&(&b.path, b.line))
        })
    });

    crate::output::emit(
        "changes_summary",
        json!({
            "base": base,
            "files": report.files,
            "semantic_changes": report.changed.len(),
        }),
        format!(
            "changes vs {base}: {} files, {} semantic changes",
            report.files,
            report.changed.len()
        ),
    );
    if !crate::output::is_jsonl() {
        println!("changed:");
    }
    let mut max_risk = 0usize;
    for (i, score) in order.iter().take(k) {
        max_risk = max_risk.max(*score);
        let c = &report.changed[*i];
        let direct = c
            .name
            .as_ref()
            .and_then(|n| report.direct_counts.get(n))
            .copied()
            .unwrap_or(0);
        let crosses = c
            .name
            .as_deref()
            .and_then(|n| cross_services.get(n))
            .copied()
            .unwrap_or(false);
        let mut signals = vec![c.nature.label().to_string(), risk_label(*score).to_string()];
        if c.public && c.nature != Nature::Documentation {
            signals.push("public API".into());
        }
        if direct > 0 {
            signals.push(format!("{direct} direct refs"));
        }
        if crosses {
            signals.push("cross-service".into());
        }
        let at_base = if c.current { "" } else { " at base" };
        crate::output::emit(
            "change",
            json!({
                "file": c.path,
                "old_file": c.old_path,
                "line": c.line,
                "name": c.name,
                "kind": c.kind,
                "container": c.container,
                "signature": c.signature,
                "service": c.service,
                "nature": c.nature.label(),
                "risk": risk_label(*score),
                "risk_score": score,
                "public_api": c.public,
                "direct_references": direct,
                "cross_service": crosses,
                "at_base": !c.current,
            }),
            format!(
                "{}:L{}{}  {}  [{}]  ({})",
                c.path,
                c.line,
                at_base,
                c.signature,
                c.service,
                signals.join(", ")
            ),
        );
    }
    if report.changed.len() > k {
        crate::output::emit(
            "truncation",
            json!({"section": "changes", "omitted": report.changed.len() - k, "limit": k}),
            format!(
                "… and {} more semantic changes (raise -k)",
                report.changed.len() - k
            ),
        );
    }

    let review: Vec<&Affected> = report.affected.iter().filter(|a| !a.is_test).collect();
    if !crate::output::is_jsonl() {
        println!("review surface:");
    }
    if review.is_empty() {
        crate::output::emit(
            "review_summary",
            json!({"graph_linked_callers": 0}),
            "-  no graph-linked callers",
        );
    } else {
        for a in review.iter().take(k) {
            crate::output::emit(
                "affected_symbol",
                json!({
                    "file": a.file,
                    "line": a.line,
                    "signature": a.signature,
                    "service": a.service,
                    "depth": a.depth,
                    "via": a.via,
                }),
                format!(
                    "{}:L{}  {}  [{}]  (depth {}, via {})",
                    a.file, a.line, a.signature, a.service, a.depth, a.via
                ),
            );
        }
        if review.len() > k {
            crate::output::emit(
                "truncation",
                json!({"section": "review", "omitted": review.len() - k, "limit": k}),
                format!(
                    "… and {} more affected symbols (raise -k)",
                    review.len() - k
                ),
            );
        }
    }

    if !crate::output::is_jsonl() {
        println!("tests to run:");
    }
    if tests.is_empty() {
        crate::output::emit(
            "test_summary",
            json!({"graph_linked_tests": 0, "fallback": "service_suite"}),
            "-  no graph-linked tests found; run the service-level suite",
        );
    } else {
        for a in tests.iter().take(k) {
            crate::output::emit(
                "test",
                json!({
                    "file": a.file,
                    "line": a.line,
                    "signature": a.signature,
                    "service": a.service,
                    "depth": a.depth,
                    "via": a.via,
                }),
                format!(
                    "{}:L{}  {}  [{}]  (depth {}, via {})",
                    a.file, a.line, a.signature, a.service, a.depth, a.via
                ),
            );
        }
        if tests.len() > k {
            crate::output::emit(
                "truncation",
                json!({"section": "tests", "omitted": tests.len() - k, "limit": k}),
                format!("… and {} more affected tests (raise -k)", tests.len() - k),
            );
        }
    }

    let affected_services = report
        .affected
        .iter()
        .map(|a| a.service.as_str())
        .collect::<HashSet<_>>()
        .len();
    crate::output::emit(
        "risk_summary",
        json!({
            "risk": risk_label(max_risk),
            "risk_score": max_risk,
            "affected_symbols": report.affected.len(),
            "affected_services": affected_services,
            "linked_tests": tests.len(),
        }),
        format!(
            "change risk: {} ({} affected symbols across {} services; {} linked tests)",
            risk_label(max_risk),
            report.affected.len(),
            affected_services,
            tests.len()
        ),
    );

    Ok(report.changed.len() + report.affected.len())
}

fn risk(c: &ChangedSymbol, direct: usize, crosses: bool) -> usize {
    c.nature.base_risk()
        + usize::from(c.public && c.nature != Nature::Documentation) * 2
        + match direct {
            0 => 0,
            1..=3 => 1,
            _ => 2,
        }
        + usize::from(crosses) * 2
}

fn risk_label(score: usize) -> &'static str {
    match score {
        0..=2 => "low",
        3..=5 => "medium",
        _ => "high",
    }
}

fn analyze(conn: &Connection, root: &Path, base: &str, depth: usize) -> Result<Report> {
    let files = changed_files(root, base)?;
    let prefix = git_prefix(root)?;
    let mut changed = Vec::new();

    for file in &files {
        let display_path = file
            .new_path
            .as_deref()
            .or(file.old_path.as_deref())
            .unwrap_or("-");
        let Some(language) = Language::from_path(Path::new(display_path)) else {
            // Do not decode non-source files: a changed image/database should
            // be reported as review context, not make the whole command fail
            // because it is not UTF-8.
            changed.push(file_level_change(file, display_path, true));
            continue;
        };
        let old_source = match &file.old_path {
            Some(path) if file.status != FileStatus::Added => {
                Some(git_show(root, base, &format!("{prefix}{path}"))?)
            }
            _ => None,
        };
        let new_source = match &file.new_path {
            Some(path) if file.status != FileStatus::Deleted => {
                let bytes = std::fs::read(root.join(path))
                    .with_context(|| format!("read changed file '{path}'"))?;
                Some(String::from_utf8_lossy(&bytes).into_owned())
            }
            _ => None,
        };

        let before = match (old_source.as_deref(), file.old_path.as_deref()) {
            (Some(src), Some(path)) => snapshots(language, path, src)?,
            _ => Vec::new(),
        };
        let after = match (new_source.as_deref(), file.new_path.as_deref()) {
            (Some(src), Some(path)) => snapshots(language, path, src)?,
            _ => Vec::new(),
        };
        compare_file(
            file,
            &before,
            &after,
            old_source.as_deref(),
            new_source.as_deref(),
            &mut changed,
        );

        // Unsupported files and symbol-free source files still matter to a
        // change review. Represent them explicitly instead of silently
        // pretending the diff is empty.
        if before.is_empty()
            && after.is_empty()
            && !changed
                .iter()
                .any(|c| c.path == display_path || c.old_path.as_deref() == Some(display_path))
        {
            changed.push(file_level_change(file, display_path, false));
        }
    }

    for c in &mut changed {
        c.service = service_for_path(conn, &c.path)?;
    }
    let (mut affected, direct_counts) = affected_symbols(conn, &changed, depth)?;
    affected.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| {
                b.rank
                    .partial_cmp(&a.rank)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| (&a.file, a.line).cmp(&(&b.file, b.line)))
    });

    Ok(Report {
        files: files.len(),
        changed,
        affected,
        direct_counts,
    })
}

fn snapshots(language: Language, path: &str, source: &str) -> Result<Vec<Snapshot>> {
    let extracted = language.extract(source)?;
    let lines: Vec<&str> = source.lines().collect();
    let containers: Vec<Option<String>> = extracted
        .symbols
        .iter()
        .enumerate()
        .map(|(i, child)| {
            extracted
                .symbols
                .iter()
                .enumerate()
                .filter(|(j, parent)| {
                    *j != i
                        && parent.start_line <= child.start_line
                        && parent.end_line >= child.end_line
                        && (parent.start_line < child.start_line
                            || parent.end_line > child.end_line)
                })
                .min_by_key(|(_, parent)| parent.end_line - parent.start_line)
                .map(|(_, parent)| parent.name.clone())
        })
        .collect();
    Ok(extracted
        .symbols
        .iter()
        .enumerate()
        .map(|(i, s)| Snapshot {
            name: s.name.clone(),
            kind: s.kind.clone(),
            start_line: s.start_line,
            end_line: s.end_line,
            signature: s.signature.clone(),
            doc: s.doc_first_line.clone(),
            body_hash: own_body_hash(s, &extracted.symbols, &lines),
            is_test: lang::is_test_symbol(path, s),
            public: is_public(language, s),
            container: containers[i].clone(),
        })
        .collect())
}

/// Hash a definition with nested definitions blanked. Otherwise changing one
/// method would also report every containing class/module as a body change.
fn own_body_hash(symbol: &RawSymbol, all: &[RawSymbol], lines: &[&str]) -> String {
    let mut kept = Vec::new();
    for line in symbol.start_line..=symbol.end_line {
        let inside_child = all.iter().any(|child| {
            (child.start_line > symbol.start_line || child.end_line < symbol.end_line)
                && child.start_line <= line
                && line <= child.end_line
        });
        if !inside_child {
            kept.push(lines.get(line.saturating_sub(1)).copied().unwrap_or(""));
        }
    }
    git::blob_hash(kept.join("\n").as_bytes())
}

fn is_public(language: Language, symbol: &RawSymbol) -> bool {
    let sig = symbol.signature.trim_start();
    match language {
        Language::Rust => sig.starts_with("pub ") || sig.starts_with("pub("),
        Language::Typescript | Language::Tsx => {
            sig.starts_with("export ") || sig.starts_with("public ")
        }
        Language::Python => !symbol.name.starts_with('_'),
        // Ruby and Scala declarations are public by default unless a richer
        // visibility analysis proves otherwise.
        Language::Ruby | Language::Scala => true,
    }
}

fn compare_file(
    file: &ChangedFile,
    before: &[Snapshot],
    after: &[Snapshot],
    old_source: Option<&str>,
    new_source: Option<&str>,
    out: &mut Vec<ChangedSymbol>,
) {
    let mut used_after = HashSet::new();
    for old in before {
        let matched = after.iter().enumerate().find(|(i, new)| {
            !used_after.contains(i) && old.name == new.name && old.kind == new.kind
        });
        match matched {
            Some((i, new)) => {
                used_after.insert(i);
                let nature = if old.signature != new.signature {
                    Some(Nature::Signature)
                } else if old.body_hash != new.body_hash {
                    Some(Nature::Body)
                } else if old.doc != new.doc {
                    Some(Nature::Documentation)
                } else {
                    None
                };
                if let Some(nature) = nature {
                    out.push(from_snapshot(
                        nature,
                        file.new_path.as_deref().unwrap_or("-"),
                        file.old_path.clone(),
                        new,
                        true,
                    ));
                }
            }
            None => out.push(from_snapshot(
                Nature::Deleted,
                file.old_path.as_deref().unwrap_or("-"),
                file.old_path.clone(),
                old,
                false,
            )),
        }
    }
    for (i, new) in after.iter().enumerate() {
        if !used_after.contains(&i) {
            out.push(from_snapshot(
                Nature::Added,
                file.new_path.as_deref().unwrap_or("-"),
                file.old_path.clone(),
                new,
                true,
            ));
        }
    }

    if matches!(file.status, FileStatus::Modified | FileStatus::Renamed) {
        let before_outside = outside_symbols_hash(old_source.unwrap_or(""), before);
        let after_outside = outside_symbols_hash(new_source.unwrap_or(""), after);
        if before_outside != after_outside {
            let path = file.new_path.as_deref().unwrap_or("-");
            out.push(file_level_change(file, path, false));
        } else if file.status == FileStatus::Renamed
            && !out.iter().any(|c| {
                c.path == file.new_path.as_deref().unwrap_or("") && c.old_path == file.old_path
            })
        {
            out.push(file_level_change(
                file,
                file.new_path.as_deref().unwrap_or("-"),
                false,
            ));
        }
    }
}

fn from_snapshot(
    nature: Nature,
    path: &str,
    old_path: Option<String>,
    s: &Snapshot,
    current: bool,
) -> ChangedSymbol {
    ChangedSymbol {
        nature,
        path: path.to_string(),
        old_path,
        line: s.start_line,
        name: Some(s.name.clone()),
        kind: Some(s.kind.clone()),
        container: s.container.clone(),
        signature: s.signature.clone(),
        service: String::new(),
        is_test: s.is_test,
        public: s.public,
        current,
    }
}

fn file_level_change(file: &ChangedFile, path: &str, unsupported: bool) -> ChangedSymbol {
    let nature = if file.status == FileStatus::Renamed {
        Nature::Renamed
    } else {
        Nature::File
    };
    let signature = if unsupported {
        "<non-indexed file change>"
    } else {
        "<file-level change outside symbol bodies>"
    };
    ChangedSymbol {
        nature,
        path: path.to_string(),
        old_path: file.old_path.clone(),
        line: 1,
        name: None,
        kind: None,
        container: None,
        signature: signature.into(),
        service: String::new(),
        is_test: false,
        public: false,
        current: file.status != FileStatus::Deleted,
    }
}

fn outside_symbols_hash(source: &str, symbols: &[Snapshot]) -> String {
    let mut covered = vec![false; source.lines().count()];
    for s in symbols {
        for line in s.start_line..=s.end_line {
            if let Some(slot) = covered.get_mut(line.saturating_sub(1)) {
                *slot = true;
            }
        }
    }
    let outside = source
        .lines()
        .enumerate()
        .filter(|(i, _)| !covered[*i])
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
    git::blob_hash(outside.as_bytes())
}

fn affected_symbols(
    conn: &Connection,
    changed: &[ChangedSymbol],
    max_depth: usize,
) -> Result<(Vec<Affected>, HashMap<String, usize>)> {
    let mut reached: HashMap<i64, (usize, String)> = HashMap::new();
    let mut roots = HashSet::new();
    let mut direct_counts = HashMap::new();

    for c in changed {
        let (Some(name), Some(kind)) = (&c.name, &c.kind) else {
            continue;
        };
        let mut direct = HashSet::new();
        if c.current {
            let mut ids =
                conn.prepare("SELECT id FROM symbols WHERE file = ?1 AND name = ?2 AND kind = ?3")?;
            for id in ids
                .query_map(rusqlite::params![c.path, name, kind], |r| {
                    r.get::<_, i64>(0)
                })?
                .filter_map(|r| r.ok())
            {
                roots.insert(id);
                let mut refs =
                    conn.prepare("SELECT DISTINCT src_symbol FROM edges WHERE dst_symbol = ?1")?;
                direct.extend(
                    refs.query_map([id], |r| r.get::<_, i64>(0))?
                        .filter_map(|r| r.ok()),
                );
            }
        } else {
            // Deleted roots have no destination id in the live graph. Reapply
            // the resolver's conservative scoping to raw references: same
            // service, or an explicit import licensing a cross-service link.
            let mut refs = conn.prepare(
                "SELECT DISTINCT er.src_symbol
                 FROM edge_raw er
                 JOIN symbols src ON src.id = er.src_symbol
                 WHERE er.dst_name = ?1
                   AND (src.service = ?2 OR EXISTS (
                       SELECT 1 FROM file_imports fi
                       WHERE fi.file = src.file AND fi.name = er.dst_name
                   ))
                   AND (
                       (er.qualifier IS NOT NULL AND er.qualifier IS ?3)
                       OR (er.qualifier IS NULL AND (?3 IS NULL OR src.container IS ?3))
                   )",
            )?;
            direct.extend(
                refs.query_map(rusqlite::params![name, c.service, c.container], |r| {
                    r.get::<_, i64>(0)
                })?
                .filter_map(|r| r.ok()),
            );
        }
        direct.remove(&0);
        direct_counts
            .entry(name.clone())
            .and_modify(|n: &mut usize| *n = (*n).max(direct.len()))
            .or_insert(direct.len());
        for id in direct {
            if !roots.contains(&id) {
                reached.entry(id).or_insert((1, name.clone()));
            }
        }
    }

    let mut frontier: Vec<i64> = reached.keys().copied().collect();
    let mut refs = conn.prepare("SELECT DISTINCT src_symbol FROM edges WHERE dst_symbol = ?1")?;
    for depth in 2..=max_depth {
        let mut next = Vec::new();
        for id in &frontier {
            let via = reached.get(id).map(|(_, v)| v.clone()).unwrap_or_default();
            let callers = refs.query_map([id], |r| r.get::<_, i64>(0))?;
            for caller in callers.filter_map(|r| r.ok()) {
                if !roots.contains(&caller) && !reached.contains_key(&caller) {
                    reached.insert(caller, (depth, via.clone()));
                    next.push(caller);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    // Changed test definitions are test-plan roots even when nothing calls
    // them; include them at depth zero.
    for c in changed.iter().filter(|c| c.current && c.is_test) {
        if let (Some(name), Some(kind)) = (&c.name, &c.kind) {
            let mut stmt =
                conn.prepare("SELECT id FROM symbols WHERE file = ?1 AND name = ?2 AND kind = ?3")?;
            for id in stmt
                .query_map(rusqlite::params![c.path, name, kind], |r| {
                    r.get::<_, i64>(0)
                })?
                .filter_map(|r| r.ok())
            {
                reached.entry(id).or_insert((0, name.clone()));
            }
        }
    }

    let mut out = Vec::new();
    let mut detail = conn.prepare(
        "SELECT file, start_line, COALESCE(signature, ''), service, is_test, rank
         FROM symbols WHERE id = ?1",
    )?;
    for (id, (depth, via)) in reached {
        let a = detail.query_row([id], |r| {
            Ok(Affected {
                file: r.get(0)?,
                line: r.get(1)?,
                signature: r.get(2)?,
                service: r.get(3)?,
                is_test: r.get::<_, i64>(4)? != 0,
                depth,
                via,
                rank: r.get(5)?,
            })
        })?;
        out.push(a);
    }
    Ok((out, direct_counts))
}

fn service_for_path(conn: &Connection, path: &str) -> Result<String> {
    if let Ok(service) = conn.query_row("SELECT service FROM files WHERE path = ?1", [path], |r| {
        r.get(0)
    }) {
        return Ok(service);
    }
    let mut stmt = conn.prepare("SELECT name, path FROM services ORDER BY length(path) DESC")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (name, prefix) = row?;
        if prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/")) {
            return Ok(name);
        }
    }
    Ok("?".into())
}

fn changed_files(root: &Path, base: &str) -> Result<Vec<ChangedFile>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "--relative",
            base,
            "--",
            ".",
        ])
        .output()
        .context("run git diff")?;
    if !out.status.success() {
        bail!(
            "git diff against '{base}' failed under '{}': {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut files = parse_name_status(&out.stdout)?;

    let untracked = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ])
        .output()
        .context("run git ls-files")?;
    if untracked.status.success() {
        let existing: HashSet<&str> = files.iter().filter_map(|f| f.new_path.as_deref()).collect();
        let additions: Vec<String> = untracked
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .filter(|p| !existing.contains(p.as_str()))
            .collect();
        files.extend(additions.into_iter().map(|path| ChangedFile {
            status: FileStatus::Added,
            old_path: None,
            new_path: Some(path),
        }));
    }
    files.sort_by(|a, b| {
        a.new_path
            .as_ref()
            .or(a.old_path.as_ref())
            .cmp(&b.new_path.as_ref().or(b.old_path.as_ref()))
    });
    // The default index is a derived working-tree artifact and is commonly
    // untracked in freshly initialized repositories. It must never appear as
    // an application change merely because a user has not added the
    // recommended ignore rule yet.
    files.retain(|f| {
        f.new_path
            .as_deref()
            .or(f.old_path.as_deref())
            .and_then(|p| p.rsplit('/').next())
            .is_none_or(|name| !name.starts_with(".repomap.db"))
    });
    Ok(files)
}

fn parse_name_status(bytes: &[u8]) -> Result<Vec<ChangedFile>> {
    let parts: Vec<String> = bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < parts.len() {
        let status = &parts[i];
        i += 1;
        let code = status.chars().next().unwrap_or('?');
        let path = parts
            .get(i)
            .cloned()
            .context("malformed git --name-status output")?;
        i += 1;
        let file = match code {
            'A' => ChangedFile {
                status: FileStatus::Added,
                old_path: None,
                new_path: Some(path),
            },
            'D' => ChangedFile {
                status: FileStatus::Deleted,
                old_path: Some(path),
                new_path: None,
            },
            'M' | 'T' => ChangedFile {
                status: FileStatus::Modified,
                old_path: Some(path.clone()),
                new_path: Some(path),
            },
            'R' | 'C' => {
                let new_path = parts
                    .get(i)
                    .cloned()
                    .context("rename missing destination path")?;
                i += 1;
                ChangedFile {
                    status: FileStatus::Renamed,
                    old_path: Some(path),
                    new_path: Some(new_path),
                }
            }
            other => bail!("unsupported git change status '{other}'"),
        };
        out.push(file);
    }
    Ok(out)
}

fn git_prefix(root: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-prefix"])
        .output()
        .context("run git rev-parse")?;
    if !out.status.success() {
        bail!("'{}' is not inside a Git repository", root.display());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_show(root: &Path, base: &str, path: &str) -> Result<String> {
    let spec = format!("{base}:{path}");
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", &spec])
        .output()
        .with_context(|| format!("read '{path}' at {base}"))?;
    if !out.status.success() {
        bail!(
            "git show '{spec}' failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modified_added_deleted_and_renamed_paths() {
        let bytes = b"M\0a.rs\0A\0new.py\0D\0old.rb\0R100\0from.ts\0to.ts\0";
        assert_eq!(
            parse_name_status(bytes).unwrap(),
            vec![
                ChangedFile {
                    status: FileStatus::Modified,
                    old_path: Some("a.rs".into()),
                    new_path: Some("a.rs".into())
                },
                ChangedFile {
                    status: FileStatus::Added,
                    old_path: None,
                    new_path: Some("new.py".into())
                },
                ChangedFile {
                    status: FileStatus::Deleted,
                    old_path: Some("old.rb".into()),
                    new_path: None
                },
                ChangedFile {
                    status: FileStatus::Renamed,
                    old_path: Some("from.ts".into()),
                    new_path: Some("to.ts".into())
                },
            ]
        );
    }

    #[test]
    fn nested_symbol_changes_do_not_change_the_container_fingerprint() {
        let lang = Language::Rust;
        let before = "impl Widget {\n  fn inner() { one(); }\n}\n";
        let after = "impl Widget {\n  fn inner() { two(); }\n}\n";
        let a = snapshots(lang, "a.rs", before).unwrap();
        let b = snapshots(lang, "a.rs", after).unwrap();
        let inner_a = a.iter().find(|s| s.name == "inner").unwrap();
        let inner_b = b.iter().find(|s| s.name == "inner").unwrap();
        assert_ne!(inner_a.body_hash, inner_b.body_hash);
        // Some grammars don't expose an impl as a named symbol; when they do,
        // its own-body fingerprint must remain stable.
        for outer_a in a
            .iter()
            .filter(|s| s.start_line < inner_a.start_line && s.end_line >= inner_a.end_line)
        {
            let outer_b = b
                .iter()
                .find(|s| s.name == outer_a.name && s.kind == outer_a.kind)
                .unwrap();
            assert_eq!(outer_a.body_hash, outer_b.body_hash);
        }
    }

    fn git(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn deleted_symbol_keeps_its_callers_and_selects_a_transitive_test() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("lib.rs"), "pub fn target() {}\n").unwrap();
        std::fs::write(
            root.join("consumer.rs"),
            "fn bridge() { target(); }\n#[test]\nfn verifies_behavior() { bridge(); }\n",
        )
        .unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "base"]);

        std::fs::write(root.join("lib.rs"), "// target deliberately removed\n").unwrap();
        let db_path = root.join(".repomap.db");
        let mut conn = crate::db::open(db_path.to_str().unwrap()).unwrap();
        crate::index::run(&mut conn, root, false, &db_path).unwrap();
        let report = analyze(&conn, root, "HEAD", 3).unwrap();

        assert!(report
            .changed
            .iter()
            .any(|c| c.nature == Nature::Deleted && c.name.as_deref() == Some("target")));
        assert!(report
            .affected
            .iter()
            .any(|a| a.signature.contains("bridge") && a.depth == 1));
        assert!(report
            .affected
            .iter()
            .any(|a| a.signature.contains("verifies_behavior") && a.is_test && a.depth == 2));
    }

    #[test]
    fn deleted_method_recovery_respects_container_and_receiver() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        let before = "object InvoiceService {\n\
                        val store = scala.collection.mutable.Map.empty[String, String]\n\
                        def get(id: String): Option[String] = None\n\
                        def total(id: String): Option[String] = get(id)\n\
                        def unrelated(id: String): Option[String] = store.get(id)\n\
                      }\n";
        std::fs::write(root.join("Invoice.scala"), before).unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "base"]);

        let after = "object InvoiceService {\n\
                       val store = scala.collection.mutable.Map.empty[String, String]\n\
                       def total(id: String): Option[String] = get(id)\n\
                       def unrelated(id: String): Option[String] = store.get(id)\n\
                     }\n";
        std::fs::write(root.join("Invoice.scala"), after).unwrap();
        let db_path = root.join(".repomap.db");
        let mut conn = crate::db::open(db_path.to_str().unwrap()).unwrap();
        crate::index::run(&mut conn, root, false, &db_path).unwrap();
        let report = analyze(&conn, root, "HEAD", 1).unwrap();

        assert!(report.changed.iter().any(|c| {
            c.nature == Nature::Deleted
                && c.name.as_deref() == Some("get")
                && c.container.as_deref() == Some("InvoiceService")
        }));
        assert!(report
            .affected
            .iter()
            .any(|a| a.signature.contains("def total")));
        assert!(!report
            .affected
            .iter()
            .any(|a| a.signature.contains("def unrelated")));
    }

    #[test]
    fn classifies_signature_changes_and_untracked_symbols() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("lib.rs"), "pub fn api(x: i32) -> i32 { x }\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "base"]);
        std::fs::write(root.join("lib.rs"), "pub fn api(x: i64) -> i64 { x }\n").unwrap();
        std::fs::write(root.join("extra.rs"), "pub fn extra() {}\n").unwrap();
        std::fs::write(root.join("asset.bin"), [0xff, 0x00, 0xfe]).unwrap();

        let db_path = root.join(".repomap.db");
        let mut conn = crate::db::open(db_path.to_str().unwrap()).unwrap();
        crate::index::run(&mut conn, root, false, &db_path).unwrap();
        let report = analyze(&conn, root, "HEAD", 1).unwrap();
        assert_eq!(report.files, 3);
        assert!(report
            .changed
            .iter()
            .any(|c| c.nature == Nature::Signature && c.name.as_deref() == Some("api")));
        assert!(report
            .changed
            .iter()
            .any(|c| c.nature == Nature::Added && c.name.as_deref() == Some("extra")));
        assert!(report
            .changed
            .iter()
            .any(|c| c.path == "asset.bin" && c.nature == Nature::File));
    }
}
