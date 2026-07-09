//! `--install-skill [AGENT]`: write the bundled repomap agent guide into the
//! conventional location for the target coding agent (Claude Code, GitHub
//! Copilot, or OpenAI Codex). The guide teaches an agent how and when to drive
//! the `repomap` CLI. Its source is embedded in the binary at build time so the
//! installed tool is self-contained — no source tree needed.
//!
//! Claude Code reads a dedicated, repomap-owned skill file, so we overwrite it
//! wholesale. Copilot and Codex read a *shared* instructions file that may hold
//! the user's own content, so there we splice our text into a marked block and
//! leave everything else untouched.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// The skill markdown, baked into the binary at compile time.
const SKILL_MD: &str = include_str!("../skills/repomap/SKILL.md");

/// Relative location of the skill within a Claude `.claude` tree.
const CLAUDE_REL_PATH: &str = ".claude/skills/repomap/SKILL.md";

/// Relative location Copilot reads repository-wide custom instructions from.
const COPILOT_REL_PATH: &str = ".github/copilot-instructions.md";

/// Relative location Codex reads agent instructions from (repo root).
const CODEX_REL_PATH: &str = "AGENTS.md";

/// Markers bounding repomap's section inside a shared instructions file, so a
/// re-install replaces just our block and preserves any user-authored content.
const BEGIN_MARKER: &str = "<!-- BEGIN repomap (managed by `repomap --install-skill`) -->";
const END_MARKER: &str = "<!-- END repomap -->";

/// Which coding agent to install the guide for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Agent {
    /// Claude Code — `.claude/skills/repomap/SKILL.md`.
    Claude,
    /// GitHub Copilot — `.github/copilot-instructions.md`.
    Copilot,
    /// OpenAI Codex — `AGENTS.md`.
    Codex,
}

/// Install the embedded guide for `agent` under `root`.
pub fn install_skill(root: &Path, agent: Agent) -> Result<()> {
    match agent {
        Agent::Claude => install_owned(root, CLAUDE_REL_PATH),
        Agent::Copilot => install_shared(root, COPILOT_REL_PATH),
        Agent::Codex => install_shared(root, CODEX_REL_PATH),
    }
}

/// Write the full skill (frontmatter included) to a repomap-owned file,
/// overwriting any prior copy so a re-install always lands the current text.
fn install_owned(root: &Path, rel_path: &str) -> Result<()> {
    let dest = root.join(rel_path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating skill directory {}", parent.display()))?;
    }
    fs::write(&dest, SKILL_MD)
        .with_context(|| format!("writing skill to {}", dest.display()))?;
    println!("Installed repomap skill -> {}", dest.display());
    Ok(())
}

/// Splice repomap's guide (frontmatter stripped) into a shared instructions
/// file as a marked block, creating the file if absent, replacing our prior
/// block if present, and appending after any other content otherwise.
fn install_shared(root: &Path, rel_path: &str) -> Result<()> {
    let dest = root.join(rel_path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }

    let existing = fs::read_to_string(&dest).unwrap_or_default();
    let block = format!("{BEGIN_MARKER}\n{}\n{END_MARKER}", skill_body().trim());
    let updated = merge_block(&existing, &block);
    fs::write(&dest, &updated)
        .with_context(|| format!("writing skill to {}", dest.display()))?;

    let verb = if existing.trim().is_empty() { "Installed" } else { "Updated" };
    println!("{verb} repomap skill -> {}", dest.display());
    Ok(())
}

/// The skill markdown with its leading YAML frontmatter removed. Claude's skill
/// format needs the `name`/`description` block; Copilot and Codex read the file
/// as plain instructions, so the frontmatter would just be literal noise there.
fn skill_body() -> &'static str {
    if let Some(rest) = SKILL_MD.strip_prefix("---\n") {
        if let Some(idx) = rest.find("\n---\n") {
            return rest[idx + "\n---\n".len()..].trim_start_matches('\n');
        }
    }
    SKILL_MD
}

