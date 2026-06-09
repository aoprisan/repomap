//! `--install-skill`: write the bundled Claude Code skill into the repo's
//! `.claude/skills/repomap/` directory. The skill teaches an agent how and when
//! to drive the `repomap` CLI. The skill source is embedded in the binary at
//! build time so the installed tool is self-contained — no source tree needed.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// The skill markdown, baked into the binary at compile time.
const SKILL_MD: &str = include_str!("../skills/repomap/SKILL.md");

/// Relative location of the skill within a `.claude` tree.
const SKILL_REL_DIR: &str = ".claude/skills/repomap";

/// Write the embedded skill into `<root>/.claude/skills/repomap/SKILL.md`,
/// creating parent directories as needed and overwriting any prior copy so a
/// re-install always lands the current skill text.
pub fn install_skill(root: &Path) -> Result<()> {
    let dir = root.join(SKILL_REL_DIR);
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating skill directory {}", dir.display()))?;
    let dest = dir.join("SKILL.md");
    fs::write(&dest, SKILL_MD)
        .with_context(|| format!("writing skill to {}", dest.display()))?;
    println!("Installed repomap skill -> {}", dest.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_skill_has_frontmatter() {
        // The bundled skill must be a valid skill: name + description frontmatter.
        assert!(SKILL_MD.starts_with("---"), "skill needs YAML frontmatter");
        assert!(SKILL_MD.contains("name: repomap"));
        assert!(SKILL_MD.contains("description:"));
    }

    #[test]
    fn install_skill_writes_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        install_skill(root).unwrap();
        let dest = root.join(SKILL_REL_DIR).join("SKILL.md");
        assert_eq!(fs::read_to_string(&dest).unwrap(), SKILL_MD);

        // Re-installing overwrites cleanly rather than erroring.
        install_skill(root).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), SKILL_MD);
    }
}
