//! Language-agnostic symbol/edge extraction driven by tree-sitter query
//! capture-name conventions (see queries/*.scm). Adding a language never
//! requires changing this file — only a new `lang/<x>.rs` + `queries/<x>.scm`.

use std::cell::RefCell;

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

thread_local! {
    // One reusable parser per thread: `Parser::new` allocates, while
    // `set_language` is cheap enough to call once per file.
    static PARSER: RefCell<Parser> = RefCell::new(Parser::new());
}

/// A definition site discovered in a file. Lines are 1-based.
pub struct RawSymbol {
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
    pub doc_first_line: Option<String>,
    /// Language-level evidence that this is a test (annotation/decorator or
    /// conventional test name). File-path conventions are added by the
    /// indexer, which has the repo-relative path.
    pub is_test_hint: bool,
}

/// A best-effort reference. `src_line` is the line of the reference; the
/// indexer attributes it to the innermost enclosing symbol. `dst_name` is
/// resolved to a symbol id by name after all files are indexed.
pub struct RawEdge {
    pub src_line: usize,
    pub dst_name: String,
    /// Last identifier before a member/scoped call (`TaxCalculator` in
    /// `TaxCalculator.withTax`). `None` denotes a bare call.
    pub qualifier: Option<String>,
    /// True when the qualifier came from a scoped path (Rust `a::b()`) rather
    /// than a member/attribute access. Scoped paths name modules and types —
    /// never runtime values — so resolution may match them against file
    /// modules without import evidence.
    pub scoped: bool,
    pub kind: String, // call | import | extends
}

pub struct Extracted {
    pub symbols: Vec<RawSymbol>,
    pub edges: Vec<RawEdge>,
}

/// Parse `src` with `language` and run the pre-compiled `query`, interpreting
/// captures by name. A capture `def.<kind>` plus a `name` capture yields a
/// symbol; the `call.name` / `extends.name` captures and any `import.*`
/// capture yield edges.
pub fn extract(
    src: &str,
    language: &tree_sitter::Language,
    query: &Query,
) -> anyhow::Result<Extracted> {
    let tree = PARSER.with(|p| -> anyhow::Result<_> {
        let mut parser = p.borrow_mut();
        parser.set_language(language)?;
        parser
            .parse(src, None)
            .ok_or_else(|| anyhow::anyhow!("parse failed"))
    })?;
    let names = query.capture_names();
    let bytes = src.as_bytes();

    let mut symbols = Vec::new();
    let mut edges = Vec::new();

    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(query, tree.root_node(), bytes);
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
            let is_test_hint = test_hint(node, bytes);
            symbols.push(RawSymbol {
                name,
                kind: kind.to_string(),
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                signature: signature_of(node, bytes),
                doc_first_line: doc_of(node, bytes),
                is_test_hint,
            });
        }
    }

    Ok(Extracted { symbols, edges })
}

/// Detect common test markers without binding the generic extractor to a
/// particular test framework. Looking at the definition plus a few preceding
/// lines covers Rust attributes and Python/Scala decorators; conventional
/// names cover unittest/pytest and many Rust test suites.
fn test_hint(node: Node, bytes: &[u8]) -> bool {
    let has_marker = |s: &str| {
        let lower = s.to_ascii_lowercase();
        lower.contains("#[test]")
            || lower.contains("::test]")
            || lower.contains("cfg(test")
            || lower.contains("@test")
            || lower.contains("@pytest")
    };
    let source = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return false;
    }
    let start = node.start_position().row;
    // Inspect only the declaration line, never the body: a function that
    // parses source or test annotations may legitimately contain the literal
    // string "#[test]" without being a test itself.
    if lines
        .get(start)
        .and_then(|line| line.split('{').next())
        .is_some_and(has_marker)
    {
        return true;
    }
    // Fall back to an immediately preceding annotation block. Stop at the
    // first line of code so a helper following a test cannot inherit the
    // earlier function's marker.
    let mut annotations = Vec::new();
    for line in lines[..start.min(lines.len())].iter().rev().take(5) {
        let trimmed = line.trim();
        if trimmed.starts_with("#[") || trimmed.starts_with('@') {
            annotations.push(trimmed);
        } else if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        } else {
            break;
        }
    }
    has_marker(&annotations.join(" "))
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
        let (qualifier, scoped) = if kind == "call" {
            match call_qualifier(node, bytes) {
                Some((q, scoped)) => (Some(q), scoped),
                None => (None, false),
            }
        } else {
            (None, false)
        };
        edges.push(RawEdge {
            src_line: node.start_position().row + 1,
            dst_name: dst,
            qualifier,
            scoped,
            kind: kind.to_string(),
        });
    }
}

