//! Language registry — the single place to touch when adding a grammar.
//! New language = new `lang/<x>.rs` + `queries/<x>.scm` + one arm below.

mod extract;
mod rust;
mod scala;

pub use extract::Extracted;

use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Language {
    Scala,
    Rust,
    // Typescript, Elm — later: add an arm to each match below.
}

impl Language {
    /// Detect a language from a file extension, or `None` to skip the file.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "scala" | "sc" => Some(Language::Scala),
            "rs" => Some(Language::Rust),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Language::Scala => "scala",
            Language::Rust => "rust",
        }
    }

    fn ts_language(&self) -> tree_sitter::Language {
        match self {
            Language::Scala => scala::language(),
            Language::Rust => rust::language(),
        }
    }

    fn query_src(&self) -> &'static str {
        match self {
            Language::Scala => scala::QUERY,
            Language::Rust => rust::QUERY,
        }
    }

    /// Extract symbols + best-effort edges from one file's source.
    pub fn extract(&self, src: &str) -> anyhow::Result<Extracted> {
        extract::extract(src, &self.ts_language(), self.query_src())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::extract::{Extracted, RawEdge, RawSymbol};

    fn symbol<'a>(e: &'a Extracted, name: &str) -> &'a RawSymbol {
        e.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol named {name}"))
    }

    fn has_edge(e: &Extracted, kind: &str, dst: &str) -> bool {
        e.edges
            .iter()
            .any(|RawEdge { dst_name, kind: k, .. }| k == kind && dst_name == dst)
    }

    #[test]
    fn from_path_maps_extensions() {
        use std::path::Path;
        assert_eq!(Language::from_path(Path::new("a.rs")), Some(Language::Rust));
        assert_eq!(Language::from_path(Path::new("a.scala")), Some(Language::Scala));
        assert_eq!(Language::from_path(Path::new("a.sc")), Some(Language::Scala));
        assert_eq!(Language::from_path(Path::new("a.py")), None);
        assert_eq!(Language::from_path(Path::new("noext")), None);
    }

    #[test]
    fn rust_extraction_captures_symbols_edges_and_docs() {
        let src = "\
/// Adds one.
pub fn alpha(x: i32) -> i32 { beta(x) }
struct Widget;
impl Display for Widget {}
use std::collections::HashMap;
";
        let e = Language::Rust.extract(src).unwrap();

        let alpha = symbol(&e, "alpha");
        assert_eq!(alpha.kind, "fn");
        assert_eq!(alpha.start_line, 2);
        assert_eq!(alpha.signature, "pub fn alpha(x: i32) -> i32");
        assert_eq!(alpha.doc_first_line.as_deref(), Some("Adds one."));

        assert_eq!(symbol(&e, "Widget").kind, "struct");

        assert!(has_edge(&e, "call", "beta"), "call edge to beta");
        assert!(has_edge(&e, "extends", "Display"), "impl Trait for Type");
        assert!(has_edge(&e, "import", "HashMap"), "use's last segment");
    }

    #[test]
    fn scala_extraction_captures_objects_defs_and_edges() {
        let src = "\
object Foo extends Bar {
  def baz(x: Int): Int = qux(x)
}
";
        let e = Language::Scala.extract(src).unwrap();

        let foo = symbol(&e, "Foo");
        assert_eq!(foo.kind, "object");
        assert_eq!(foo.signature, "object Foo extends Bar");

        assert_eq!(symbol(&e, "baz").kind, "def");
        assert!(has_edge(&e, "call", "qux"), "call edge to qux");
        assert!(has_edge(&e, "extends", "Bar"), "extends edge to Bar");
    }
}
