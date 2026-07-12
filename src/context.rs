//! `context`: a one-shot, token-budgeted orientation pack for a task.
//!
//! Instead of an agent running find → def → callers → callees and stitching
//! the answers together, one command seeds symbols from full-text search,
//! pulls each seed's most important graph neighbors, and packs the result to
//! fit a declared token budget. Output stays pointers-only, so the whole pack
//! is safe to drop into a model's context verbatim.

use anyhow::Result;
use rusqlite::Connection;

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

pub fn context(conn: &Connection, query: &str, budget: usize) -> Result<()> {
    let seeds = seeds(conn, query)?;
    if seeds.is_empty() {
        eprintln!("no matches for '{query}'");
        return Ok(());
    }

    // Assemble blocks first, then pack: header + services always ship, seed
    // blocks are added greedily (they arrive relevance-ordered) until the
    // budget runs out — but at least one seed always ships, otherwise the
    // pack answers nothing.
    let header = format!("# context: {query}");
    let services_block = services_block(conn, &seeds)?;
    let seed_blocks: Vec<String> = seeds
        .iter()
        .map(|s| seed_block(conn, s))
        .collect::<Result<_>>()?;

    let mut used = est_tokens(&header) + est_tokens(&services_block) + est_tokens("## symbols");
    let mut shown = 0usize;
    for block in &seed_blocks {
        let cost = est_tokens(block);
        if shown > 0 && used + cost > budget {
            break;
        }
        used += cost;
        shown += 1;
    }

    println!("{header}");
    println!("{services_block}");
    println!("## symbols");
    for block in seed_blocks.iter().take(shown) {
        println!("{block}");
    }
    println!(
        "[~{used} tokens / budget {budget}; {shown}/{} seeds{}]",
        seed_blocks.len(),
        if shown < seed_blocks.len() { " — raise --budget for more" } else { "" }
    );
    Ok(())
}

/// Top FTS matches for the query, ordered by text relevance then importance —
/// the same ordering `find` uses, so the pack starts where `find` would.
fn seeds(conn: &Connection, query: &str) -> Result<Vec<Seed>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.file, s.start_line, s.signature, s.name, s.service
         FROM symbols_fts f
         JOIN symbols s ON s.id = f.rowid
         WHERE symbols_fts MATCH ?1
         ORDER BY bm25(symbols_fts), s.rank DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![crate::query::fts_query_any(query), MAX_SEEDS as i64],
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
/// and callees (`->`), one line each.
fn seed_block(conn: &Connection, seed: &Seed) -> Result<String> {
    let mut out = seed.line.clone();
    for (arrow, sql) in [
        (
            "<-",
            "SELECT n.file, n.start_line, n.name, e.kind FROM edges e
             JOIN symbols n ON n.id = e.src_symbol
             WHERE e.dst_symbol = ?1
             GROUP BY n.id ORDER BY n.rank DESC LIMIT ?2",
        ),
        (
            "->",
            "SELECT n.file, n.start_line, n.name, e.kind FROM edges e
             JOIN symbols n ON n.id = e.dst_symbol
             WHERE e.src_symbol = ?1
             GROUP BY n.id ORDER BY n.rank DESC LIMIT ?2",
        ),
    ] {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params![seed.id, MAX_NEIGHBORS as i64], |r| {
            let file: String = r.get(0)?;
            let line: i64 = r.get(1)?;
            let name: String = r.get(2)?;
            let kind: String = r.get(3)?;
            Ok(format!("  {arrow} {file}:L{line}  {name}  ({kind})"))
        })?;
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

    #[test]
    fn token_estimate_scales_with_length_and_lines() {
        assert_eq!(est_tokens(""), 0);
        let one = est_tokens("abcdefgh"); // 8 chars, 1 line -> 3
        assert_eq!(one, 3);
        assert!(est_tokens("abcdefgh\nabcdefgh") > one);
    }
}
