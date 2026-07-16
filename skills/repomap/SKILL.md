---
name: repomap
description: Navigate and review a polyglot codebase fast using the `repomap` CLI — a compact code index (file:line + signature + service, never code bodies). Use BEFORE grepping or reading files broadly to locate definitions, find callers, map services, measure blast radius, find historically coupled files, or turn a working-tree diff into a semantic review and test plan. Trigger phrases include "where is X defined", "who calls X", "map the codebase", "what breaks if I change X", "what tests should I run", "review my changes", "what else do I need to update", "what's important in this repo", "orient me on this task".
---

# repomap

`repomap` is a CLI that indexes the repository into a SQLite/FTS5 database and
answers code-navigation queries as **compact pointers** — one line each, of the
form `path:Lstart  <signature>  [enclosing]`, where `path` is the repo-relative
file path, openable exactly as printed. It returns locations and signatures,
never code bodies, so it is cheap to drop into context.

Reach for it **before** broad `grep`/`find`/file reading: one `repomap` query
usually replaces many file reads when you need to locate a symbol, see its
callers, or understand how the repo is organized. Once it points you at a
`file:line`, open that exact spot to read the actual code.

## Setup (none required)

The index lives in `./.repomap.db` and **maintains itself**: every query
command first refreshes the index (a full build on first use, an incremental
one after), so you can run `repomap find …` immediately and after editing
files — results always reflect the working tree. Pass `--no-refresh` to answer
from the index as-is. Refresh failures fail closed; `--allow-stale` explicitly
returns the older index with exit status 4.

To build or refresh explicitly:

```sh
repomap index                 # full (re)index of the repo
repomap index --incremental   # skip unchanged files (stat, then git-hash check)
```

(`--incremental` automatically upgrades to a full reindex if the service
definitions in `repomap.toml` changed since the last run.)

Check whether an index exists and is fresh:

```sh
repomap --show-db             # resolved db path + size, row counts, indexed time
```

If `repomap` is not on PATH, build and install it: `cargo build --release &&
./target/release/repomap --install`.

## Commands

Text output is one pointer per line. Global flags work on any subcommand:
`--root <dir>` (repo root, default `.`), `--db <path>` (default
`<root>/.repomap.db`), and `--format jsonl` for versioned structured output.
Exit 3 means a lookup had no results; exit 4 means `--allow-stale` returned
results from an older index; other nonzero statuses are failures.

### `repomap map`
List services as `name  (stack)  N files  entrypoint`. Start here to orient in
an unfamiliar repo.

### `repomap find <query> [--service S] [--kind K] [--lang L] [-k N]`
Full-text symbol search. Each bareword is matched as a prefix, so `find handle`
matches `handleRequest`. `-k` caps results (default 10). Filter with
`--service`, `--kind`, `--lang`. `--kind` takes the language-native kind
(`fn`, `def`, `struct`, `class`, `object`, …) or the generic alias `function`
(matches `fn`/`def`/`method`) or `module` (matches `mod`).

```sh
repomap find Invoice              # symbols matching "Invoice"
repomap find handle --lang rust   # Rust symbols starting "handle"
repomap find get --service billing -k 5
```

### `repomap def <symbol> [--service S] [--file F] [--kind K]`
Definition site(s) of an exact symbol name. Use to jump straight to where
something is declared.

```sh
repomap def TaxCalculator
```

### `repomap callers <symbol> [--service S] [--file F] [--kind K]`
Symbols with an edge (call / `extends` / import) pointing at `<symbol>` — i.e.
who uses it. Each line ends with the edge kind, e.g. `(call)`.

```sh
repomap callers get
```

### `repomap callees <symbol> [--service S] [--file F] [--kind K]`
The inverse: symbols that `<symbol>` points at — what it calls, extends, or
imports. Use it to see a function's dependencies before changing it.

```sh
repomap callees total
```

`<symbol>` may be qualified (`InvoiceService::get`). Use `--service`, `--file`
(exact path or suffix), and `--kind` when names repeat.

> **Caveat:** calls belong to the innermost callable. Bare calls prefer the
> same lexical container and otherwise resolve only to a unique same-file or
> same-service definition. Qualified calls resolve to a method owned by the
> qualifier, or — for scoped paths (`index::refresh`) and imported modules
> (`import pricing; pricing.unit_price(x)`) — to the unique top-level
> definition in that module's file. Ambiguous or unknown instance receivers
> are dropped instead of guessed. Imports may license unique cross-service
> links. The graph is deliberately conservative and can omit dynamic dispatch.

