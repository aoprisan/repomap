//! Language-agnostic symbol/edge extraction driven by tree-sitter query
//! capture-name conventions (see queries/*.scm). Adding a language never
//! requires changing this file — only a new `lang/<x>.rs` + `queries/<x>.scm`.

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

/// A definition site discovered in a file. Lines are 1-based.
pub struct RawSymbol {
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
    pub doc_first_line: Option<String>,
}

/// A best-effort reference. `src_line` is the line of the reference; the
/// indexer attributes it to the innermost enclosing symbol. `dst_name` is
/// resolved to a symbol id by name after all files are indexed.
pub struct RawEdge {
    pub src_line: usize,
    pub dst_name: String,
    pub kind: String, // call | import | extends
}

pub struct Extracted {
    pub symbols: Vec<RawSymbol>,
    pub edges: Vec<RawEdge>,
}

/// Parse `src` with `language` and run `query_src`, interpreting captures by
/// name. A capture `def.<kind>` plus a `name` capture yields a symbol; the
/// `call.name` / `extends.name` captures and any `import.*` capture yield edges.
pub fn extract(
    src: &str,
    language: &tree_sitter::Language,
    query_src: &str,
) -> anyhow::Result<Extracted> {
    let mut parser = Parser::new();
    parser.set_language(language)?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow::anyhow!("parse failed"))?;
    let query = Query::new(language, query_src)?;
    let names = query.capture_names();
    let bytes = src.as_bytes();

    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, tree.root_node(), bytes);
    while let Some(m) = it.next() {
        // Locate the relevant nodes within this match by capture name.
        let mut def_node: Option<(Node, &str)> = None; // (whole def, kind)
        let mut name_node: Option<Node> = None;
        for cap in m.captures {
            let cname = names[cap.index as usize];
            if let Some(kind) = cname.strip_prefix("def.") {
                def_node = Some((cap.node, kind));
            } else if cname == "name" {
                name_node = Some(cap.node);
            } else if cname == "call.name" {
                push_edge(&mut edges, cap.node, bytes, "call", false);
            } else if cname == "extends.name" {
                push_edge(&mut edges, cap.node, bytes, "extends", false);
            } else if cname.starts_with("import") {
                push_edge(&mut edges, cap.node, bytes, "import", true);
            }
        }

        if let (Some((node, kind)), Some(nn)) = (def_node, name_node) {
            let name = text(nn, bytes).to_string();
            symbols.push(RawSymbol {
                name,
                kind: kind.to_string(),
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                signature: signature_of(node, bytes),
                doc_first_line: doc_of(node, bytes),
            });
        }
    }

    Ok(Extracted { symbols, edges })
}

fn push_edge(edges: &mut Vec<RawEdge>, node: Node, bytes: &[u8], kind: &str, is_import: bool) {
    let raw = text(node, bytes);
    let dst = if is_import {
        // `use a::b::c;` / `import a.b.c` -> last identifier-ish segment.
        last_segment(raw)
    } else {
        raw.to_string()
    };
    if !dst.is_empty() {
        edges.push(RawEdge {
            src_line: node.start_position().row + 1,
            dst_name: dst,
            kind: kind.to_string(),
        });
    }
}

fn text<'a>(node: Node, bytes: &'a [u8]) -> &'a str {
    node.utf8_text(bytes).unwrap_or("")
}

/// First line of the def, up to the body opener, whitespace-collapsed and
/// truncated. Language-agnostic and good enough for a compact pointer.
fn signature_of(node: Node, bytes: &[u8]) -> String {
    let full = text(node, bytes);
    let mut end = full.len();
    for (i, ch) in full.char_indices() {
        if ch == '{' || ch == '\n' {
            end = i;
            break;
        }
    }
    let one = full[..end].split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&one, 200)
}

/// Nearest preceding comment sibling's first line, markers stripped.
fn doc_of(node: Node, bytes: &[u8]) -> Option<String> {
    let prev = node.prev_sibling()?;
    if !prev.kind().contains("comment") {
        return None;
    }
    let raw = text(prev, bytes);
    let line = raw.lines().next().unwrap_or("");
    let cleaned = line
        .trim_start_matches("///")
        .trim_start_matches("/**")
        .trim_start_matches("/*")
        .trim_start_matches("//")
        .trim_start_matches('*')
        .trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(truncate(cleaned, 200))
    }
}

fn last_segment(path: &str) -> String {
    // Take the trailing run of identifier characters (handles `a::b::c`,
    // `a.b.c`, trailing `;`, `{..}` groups fall back to last bare ident).
    let trimmed = path.trim_end_matches([';', ' ', '\t', '_']);
    let seg: String = trimmed
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    seg
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}
