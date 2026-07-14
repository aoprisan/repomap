//! Language registry — the single place to touch when adding a grammar.
//! New language = new `lang/<x>.rs` + `queries/<x>.scm` + one arm below.

mod extract;
mod python;
mod ruby;
mod rust;
mod scala;
mod typescript;

pub use extract::{Extracted, RawSymbol};

use std::path::Path;
use std::sync::OnceLock;

use tree_sitter::Query;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Language {
    Scala,
    Rust,
    Ruby,
    Python,
    Typescript,
    /// `.tsx` — same query as TypeScript but the JSX-aware grammar.
    Tsx,
    // Elm — later: add an arm to each match below.
}

impl Language {
    /// Detect a language from a file extension, or `None` to skip the file.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "scala" | "sc" => Some(Language::Scala),
            "rs" => Some(Language::Rust),
            "rb" => Some(Language::Ruby),
            "py" => Some(Language::Python),
            "ts" => Some(Language::Typescript),
            "tsx" => Some(Language::Tsx),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Language::Scala => "scala",
            Language::Rust => "rust",
            Language::Ruby => "ruby",
            Language::Python => "python",
            // Both grammars index (and are queried) as one language.
            Language::Typescript | Language::Tsx => "typescript",
        }
    }

    fn ts_language(&self) -> tree_sitter::Language {
        match self {
            Language::Scala => scala::language(),
            Language::Rust => rust::language(),
            Language::Ruby => ruby::language(),
            Language::Python => python::language(),
            Language::Typescript => typescript::language(),
            Language::Tsx => typescript::language_tsx(),
        }
    }

    /// The compiled query for this grammar, built once and reused for every
    /// file (including across indexing threads — `Query` is `Sync`).
    /// Compiling the query is the expensive half of extraction; doing it per
    /// file dominated indexing time. Tsx shares TypeScript's query *source*
    /// but needs its own compiled copy, since a `Query` is bound to the
    /// grammar it was compiled against. The sources are bundled and every
    /// language is exercised by tests, so a compile failure here is a build
    /// defect, not a runtime condition — hence the `expect`.
    fn compiled_query(&self) -> &'static Query {
        fn get(
            cell: &'static OnceLock<Query>,
            lang: tree_sitter::Language,
            src: &str,
        ) -> &'static Query {
            cell.get_or_init(|| {
                Query::new(&lang, src).expect("bundled tree-sitter query must compile")
            })
        }
        macro_rules! cached {
            ($lang:expr, $src:expr) => {{
                static Q: OnceLock<Query> = OnceLock::new();
                get(&Q, $lang, $src)
            }};
        }
        match self {
            Language::Scala => cached!(scala::language(), scala::QUERY),
            Language::Rust => cached!(rust::language(), rust::QUERY),
            Language::Ruby => cached!(ruby::language(), ruby::QUERY),
            Language::Python => cached!(python::language(), python::QUERY),
            Language::Typescript => cached!(typescript::language(), typescript::QUERY),
            Language::Tsx => cached!(typescript::language_tsx(), typescript::QUERY),
        }
    }

    /// Extract symbols + best-effort edges from one file's source.
    pub fn extract(&self, src: &str) -> anyhow::Result<Extracted> {
        extract::extract(src, &self.ts_language(), self.compiled_query())
    }
}

