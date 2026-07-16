//! `context`: a one-shot, token-budgeted orientation pack for a task.
//!
//! Instead of an agent running find → def → callers → callees and stitching
//! the answers together, one command seeds symbols from full-text search,
//! pulls each seed's most important graph neighbors, and packs the result to
//! fit a declared token budget. Output stays pointers-only, so the whole pack
//! is safe to drop into a model's context verbatim.

use anyhow::{bail, Result};
use rusqlite::Connection;
use serde_json::json;

/// How many seed symbols to consider before budget packing.
const MAX_SEEDS: usize = 8;
/// Graph neighbors shown per direction per seed (most important first).
const MAX_NEIGHBORS: usize = 3;
/// Every seed block cheaper than this always fits; tokens ≈ chars / 4, the
/// usual code-ish approximation — a budget, not an exact meter.
const CHARS_PER_TOKEN: usize = 4;

struct Seed {
    id: i64,
    line: String,
    service: String,
}

pub fn context(
    conn: &Connection,
    query: &str,
    budget: usize,
    include_tests: bool,
) -> Result<usize> {
    let Some((output, shown)) = build_context(conn, query, budget, include_tests)? else {
        crate::output::no_match(format!("no matches for '{query}'"));
        return Ok(0);
    };
    crate::output::emit(
        "context",
        json!({
            "query": query,
            "budget_tokens": budget,
            "seeds_shown": shown,
            "content": output,
        }),
        &output,
    );
    Ok(shown)
}

fn build_context(
    conn: &Connection,
    query: &str,
    budget: usize,
    include_tests: bool,
) -> Result<Option<(String, usize)>> {
    let seeds = seeds(conn, query, include_tests)?;
    if seeds.is_empty() {
        return Ok(None);
    }

    // Blocks arrive in relevance order. Render each candidate prefix in full
    // so services omitted by the budget do not consume space and the footer
    // itself is included in the estimate.
    let seed_blocks: Vec<String> = seeds
        .iter()
        .map(|s| seed_block(conn, s, include_tests))
        .collect::<Result<_>>()?;

    let (mut output, minimum) = render_pack(conn, query, &seeds, &seed_blocks, 1, budget)?;
    if minimum > budget {
        bail!("token budget {budget} is too small for one context result (minimum ~{minimum})");
    }

    let mut shown = 1;
    for candidate in 2..=seed_blocks.len() {
        let (next, used) = render_pack(conn, query, &seeds, &seed_blocks, candidate, budget)?;
        if used > budget {
            break;
        }
        output = next;
        shown = candidate;
    }
    Ok(Some((output, shown)))
}

fn render_pack(
    conn: &Connection,
    query: &str,
    seeds: &[Seed],
    seed_blocks: &[String],
    shown: usize,
    budget: usize,
) -> Result<(String, usize)> {
    let services = services_block(conn, &seeds[..shown])?;
    let body = format!(
        "# context: {query}\n{services}\n## symbols\n{}",
        seed_blocks[..shown].join("\n")
    );
    let suffix = if shown < seed_blocks.len() {
        " — raise --budget for more"
    } else {
        ""
    };
    let mut used = est_tokens(&body);
    let mut output = String::new();
    // The footer contains the estimate itself. A few fixed-point iterations
    // account for changes in its digit count.
    for _ in 0..4 {
        output = format!(
            "{body}\n[~{used} tokens / budget {budget}; {shown}/{} seeds{suffix}]",
            seed_blocks.len()
        );
        let next = est_tokens(&output);
        if next == used {
            break;
        }
        used = next;
    }
    let final_used = est_tokens(&output);
    Ok((output, final_used))
}

/// Top FTS matches for the query, ordered by text relevance then importance —
/// the same ordering `find` uses, so the pack starts where `find` would. An
/// orientation pack is about production code, so test symbols are excluded
/// unless explicitly requested.
fn seeds(conn: &Connection, query: &str, include_tests: bool) -> Result<Vec<Seed>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.file, s.start_line, s.signature, s.name, s.service
         FROM symbols_fts f
         JOIN symbols s ON s.id = f.rowid
         WHERE symbols_fts MATCH ?1 AND (s.is_test = 0 OR ?3)
         ORDER BY bm25(symbols_fts), s.rank DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![
            crate::query::fts_query_any(query),
            MAX_SEEDS as i64,
            include_tests
        ],
        |r| {
            let file: String = r.get(1)?;
            let line: i64 = r.get(2)?;
            let sig: Option<String> = r.get(3)?;
            let name: String = r.get(4)?;
            Ok(Seed {
                id: r.get(0)?,
                line: format!("{file}:L{line}  {}", sig.unwrap_or(name)),
                service: r.get(5)?,
            })
        },
    )?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// `## services` section: the services the seeds live in, with stack and size.
