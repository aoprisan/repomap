---
name: repomap
description: Navigate a polyglot codebase fast using the `repomap` CLI — a compact code index (file:line + signature + service, never code bodies). Use BEFORE grepping or reading files broadly to locate a symbol's definition, find who calls it, search symbols by name, or get a high-level map of services. Trigger phrases include "where is X defined", "who calls X", "find the X function/class", "what services are in this repo", "map the codebase", "navigate the code".
---

# repomap

`repomap` is a CLI that indexes the repository into a SQLite/FTS5 database and
answers code-navigation queries as **compact pointers** — one line each, of the
form `service/path:Lstart  <signature>  [enclosing]`. It returns locations and
signatures, never code bodies, so it is cheap to drop into context.

Reach for it **before** broad `grep`/`find`/file reading: one `repomap` query
usually replaces many file reads when you need to locate a symbol, see its
callers, or understand how the repo is organized. Once it points you at a
`file:line`, open that exact spot to read the actual code.

## Setup (once per repo)

The index lives in `./.repomap.db`. If queries say "not indexed yet" or return
nothing, build it first:

```sh
repomap index                 # full (re)index of the repo
repomap index --incremental   # skip files whose git blob hash is unchanged
```

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

> **Caveat:** edges are resolved best-effort by name, **scoped to the source's
> own service** (same-file definition preferred). A bare reference with no
> same-service definition is dropped rather than guessed — so `callers` can miss
> genuine cross-service calls. Treat its output as a strong hint, not an
> exhaustive list, and confirm by reading the cited `file:line`.

## Typical workflow

1. `repomap map` — see the services and stacks at a glance.
2. `repomap find <name>` — locate candidate symbols by fuzzy name.
3. `repomap def <name>` — pin the exact definition site(s).
4. `repomap callers <name>` — see who depends on it before changing it.
5. Open the reported `service/path:Lstart` to read the real code.

## Notes

- Results are **pointers, not code** — always open the cited location to read or
  edit the actual source.
- If results look stale after you edit files, re-run `repomap index
  --incremental`.
- Languages indexed today: Rust, Scala, Ruby, Python, TypeScript. Symbols in
  other languages won't appear.