/// Combine syntax/name hints from extraction with cross-language file naming
/// conventions. Keeping this in the language layer makes index-time and
/// working-tree diff analysis agree on what counts as a test.
pub fn is_test_symbol(path: &str, symbol: &RawSymbol) -> bool {
    if symbol.is_test_hint {
        return true;
    }
    let lower = path.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);
    let conventional_name = matches!(
        Path::new(path).extension().and_then(|e| e.to_str()),
        Some("py" | "rb")
    ) && (symbol.name.to_ascii_lowercase().starts_with("test_")
        || symbol.name.to_ascii_lowercase().ends_with("_test")
        || symbol.name.to_ascii_lowercase().starts_with("should_"));
    lower
        .split('/')
        .any(|part| matches!(part, "test" | "tests" | "spec" | "specs" | "__tests__"))
        || file.starts_with("test_")
        || file.contains("_test.")
        || file.contains(".test.")
        || file.contains("_spec.")
        || file.contains(".spec.")
        || conventional_name
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
        e.edges.iter().any(
            |RawEdge {
                 dst_name, kind: k, ..
             }| k == kind && dst_name == dst,
        )
    }

    #[test]
    fn from_path_maps_extensions() {
        use std::path::Path;
        assert_eq!(Language::from_path(Path::new("a.rs")), Some(Language::Rust));
        assert_eq!(
            Language::from_path(Path::new("a.scala")),
            Some(Language::Scala)
        );
        assert_eq!(
            Language::from_path(Path::new("a.sc")),
            Some(Language::Scala)
        );
        assert_eq!(Language::from_path(Path::new("a.rb")), Some(Language::Ruby));
        assert_eq!(
            Language::from_path(Path::new("a.py")),
            Some(Language::Python)
        );
        assert_eq!(
            Language::from_path(Path::new("a.ts")),
            Some(Language::Typescript)
        );
        assert_eq!(Language::from_path(Path::new("a.tsx")), Some(Language::Tsx));
        assert_eq!(Language::from_path(Path::new("a.txt")), None);
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

    #[test]
    fn ruby_extraction_captures_classes_methods_and_edges() {
        let src = "\
# A widget.
class Widget < Base
  def render(x)
    draw(x)
  end
end

module Helpers
  def self.format(s)
    s
  end
end
";
        let e = Language::Ruby.extract(src).unwrap();

        let widget = symbol(&e, "Widget");
        assert_eq!(widget.kind, "class");
        assert_eq!(widget.signature, "class Widget < Base");
        assert_eq!(widget.doc_first_line.as_deref(), Some("A widget."));

        assert_eq!(symbol(&e, "render").kind, "method");
        assert_eq!(symbol(&e, "Helpers").kind, "module");
        assert_eq!(symbol(&e, "format").kind, "method"); // singleton_method

        assert!(has_edge(&e, "call", "draw"), "call edge to draw");
        assert!(has_edge(&e, "extends", "Base"), "superclass edge to Base");
    }

    #[test]
    fn python_extraction_captures_classes_functions_and_edges() {
        let src = "\
import os.path
from collections import OrderedDict

# A widget.
class Widget(Base):
    def render(self, x):
        return draw(x)
";
        let e = Language::Python.extract(src).unwrap();

        let widget = symbol(&e, "Widget");
        assert_eq!(widget.kind, "class");
        assert_eq!(widget.signature, "class Widget(Base):");
        assert_eq!(widget.doc_first_line.as_deref(), Some("A widget."));

        assert_eq!(symbol(&e, "render").kind, "def");

        assert!(has_edge(&e, "call", "draw"), "call edge to draw");
        assert!(has_edge(&e, "extends", "Base"), "base-class edge to Base");
        assert!(
            has_edge(&e, "import", "path"),
            "import os.path last segment"
        );
        assert!(
            has_edge(&e, "import", "OrderedDict"),
            "from-import last segment"
        );
    }

    #[test]
    fn typescript_extraction_captures_classes_funcs_and_edges() {
        let src = "\
import { Base } from './base';

/** A widget. */
export class Widget extends Base implements Drawable {
  render(x: number): number {
    return draw(x);
  }
}

interface Drawable {}

export function helper(n: number): number { return n; }
const arrow = (y: number) => helper(y);
";
        let e = Language::Typescript.extract(src).unwrap();

        let widget = symbol(&e, "Widget");
        assert_eq!(widget.kind, "class");
        assert_eq!(widget.doc_first_line.as_deref(), Some("A widget."));

        assert_eq!(symbol(&e, "render").kind, "method");
        assert_eq!(symbol(&e, "Drawable").kind, "interface");
        assert_eq!(symbol(&e, "helper").kind, "function");
        assert_eq!(symbol(&e, "arrow").kind, "function"); // const arrow fn

        assert!(has_edge(&e, "call", "draw"), "call edge to draw");
        assert!(has_edge(&e, "extends", "Base"), "extends edge to Base");
        assert!(
            has_edge(&e, "extends", "Drawable"),
            "implements edge to Drawable"
        );
        assert!(has_edge(&e, "import", "Base"), "named import Base");
    }

    #[test]
    fn tsx_extraction_survives_jsx() {
        // With the plain-TS grammar this file parsed into errors and yielded
        // zero symbols — even `after`, which sits below the JSX.
        let src = "\
const Card = ({t}: {t: string}) => <div className=\"c\">{t}</div>;
export function List(items: string[]) {
  return <ul>{items.map(i => <Card t={i}/>)}</ul>;
}
export function after(): number { return render(1); }
";
        let e = Language::Tsx.extract(src).unwrap();

        assert_eq!(symbol(&e, "Card").kind, "function");
        assert_eq!(symbol(&e, "List").kind, "function");
        assert_eq!(symbol(&e, "after").kind, "function");
        assert!(
            has_edge(&e, "call", "render"),
            "call edge inside a TSX file"
        );
    }

    #[test]
    fn test_detection_combines_annotations_paths_and_language_conventions() {
        let rust = Language::Rust
            .extract(
                "fn helper() { let marker = \"#[test]\"; }\n#[test]\nfn arbitrary_name() {}\nfn after_test() {}\n",
            )
            .unwrap();
        assert!(!is_test_symbol("src/lib.rs", symbol(&rust, "helper")));
        assert!(is_test_symbol(
            "src/lib.rs",
            symbol(&rust, "arbitrary_name")
        ));
        assert!(!is_test_symbol("src/lib.rs", symbol(&rust, "after_test")));

        let python = Language::Python
            .extract("def test_total():\n    pass\ndef helper():\n    pass\n")
            .unwrap();
        assert!(is_test_symbol("billing.py", symbol(&python, "test_total")));
        assert!(!is_test_symbol("billing.py", symbol(&python, "helper")));

        let typescript = Language::Typescript
            .extract("export function helper() {}\n")
            .unwrap();
        assert!(is_test_symbol(
            "src/widget.test.ts",
            symbol(&typescript, "helper")
        ));
    }
}