fn services_block(conn: &Connection, seeds: &[Seed]) -> Result<String> {
    let mut names: Vec<&str> = seeds.iter().map(|s| s.service.as_str()).collect();
    names.sort();
    names.dedup();

    let mut out = String::from("## services");
    let mut stmt = conn.prepare(
        "SELECT sv.stack, (SELECT count(*) FROM files f WHERE f.service = sv.name)
         FROM services sv WHERE sv.name = ?1",
    )?;
    for name in names {
        let (stack, nfiles): (Option<String>, i64) = stmt
            .query_row([name], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap_or((None, 0));
        out.push_str(&format!(
            "\n{name}  ({})  {nfiles} files",
            stack.unwrap_or_else(|| "?".into())
        ));
    }
    Ok(out)
}

/// One seed's block: its pointer line plus its most important callers (`<-`)
/// and callees (`->`), one line each. Test neighbors are excluded with the
/// same default as seeds so they can't crowd out production callers.
fn seed_block(conn: &Connection, seed: &Seed, include_tests: bool) -> Result<String> {
    let mut out = seed.line.clone();
    for (arrow, sql) in [
        (
            "<-",
            "SELECT n.file, n.start_line, n.name, e.kind FROM edges e
             JOIN symbols n ON n.id = e.src_symbol
             WHERE e.dst_symbol = ?1 AND (n.is_test = 0 OR ?3)
             GROUP BY n.id ORDER BY n.rank DESC LIMIT ?2",
        ),
        (
            "->",
            "SELECT n.file, n.start_line, n.name, e.kind FROM edges e
             JOIN symbols n ON n.id = e.dst_symbol
             WHERE e.src_symbol = ?1 AND (n.is_test = 0 OR ?3)
             GROUP BY n.id ORDER BY n.rank DESC LIMIT ?2",
        ),
    ] {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(
            rusqlite::params![seed.id, MAX_NEIGHBORS as i64, include_tests],
            |r| {
                let file: String = r.get(0)?;
                let line: i64 = r.get(1)?;
                let name: String = r.get(2)?;
                let kind: String = r.get(3)?;
                Ok(format!("  {arrow} {file}:L{line}  {name}  ({kind})"))
            },
        )?;
        for line in rows.filter_map(|r| r.ok()) {
            out.push('\n');
            out.push_str(&line);
        }
    }
    Ok(out)
}

/// tokens ≈ ceil(chars / 4), plus one per line for the newline-ish overhead.
fn est_tokens(s: &str) -> usize {
    s.len().div_ceil(CHARS_PER_TOKEN) + s.lines().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed_symbol() -> Connection {
        let conn = crate::db::open(":memory:").unwrap();
        conn.execute(
            "INSERT INTO services(name, path, stack) VALUES ('app', '.', 'rust')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files(path, service, language, loc, git_hash, indexed_at)
             VALUES ('src/lib.rs', 'app', 'rust', 10, 'h', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols(name, kind, file, start_line, end_line, signature,
                                 service, language)
             VALUES ('target', 'fn', 'src/lib.rs', 1, 2, 'fn target()', 'app', 'rust')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn token_estimate_scales_with_length_and_lines() {
        assert_eq!(est_tokens(""), 0);
        let one = est_tokens("abcdefgh"); // 8 chars, 1 line -> 3
        assert_eq!(one, 3);
        assert!(est_tokens("abcdefgh\nabcdefgh") > one);
    }

    #[test]
    fn context_never_exceeds_the_declared_budget() {
        let conn = indexed_symbol();
        let minimum = (0..500)
            .find(|budget| build_context(&conn, "target", *budget, false).is_ok())
            .expect("a reasonable budget must fit one result");
        assert!(minimum > 0);
        assert!((0..minimum).all(|budget| build_context(&conn, "target", budget, false).is_err()));

        let (output, shown) = build_context(&conn, "target", minimum, false)
            .unwrap()
            .unwrap();
        assert_eq!(shown, 1);
        assert!(est_tokens(&output) <= minimum);
    }

    #[test]
    fn context_excludes_test_symbols_unless_asked() {
        let conn = indexed_symbol();
        conn.execute(
            "INSERT INTO symbols(name, kind, file, start_line, end_line, signature,
                                 service, language, is_test)
             VALUES ('target_test_helper', 'fn', 'src/lib.rs', 5, 6,
                     'fn target_test_helper()', 'app', 'rust', 1)",
            [],
        )
        .unwrap();

        let (output, _) = build_context(&conn, "target", 2000, false)
            .unwrap()
            .unwrap();
        assert!(
            !output.contains("target_test_helper"),
            "test symbols must not seed a default context pack:\n{output}"
        );
        let (output, _) = build_context(&conn, "target", 2000, true).unwrap().unwrap();
        assert!(
            output.contains("target_test_helper"),
            "--include-tests must bring test symbols back:\n{output}"
        );
    }
}
