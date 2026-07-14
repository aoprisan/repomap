//! Graph analytics over the resolved symbol graph: PageRank importance
//! (computed at index time, stored on `symbols.rank`) and transitive
//! blast-radius traversal for `impact`.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

const DAMPING: f64 = 0.85;
const MAX_ITERS: usize = 50;
const CONVERGENCE: f64 = 1e-9;

/// PageRank over a directed graph of `n` nodes given `(src, dst)` edges.
/// Importance flows src → dst, so a node referenced by important nodes ends
/// up important — the graph-aware upgrade of "count the callers". Rank mass
/// from dangling nodes (no outgoing edges) is redistributed uniformly, and
/// parallel edges count once each (a caller referencing you twice vouches
/// twice). Returns ranks summing to ~1.0; empty input yields an empty vec.
pub fn pagerank(n: usize, edges: &[(usize, usize)]) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let mut out_degree = vec![0usize; n];
    for &(src, _) in edges {
        out_degree[src] += 1;
    }
    let uniform = 1.0 / n as f64;
    let mut rank = vec![uniform; n];
    let mut next = vec![0.0f64; n];

    for _ in 0..MAX_ITERS {
        // Base share: teleportation + the rank of dangling nodes, spread evenly.
        let dangling: f64 = (0..n)
            .filter(|&i| out_degree[i] == 0)
            .map(|i| rank[i])
            .sum();
        let base = (1.0 - DAMPING) * uniform + DAMPING * dangling * uniform;
        next.iter_mut().for_each(|r| *r = base);
        for &(src, dst) in edges {
            next[dst] += DAMPING * rank[src] / out_degree[src] as f64;
        }
        let delta: f64 = rank.iter().zip(&next).map(|(a, b)| (a - b).abs()).sum();
        std::mem::swap(&mut rank, &mut next);
        if delta < CONVERGENCE {
            break;
        }
    }
    rank
}

/// Compute PageRank over the `edges` table and persist it into
/// `symbols.rank`. Called whenever an index run changes the symbol graph.
pub fn compute_ranks(conn: &Connection) -> Result<()> {
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM symbols ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    if ids.is_empty() {
        return Ok(());
    }
    let index_of: HashMap<i64, usize> = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();

    let edges: Vec<(usize, usize)> = {
        let mut stmt = conn.prepare("SELECT src_symbol, dst_symbol FROM edges")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        rows.filter_map(|r| r.ok())
            .filter_map(|(s, d)| Some((*index_of.get(&s)?, *index_of.get(&d)?)))
            .collect()
    };

    let ranks = pagerank(ids.len(), &edges);
    let mut stmt = conn.prepare("UPDATE symbols SET rank = ?1 WHERE id = ?2")?;
    for (i, id) in ids.iter().enumerate() {
        stmt.execute(rusqlite::params![ranks[i], id])?;
    }
    Ok(())
}

/// One symbol reached by the impact traversal, at its BFS distance from the
/// changed symbol (depth 1 = direct callers, 2 = callers of callers, …).
pub struct Reached {
    pub id: i64,
    pub depth: usize,
}