### `repomap outline <file>`
All symbols defined in one file, in source order. Accepts the exact
repo-relative path or a suffix (`outline Invoice.scala`). Run this **before
editing a file** to see its shape without reading it whole.

```sh
repomap outline src/query.rs
repomap outline Invoice.scala
```

### `repomap rank [--service S] [-k N] [--include-tests]`
The most structurally important symbols, by PageRank over the reference graph
(score 100 = top symbol in scope; caller count shown alongside). The fastest
way to learn what an unfamiliar repo actually revolves around — run it before
diving in, or per `--service` to orient inside one service. Test code neither
appears nor votes by default; `--include-tests` lists it.

```sh
repomap rank -k 10
repomap rank --service billing
```

### `repomap impact <symbol> [--service S] [--file F] [--kind K] [--depth N] [-k N]`
Transitive blast radius: callers, callers-of-callers, … up to `--depth` hops
(default 2), each tagged `(depth d)`, nearest first, with a closing summary of
symbols/files/services touched. Run this **before changing a shared symbol**
to know what could break and how far the change ripples.

```sh
repomap impact resolve_edges
repomap impact get --depth 3
```

### `repomap cochange <file> [-k N]`
Files that historically change **in the same commit** as `<file>` (mined from
git history), with co-change counts and confidence. This surfaces coupling no
static analysis sees — schema + DAO, config + reader, mirrored types. Run it
after deciding to edit a file and **check whether the top partners also need
your change**. Accepts a path suffix; also works for unindexed files (`.sql`,
`.yml`, …).

```sh
repomap cochange src/query.rs
repomap cochange schema.sql -k 5
```

### `repomap context <query> [--budget N] [--include-tests]`
A one-shot orientation pack for a task: seed symbols matching `<query>`
(any-word match), each with its top callers (`<-`) and callees (`->`), plus
the services involved, packed to fit `--budget` tokens (default 2000). Test
symbols are excluded unless `--include-tests`. Use it as the **first command
for a new task** instead of stitching together find / def / callers /
callees; the footer says how much budget was used and whether seeds were cut.

```sh
repomap context "invoice tax rounding"
repomap context "index refresh" --budget 800
```

If one result cannot fit, the command reports the minimum required budget
instead of exceeding the limit.

### `repomap changes [--base REV] [--depth N] [-k N]`
Analyze the actual working-tree diff before declaring a change finished. It
classifies symbol additions/deletions/signature/body changes, shows transitive
callers that deserve review, and selects graph-linked tests. Deleted symbols
still get a blast radius because repomap retains scoped unresolved references.
Untracked and non-indexed files are also reported.

```sh
repomap changes                 # compare working tree with HEAD
repomap changes --base main     # review the whole branch against main
repomap changes --depth 4 -k 50
```

Treat the test list as a high-signal minimum, not proof that no other suite is
needed. When no graph-linked test is found, run the affected service's suite.

### `repomap usage [--reset]`
Shows lifetime query runs, result counts, and the estimated tokens saved by
using compact pointers. Run `repomap usage --reset` to clear the totals.

## Typical workflow

1. `repomap context "<task words>"` — one-shot orientation: seeds + neighbors
   + services for the task at hand. (Or `repomap map` + `repomap rank` to
   orient in the repo as a whole.)
2. `repomap find <name>` — locate candidate symbols by fuzzy name.
3. `repomap def <name>` — pin the exact definition site(s).
4. `repomap callers <name>` / `repomap callees <name>` — see who depends on it
   and what it depends on before changing it.
5. `repomap impact <name>` — blast radius before you change something shared.
6. `repomap outline <file>` — map a file's shape before opening or editing it.
7. `repomap cochange <file>` — files that historically change with the one
   you're editing; check whether they need the same change.
8. `repomap changes` — after editing, review semantic risk and run the linked
   tests before broad fallback suites.
9. Open the reported `path:Lstart` to read the real code.

## Notes

- Results are **pointers, not code** — always open the cited location to read or
  edit the actual source.
- Query results are never stale: each query auto-refreshes the index against
  the working tree first (`--no-refresh` skips this).
- Languages indexed today: Rust, Scala, Ruby, Python, TypeScript. Symbols in
  other languages won't appear.