/// Insert `block` into `existing`:
/// - if `existing` already has a `BEGIN..END` region, replace exactly that span
///   (keeping any surrounding user content);
/// - if `existing` is empty, the block becomes the whole file;
/// - otherwise append the block after a blank-line separator.
///
/// The result always ends with a single trailing newline.
fn merge_block(existing: &str, block: &str) -> String {
    if let (Some(start), Some(end_start)) =
        (existing.find(BEGIN_MARKER), existing.find(END_MARKER))
    {
        if end_start >= start {
            let end = end_start + END_MARKER.len();
            let merged = format!("{}{}{}", &existing[..start], block, &existing[end..]);
            return ensure_trailing_newline(&merged);
        }
    }

    if existing.trim().is_empty() {
        return format!("{block}\n");
    }

    let sep = if existing.ends_with('\n') { "\n" } else { "\n\n" };
    format!("{existing}{sep}{block}\n")
}

fn ensure_trailing_newline(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_skill_has_frontmatter() {
        // The bundled skill must be a valid Claude skill: name + description.
        assert!(SKILL_MD.starts_with("---"), "skill needs YAML frontmatter");
        assert!(SKILL_MD.contains("name: repomap"));
        assert!(SKILL_MD.contains("description:"));
    }

    #[test]
    fn skill_body_strips_frontmatter() {
        let body = skill_body();
        assert!(!body.starts_with("---"), "frontmatter should be stripped");
        assert!(!body.contains("description:"), "frontmatter fields should be gone");
        assert!(body.starts_with("# repomap"), "body should start at the heading");
    }

    #[test]
    fn claude_install_writes_full_skill_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        install_skill(root, Agent::Claude).unwrap();
        let dest = root.join(CLAUDE_REL_PATH);
        assert_eq!(fs::read_to_string(&dest).unwrap(), SKILL_MD);

        // Re-installing overwrites cleanly rather than erroring.
        install_skill(root, Agent::Claude).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), SKILL_MD);
    }

    #[test]
    fn copilot_install_writes_marked_block_without_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        install_skill(root, Agent::Copilot).unwrap();
        let dest = root.join(COPILOT_REL_PATH);
        let text = fs::read_to_string(&dest).unwrap();

        assert!(text.contains(BEGIN_MARKER) && text.contains(END_MARKER));
        assert!(text.contains("# repomap"));
        assert!(!text.contains("description:"), "frontmatter must not leak in");
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn codex_install_targets_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        install_skill(root, Agent::Codex).unwrap();
        assert!(root.join(CODEX_REL_PATH).exists());
    }

    #[test]
    fn shared_install_preserves_user_content_and_replaces_our_block() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let dest = root.join(CODEX_REL_PATH);

        // Pre-existing, user-authored instructions.
        fs::write(&dest, "# My project\n\nAlways run tests.\n").unwrap();

        install_skill(root, Agent::Codex).unwrap();
        let after_first = fs::read_to_string(&dest).unwrap();
        assert!(after_first.contains("Always run tests."), "user content kept");
        assert!(after_first.contains(BEGIN_MARKER));
        assert_eq!(after_first.matches(BEGIN_MARKER).count(), 1);

        // Re-installing replaces our block in place — no duplication, user text intact.
        install_skill(root, Agent::Codex).unwrap();
        let after_second = fs::read_to_string(&dest).unwrap();
        assert_eq!(after_second, after_first, "re-install is idempotent");
        assert_eq!(after_second.matches(BEGIN_MARKER).count(), 1);
        assert!(after_second.contains("Always run tests."));
    }

    #[test]
    fn merge_block_appends_when_no_markers() {
        let out = merge_block("existing content", "BLOCK");
        assert_eq!(out, "existing content\n\nBLOCK\n");
    }

    #[test]
    fn merge_block_creates_file_when_empty() {
        assert_eq!(merge_block("", "BLOCK"), "BLOCK\n");
        assert_eq!(merge_block("   \n", "BLOCK"), "BLOCK\n");
    }

    #[test]
    fn merge_block_replaces_existing_region_in_place() {
        let existing = format!("head\n\n{BEGIN_MARKER}\nold\n{END_MARKER}\n\ntail\n");
        let block = format!("{BEGIN_MARKER}\nnew\n{END_MARKER}");
        let out = merge_block(&existing, &block);
        assert_eq!(out, format!("head\n\n{BEGIN_MARKER}\nnew\n{END_MARKER}\n\ntail\n"));
    }
}
