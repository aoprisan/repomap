//! Lifetime usage stats, persisted in the index's `usage` table: how much the
//! tool was used and a rough estimate of the tokens it saved. Every query
//! command records one event; `repomap usage` reports the running totals.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::json;

use crate::index::epoch_secs;

/// Rough tokens per line of source — used only to turn an avoided file read
/// into a tokens-saved figure. Deliberately conservative.
const TOKENS_PER_LINE: f64 = 8.0;

/// Approximate token cost of the one compact pointer line we return instead.
const POINTER_TOKENS: f64 = 30.0;

/// Add one command invocation to the running totals. Upserts so the first use
/// of a command seeds its row and later uses accumulate onto it.
pub fn record(conn: &Connection, command: &str, results: usize, tokens_saved: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO usage(command, runs, results, tokens_saved, last_used)
         VALUES (?1, 1, ?2, ?3, ?4)
         ON CONFLICT(command) DO UPDATE SET
           runs         = runs + 1,
           results      = results + ?2,
           tokens_saved = tokens_saved + ?3,
           last_used    = ?4",
        rusqlite::params![command, results as i64, tokens_saved, epoch_secs()],
    )?;
    Ok(())
}

/// Estimate the tokens a query saved: each returned pointer stands in for the
/// agent opening ~one average-sized file to locate that symbol. Grounded in the
/// index's own average file length, so it scales with the repo. Returns 0 when
/// nothing is indexed yet (no basis to estimate from).
pub fn estimate_tokens_saved(conn: &Connection, results: usize) -> i64 {
    if results == 0 {
        return 0;
    }
    let per_result = (avg_file_tokens(conn) - POINTER_TOKENS).max(0.0);
    (per_result * results as f64).round() as i64
}

/// Mean file length (in estimated tokens) across the indexed files, 0 if empty.
fn avg_file_tokens(conn: &Connection) -> f64 {
    let (total_loc, n): (i64, i64) = conn
        .query_row(
            "SELECT COALESCE(sum(loc), 0), count(*) FROM files",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    if n == 0 {
        0.0
    } else {
        (total_loc as f64 / n as f64) * TOKENS_PER_LINE
    }
}

/// Print the per-command totals plus a grand total. The estimate is labelled as
/// such — it is a heuristic, not a measurement.
pub fn report(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT command, runs, results, tokens_saved, last_used
         FROM usage ORDER BY runs DESC, command",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;

    if !crate::output::is_jsonl() {
        println!(
            "{:<10} {:>7} {:>8} {:>15}  last_used",
            "command", "runs", "results", "~tokens_saved"
        );
    }
    let (mut total_runs, mut total_results, mut total_saved) = (0i64, 0i64, 0i64);
    let mut any = false;
    for row in rows {
        let (cmd, runs, results, saved, last) = row?;
        total_runs += runs;
        total_results += results;
        total_saved += saved;
        any = true;
        crate::output::emit(
            "usage",
            json!({
                "query_command": cmd,
                "runs": runs,
                "results": results,
                "estimated_tokens_saved": saved,
                "last_used_epoch": last,
            }),
            format!(
                "{:<10} {:>7} {:>8} {:>15}  {}",
                cmd, runs, results, saved, last
            ),
        );
    }
    if !any {
        crate::output::emit(
            "usage_summary",
            json!({"runs": 0, "results": 0, "estimated_tokens_saved": 0}),
            "(no usage recorded yet — run a `find`/`def`/`callers`/`map`)",
        );
        return Ok(());
    }
    crate::output::emit(
        "usage_summary",
        json!({
            "runs": total_runs,
            "results": total_results,
            "estimated_tokens_saved": total_saved,
        }),
        format!(
            "{:<10} {:>7} {:>8} {:>15}",
            "total", total_runs, total_results, total_saved
        ),
    );
    if !crate::output::is_jsonl() {
        println!(
            "\n~tokens_saved is a rough estimate: each result pointer stands in for\nopening ~one average indexed file the agent didn't have to read.\nlast_used is epoch seconds."
        );
    }
    Ok(())
}

/// Clear all usage history.
pub fn reset(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM usage", [])?;
    crate::output::emit(
        "usage_reset",
        json!({"cleared": true}),
        "usage stats cleared",
    );
    Ok(())
}

/// Compact one-line summary for `--show-db`: total runs and tokens saved.
pub fn summary_line(conn: &Connection) -> Option<String> {
    let (runs, saved): (i64, i64) = conn
        .query_row(
            "SELECT COALESCE(sum(runs), 0), COALESCE(sum(tokens_saved), 0) FROM usage",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    if runs == 0 {
        None
    } else {
        Some(format!("{runs} runs, ~{saved} tokens saved"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn mem() -> Connection {
        db::open(":memory:").unwrap()
    }

    #[test]
    fn record_accumulates_per_command() {
        let conn = mem();
        record(&conn, "find", 3, 100).unwrap();
        record(&conn, "find", 2, 50).unwrap();
        record(&conn, "def", 1, 10).unwrap();

        let (runs, results, saved): (i64, i64, i64) = conn
            .query_row(
                "SELECT runs, results, tokens_saved FROM usage WHERE command = 'find'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((runs, results, saved), (2, 5, 150));

        let total: i64 = conn
            .query_row("SELECT sum(runs) FROM usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 3);
    }

    #[test]
    fn estimate_scales_with_results_and_is_zero_when_empty() {
        let conn = mem();
        // No files indexed: nothing to base an estimate on.
        assert_eq!(estimate_tokens_saved(&conn, 5), 0);
        assert_eq!(estimate_tokens_saved(&conn, 0), 0);

        // Seed a couple of files so avg_file_tokens has a basis.
        conn.execute(
            "INSERT INTO files(path, service, language, loc, git_hash, indexed_at)
             VALUES ('a', 's', 'rust', 100, 'h1', 0), ('b', 's', 'rust', 100, 'h2', 0)",
            [],
        )
        .unwrap();
        // avg loc 100 -> 800 tok/file, minus 30 pointer = 770 per result.
        assert_eq!(estimate_tokens_saved(&conn, 1), 770);
        assert_eq!(estimate_tokens_saved(&conn, 3), 2310);
        assert_eq!(estimate_tokens_saved(&conn, 0), 0);
    }

    #[test]
    fn reset_clears_and_summary_reflects_state() {
        let conn = mem();
        assert!(summary_line(&conn).is_none());
        record(&conn, "find", 2, 40).unwrap();
        assert!(summary_line(&conn).unwrap().contains("1 runs"));
        reset(&conn).unwrap();
        assert!(summary_line(&conn).is_none());
        // report() on an empty table must not error.
        report(&conn).unwrap();
    }
}
