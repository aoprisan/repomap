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
