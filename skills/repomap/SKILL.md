---
name: repomap
description: Navigate a polyglot codebase fast using the `repomap` CLI — a compact code index (file:line + signature + service, never code bodies). Use BEFORE grepping or reading files broadly to locate a symbol's definition, find who calls it, search symbols by name, or get a high-level map of services. Trigger phrases include "where is X defined", "who calls X", "find the X function/class", "what services are in this repo", "map the codebase", "navigate the code".
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
from the index as-is.

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

All output is one pointer per line. Global flags work on any subcommand:
`--root <dir>` (repo root, default `.`), `--db <path>` (default
`<root>/.repomap.db`).

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

### `repomap def <symbol>`
Definition site(s) of an exact symbol name. Use to jump straight to where
something is declared.

```sh
repomap def TaxCalculator
```

### `repomap callers <symbol>`
Symbols with an edge (call / `extends` / import) pointing at `<symbol>` — i.e.
who uses it. Each line ends with the edge kind, e.g. `(call)`.

```sh
repomap callers get
```

### `repomap callees <symbol>`
The inverse: symbols that `<symbol>` points at — what it calls, extends, or
imports. Use it to see a function's dependencies before changing it.

```sh
repomap callees total
```

> **Caveat:** edges are resolved best-effort by name. A bare reference resolves
> within the source's **own service** (same-file definition preferred); a name
> the source file **imports** may also resolve across services. Anything else
> is dropped rather than guessed — so `callers`/`callees` can still miss some
> cross-service links. Treat their output as a strong hint, not an exhaustive
> list, and confirm by reading the cited `file:line`.

### `repomap outline <file>`
All symbols defined in one file, in source order. Accepts the exact
repo-relative path or a suffix (`outline Invoice.scala`). Run this **before
editing a file** to see its shape without reading it whole.

```sh
repomap outline src/query.rs
repomap outline Invoice.scala
```

## Typical workflow

1. `repomap map` — see the services and stacks at a glance.
2. `repomap find <name>` — locate candidate symbols by fuzzy name.
3. `repomap def <name>` — pin the exact definition site(s).
4. `repomap callers <name>` / `repomap callees <name>` — see who depends on it
   and what it depends on before changing it.
5. `repomap outline <file>` — map a file's shape before opening or editing it.
6. Open the reported `path:Lstart` to read the real code.

## Notes

- Results are **pointers, not code** — always open the cited location to read or
  edit the actual source.
- Query results are never stale: each query auto-refreshes the index against
  the working tree first (`--no-refresh` skips this).
- Languages indexed today: Rust, Scala, Ruby, Python, TypeScript. Symbols in
  other languages won't appear.
