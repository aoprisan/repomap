use clap::{Parser, Subcommand};

use crate::skill::Agent;

#[derive(Parser)]
#[command(name = "repomap", version, about = "Compact code-navigation index for LLM agents")]
pub struct Cli {
    /// Repo root to index/query.
    #[arg(long, global = true, default_value = ".")]
    pub root: String,

    /// Index database path (default: <root>/.repomap.db).
    #[arg(long, global = true)]
    pub db: Option<String>,

    /// Copy this binary into a `bin` directory on your PATH and exit.
    #[arg(long, global = true)]
    pub install: bool,

    /// Install the bundled repomap guide for a coding agent into `<root>` and
    /// exit. Target one of `claude` (default), `copilot`, or `codex`.
    #[arg(
        long,
        global = true,
        value_name = "AGENT",
        num_args = 0..=1,
        default_missing_value = "claude"
    )]
    pub install_skill: Option<Agent>,

    /// Print the resolved index database path (with stats) and exit.
    #[arg(long, global = true)]
    pub show_db: bool,

    /// Delete the resolved index database and exit.
    #[arg(long, global = true)]
    pub clear_db: bool,

    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// (Re)index the repo. --incremental skips files whose git hash is unchanged.
    Index {
        #[arg(long)]
        incremental: bool,
    },
    /// List services: `name  (stack)  N files  entrypoint`.
    Map,
    /// Search symbols; each hit one line: `path:Lstart  <sig>  [enclosing]`.
    Find {
        query: String,
        #[arg(long)]
        service: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(short = 'k', default_value_t = 10)]
        k: usize,
    },
    /// Definition site(s) of a symbol, one line each.
    Def { symbol: String },
    /// Symbols with an edge pointing at <symbol>, one line each.
    Callers { symbol: String },
}
