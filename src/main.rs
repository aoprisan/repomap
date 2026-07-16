mod changes;
mod cli;
mod context;
mod db;
mod git;
mod graph;
mod history;
mod index;
mod install;
mod lang;
mod output;
mod query;
mod services;
mod skill;
mod usage;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Cmd, SymbolSelectorArgs};

fn selector(args: SymbolSelectorArgs) -> query::SymbolSelector {
    query::SymbolSelector {
        name: args.symbol,
        service: args.service,
        file: args.file,
        kind: args.kind,
    }
}

fn record_query(conn: &rusqlite::Connection, command: &str, results: usize) -> Result<()> {
    let saved = usage::estimate_tokens_saved(conn, results);
    usage::record(conn, command, results, saved)
}

fn command_name(args: &Cli) -> &'static str {
    if args.install {
        return "install";
    }
    if args.install_skill.is_some() {
        return "install-skill";
    }
    if args.show_db {
        return "show-db";
    }
    if args.clear_db {
        return "clear-db";
    }
    match args.cmd.as_ref() {
        Some(Cmd::Index { .. }) => "index",
        Some(Cmd::Map) => "map",
        Some(Cmd::Find { .. }) => "find",
        Some(Cmd::Def { .. }) => "def",
        Some(Cmd::Callers { .. }) => "callers",
        Some(Cmd::Callees { .. }) => "callees",
        Some(Cmd::Outline { .. }) => "outline",
        Some(Cmd::Rank { .. }) => "rank",
        Some(Cmd::Impact { .. }) => "impact",
        Some(Cmd::Cochange { .. }) => "cochange",
        Some(Cmd::Context { .. }) => "context",
        Some(Cmd::Changes { .. }) => "changes",
        Some(Cmd::Usage { .. }) => "usage",
        None => "help",
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            output::diagnostic("error", "command_failed", format!("{error:#}"));
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let args = Cli::parse();
    output::configure(args.format, command_name(&args));

    // `--install` is a one-shot side task: copy the binary onto PATH and exit
    // before we open the database or touch the repo.
    if args.install {
        install::install()?;
        return Ok(ExitCode::SUCCESS);
    }

    let root = PathBuf::from(&args.root);

    // `--install-skill [AGENT]` writes the bundled repomap guide into the
    // target agent's conventional location and exits, before touching the db.
    if let Some(agent) = args.install_skill {
        skill::install_skill(&root, agent)?;
        return Ok(ExitCode::SUCCESS);
    }
    let db_path = args
        .db
        .clone()
        .unwrap_or_else(|| root.join(".repomap.db").to_string_lossy().to_string());
    let db_file = PathBuf::from(&db_path);

    // `--show-db` is a read-only diagnostic: report the path we resolved (and
    // its contents if it exists) without creating an empty database first.
    if args.show_db {
        query::show_db(&db_path)?;
        return Ok(ExitCode::SUCCESS);
    }

    // `--clear-db` removes the index file (incl. SQLite's WAL/SHM sidecars) and
    // exits, so we never reopen/recreate an empty database afterward.
    if args.clear_db {
        query::clear_db(&db_path)?;
        return Ok(ExitCode::SUCCESS);
    }

    let cmd = match args.cmd {
        Some(c) => c,
        None => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
            return Ok(ExitCode::SUCCESS);
        }
    };

    // Every repository command validates its root before the database can be
    // created or mutated. This prevents a typo plus a custom --db from wiping
    // an otherwise valid index as an apparently successful empty reindex.
    let root = index::canonical_root(&root)?;
    let mut conn = db::open(&db_path)?;
    index::check_root_binding(&conn, &root)?;

    // Query commands answer from a live index: refresh first (full when
    // nothing is indexed yet, incremental otherwise) so an agent that just
    // edited files never follows stale line numbers. `--no-refresh` opts out,
    // A refresh failure fails closed by default. `--allow-stale` is an
    // explicit escape hatch for read-only or temporarily broken checkouts.
    let mut stale = false;
    if !matches!(cmd, Cmd::Index { .. } | Cmd::Usage { .. }) && !args.no_refresh {
        if let Err(e) = index::refresh(&mut conn, &root, &db_file) {
            if args.allow_stale {
                stale = true;
                output::warning(
                    "stale_index",
                    format!("index refresh failed ({e}); answering from the existing index"),
                );
            } else {
                return Err(e);
            }
        }
    }

    let result_count = match cmd {
        Cmd::Index { incremental } => {
            let s = index::run(&mut conn, &root, incremental, &db_file)?;
            output::emit(
                "index_summary",
                serde_json::json!({
                    "files_indexed": s.files_indexed,
                    "files_skipped": s.files_skipped,
                    "files_removed": s.files_removed,
                    "symbols": s.symbols,
                    "edges": s.edges,
                    "services": s.services,
                    "mode": s.mode,
                }),
                format!(
                    "indexed {} files ({} skipped, {} removed), {} symbols, {} edges, {} services [{}]",
                    s.files_indexed,
                    s.files_skipped,
                    s.files_removed,
                    s.symbols,
                    s.edges,
                    s.services,
                    s.mode
                ),
            );
            None
        }
        Cmd::Map => {
            let results = query::map(&conn)?;
            record_query(&conn, "map", results)?;
            Some(results)
        }
        Cmd::Find {
            query,
            service,
            kind,
            lang,
            k,
        } => {
            let results = query::find(
                &conn,
                &query,
                &query::FindOpts {
                    service,
                    kind,
                    lang,
                    k,
                },
            )?;
            record_query(&conn, "find", results)?;
            Some(results)
        }
        Cmd::Def { selector: args } => {
            let results = query::def(&conn, &selector(args))?;
            record_query(&conn, "def", results)?;
            Some(results)
        }
        Cmd::Callers { selector: args } => {
            let results = query::callers(&conn, &selector(args))?;
            record_query(&conn, "callers", results)?;
            Some(results)
        }
        Cmd::Callees { selector: args } => {
            let results = query::callees(&conn, &selector(args))?;
            record_query(&conn, "callees", results)?;
            Some(results)
        }
        Cmd::Outline { file } => {
            let results = query::outline(&conn, &file)?;
            record_query(&conn, "outline", results)?;
            Some(results)
        }
        Cmd::Rank {
            service,
            k,
            include_tests,
        } => {
            let results = query::rank(&conn, service.as_deref(), k, include_tests)?;
            record_query(&conn, "rank", results)?;
            Some(results)
        }
        Cmd::Impact {
            selector: args,
            depth,
            k,
        } => {
            let results = query::impact(&conn, &selector(args), depth, k)?;
            record_query(&conn, "impact", results)?;
            Some(results)
        }
        Cmd::Cochange { file, commits, k } => {
            let results = history::cochange(&conn, &root, &file, commits, k)?;
            record_query(&conn, "cochange", results)?;
            Some(results)
        }
        Cmd::Context {
            query,
            budget,
            include_tests,
        } => {
            let results = context::context(&conn, &query, budget, include_tests)?;
            record_query(&conn, "context", results)?;
            Some(results)
        }
        Cmd::Changes { base, depth, k } => {
            let results = changes::run(&conn, &root, &base, depth, k)?;
            record_query(&conn, "changes", results)?;
            // An empty change set is a successful review, not a failed lookup.
            None
        }
        Cmd::Usage { reset } => {
            if reset {
                usage::reset(&conn)?;
            } else {
                usage::report(&conn)?;
            }
            None
        }
    };
    if stale {
        // Results were deliberately produced, but automation can distinguish
        // them from fresh success without parsing stderr.
        return Ok(ExitCode::from(4));
    }
    if result_count == Some(0) {
        return Ok(ExitCode::from(3));
    }
    Ok(ExitCode::SUCCESS)
}