/// Recover the syntactic receiver/module for a captured member or scoped
/// call, plus whether the syntax was a scoped path. We deliberately keep only
/// its last identifier: it is enough to link `TaxCalculator.withTax` to a
/// method owned by `TaxCalculator`, while an instance call such as
/// `store.get` will stay unresolved instead of being guessed to an unrelated
/// same-named method.
fn call_qualifier(node: Node, bytes: &[u8]) -> Option<(String, bool)> {
    let parent = node.parent()?;
    let kind = parent.kind();
    // `scope` covers both Rust's scoped_identifier and Ruby's scope_resolution.
    let scoped = kind.contains("scope");
    if !(scoped || kind.contains("field") || kind.contains("member") || kind.contains("attribute"))
    {
        return None;
    }
    let prefix = bytes.get(parent.start_byte()..node.start_byte())?;
    let prefix = std::str::from_utf8(prefix).ok()?;
    Some((last_identifier(prefix)?, scoped))
}

fn last_identifier(s: &str) -> Option<String> {
    let ident: String = s
        .chars()
        .rev()
        .skip_while(|c| !(c.is_alphanumeric() || *c == '_'))
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    (!ident.is_empty()).then_some(ident)
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

/// Nearest preceding comment sibling's first line, markers stripped. When the
/// def has no preceding sibling (e.g. a TS `export`-wrapped declaration), look
/// at the wrapping parent's preceding sibling instead.
fn doc_of(node: Node, bytes: &[u8]) -> Option<String> {
    // The def's own preceding sibling, or — for `export`-wrapped declarations
    // where that sibling is the `export` keyword — the wrapper's. Take whichever
    // is actually a comment.
    let prev = [
        node.prev_sibling(),
        node.parent().and_then(|p| p.prev_sibling()),
    ]
    .into_iter()
    .flatten()
    .find(|n| n.kind().contains("comment"))?;
    let raw = text(prev, bytes);
    let line = raw.lines().next().unwrap_or("");
    let cleaned = line
        .trim_start_matches("///")
        .trim_start_matches("/**")
        .trim_start_matches("/*")
        .trim_start_matches("//")
        .trim_start_matches('#') // Ruby/shell line comments
        .trim_start_matches('*')
        .trim_end_matches("*/") // close of a `/** ... */` block comment
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_segment_takes_the_trailing_identifier() {
        assert_eq!(last_segment("a::b::c"), "c");
        assert_eq!(last_segment("a.b.c"), "c");
        assert_eq!(last_segment("a.b.c;"), "c"); // trailing punctuation stripped
        assert_eq!(last_segment("use std::collections::HashMap;"), "HashMap");
        assert_eq!(last_segment("plain"), "plain");
    }

    #[test]
    fn last_identifier_skips_member_punctuation() {
        assert_eq!(
            last_identifier("TaxCalculator."),
            Some("TaxCalculator".into())
        );
        assert_eq!(last_identifier("crate::billing::"), Some("billing".into()));
        assert_eq!(last_identifier("."), None);
    }

    #[test]
    fn truncate_is_char_aware_and_appends_ellipsis() {
        assert_eq!(truncate("abc", 10), "abc"); // under the limit, unchanged
        assert_eq!(truncate("abc", 3), "abc"); // exactly at the limit
        assert_eq!(truncate("abcdef", 3), "abc…");
        // Counts characters, not bytes: a 3-char multibyte string is untouched.
        assert_eq!(truncate("é€λ", 3), "é€λ");
    }
}