/// Transitive blast radius: BFS over *reverse* edges from every symbol named
/// `symbol`, up to `max_depth` hops. Answers "if I change this, what could
/// break?" — each frontier is the callers of the previous one. Roots
/// themselves are not reported; a symbol is reported once, at its shortest
/// distance.
#[cfg(test)]
pub fn impact(conn: &Connection, symbol: &str, max_depth: usize) -> Result<Vec<Reached>> {
    let roots: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM symbols WHERE name = ?1")?;
        let rows = stmt.query_map([symbol], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    impact_from_roots(conn, &roots, max_depth)
}

pub fn impact_from_roots(
    conn: &Connection,
    roots: &[i64],
    max_depth: usize,
) -> Result<Vec<Reached>> {
    let mut seen: HashSet<i64> = roots.iter().copied().collect();
    let mut frontier = roots.to_vec();
    let mut out = Vec::new();

    let mut stmt = conn.prepare("SELECT DISTINCT src_symbol FROM edges WHERE dst_symbol = ?1")?;
    for depth in 1..=max_depth {
        let mut next = Vec::new();
        for id in &frontier {
            let callers = stmt.query_map([id], |r| r.get::<_, i64>(0))?;
            for caller in callers.filter_map(|r| r.ok()) {
                if seen.insert(caller) {
                    out.push(Reached { id: caller, depth });
                    next.push(caller);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagerank_of_empty_graph_is_empty() {
        assert!(pagerank(0, &[]).is_empty());
    }

    #[test]
    fn pagerank_sums_to_one_and_rewards_being_referenced() {
        // 0 -> 2, 1 -> 2: node 2 is the popular one.
        let r = pagerank(3, &[(0, 2), (1, 2)]);
        let total: f64 = r.iter().sum();
        assert!(
            (total - 1.0).abs() < 1e-6,
            "ranks must sum to 1, got {total}"
        );
        assert!(
            r[2] > r[0] && r[2] > r[1],
            "referenced node must outrank referencers: {r:?}"
        );
    }

    #[test]
    fn pagerank_flows_transitively() {
        // A chain 0 -> 1 -> 2 plus 3 -> 1: node 2's only referencer is the
        // important node 1, so 2 must outrank the leaf nodes 0 and 3 even
        // though it has fewer direct referencers than 1 — the property raw
        // in-degree cannot express.
        let r = pagerank(4, &[(0, 1), (1, 2), (3, 1)]);
        assert!(
            r[1] > r[0] && r[1] > r[3],
            "referenced node outranks leaves: {r:?}"
        );
        assert!(
            r[2] > r[0] && r[2] > r[3],
            "importance flows through the chain: {r:?}"
        );
    }

    #[test]
    fn pagerank_isolated_nodes_share_rank_equally() {
        let r = pagerank(2, &[]);
        assert!((r[0] - r[1]).abs() < 1e-9);
        assert!((r[0] + r[1] - 1.0).abs() < 1e-6);
    }

    /// Index a tiny repo where `hub` is called from two places and `leaf`
    /// from none; the persisted ranks and the impact BFS must reflect that.
    fn indexed_fixture() -> (rusqlite::Connection, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("svc")).unwrap();
        std::fs::write(
            dir.path().join("svc/a.rs"),
            "pub fn hub() {}\npub fn leaf() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("svc/b.rs"),
            "pub fn caller1() { hub(); }\npub fn caller2() { hub(); }\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("svc/c.rs"), "pub fn top() { caller1(); }\n").unwrap();
        let db_path = dir.path().join(".repomap.db");
        let mut conn = crate::db::open(db_path.to_str().unwrap()).unwrap();
        crate::index::run(&mut conn, dir.path(), false, &db_path).unwrap();
        (conn, dir)
    }

    fn rank_of(conn: &rusqlite::Connection, name: &str) -> f64 {
        conn.query_row("SELECT rank FROM symbols WHERE name = ?1", [name], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn index_run_persists_ranks_that_reward_referenced_symbols() {
        let (conn, _dir) = indexed_fixture();
        assert!(
            rank_of(&conn, "hub") > rank_of(&conn, "leaf"),
            "a called symbol must outrank an uncalled one"
        );
    }

    #[test]
    fn impact_walks_callers_transitively_and_reports_shortest_depth() {
        let (conn, _dir) = indexed_fixture();
        let name_of = |id: i64| -> String {
            conn.query_row("SELECT name FROM symbols WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        let reached = impact(&conn, "hub", 3).unwrap();
        let mut by_name: Vec<(String, usize)> =
            reached.iter().map(|r| (name_of(r.id), r.depth)).collect();
        by_name.sort();
        assert_eq!(
            by_name,
            vec![
                ("caller1".to_string(), 1),
                ("caller2".to_string(), 1),
                ("top".to_string(), 2),
            ]
        );

        // Depth 1 stops before the transitive hop.
        let shallow = impact(&conn, "hub", 1).unwrap();
        assert!(shallow.iter().all(|r| r.depth == 1));
        assert_eq!(shallow.len(), 2);

        // Unknown symbol: empty, not an error.
        assert!(impact(&conn, "nope", 3).unwrap().is_empty());
    }
}
