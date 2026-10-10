//! The Claude skill for slog, embedded so it always matches the installed binary.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const TEMPLATE: &str = include_str!("../skill/SKILL.md");

pub fn contents() -> String {
    TEMPLATE.replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
}

/// `~/.claude/skills/slog/SKILL.md`
pub fn user_path() -> Result<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    // An empty or relative HOME would silently install relative to the current directory.
    if !home.is_absolute() {
        bail!("HOME is not an absolute path: {:?}", home);
    }
    Ok(home.join(".claude/skills/slog/SKILL.md"))
}

/// `./.claude/skills/slog/SKILL.md`, relative to the current directory.
pub fn project_path() -> PathBuf {
    PathBuf::from(".claude/skills/slog/SKILL.md")
}

pub enum Installed {
    Created,
    Updated,
    Unchanged,
}

/// Writes the skill to `path`. An existing file with different contents is only replaced
/// with `force`, since it may hold the user's own edits.
pub fn install(path: &Path, force: bool) -> Result<Installed> {
    let new = contents();
    let existing = match fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let result = match existing {
        Some(s) if s == new => return Ok(Installed::Unchanged),
        Some(_) if !force => bail!(
            "{} exists with different contents (an older slog version or local edits); \
             re-run with --force to overwrite it",
            path.display()
        ),
        Some(_) => Installed::Updated,
        None => Installed::Created,
    };
    let dir = path.parent().expect("skill path has a parent");
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    fs::write(path, new).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory removed when dropped, including when a test fails.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir =
                std::env::temp_dir().join(format!("slog-skill-test-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            TempDir(dir)
        }

        fn skill_path(&self) -> PathBuf {
            self.0.join(".claude/skills/slog/SKILL.md")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn contents_have_frontmatter_and_version() {
        let s = contents();
        assert!(s.starts_with("---\nname: slog\n"));
        assert!(s.contains(&format!("version: {}\n", env!("CARGO_PKG_VERSION"))));
        assert!(!s.contains("{{"));
    }

    #[test]
    fn install_creates_then_is_unchanged() {
        let dir = TempDir::new("create");
        let path = dir.skill_path();
        assert!(matches!(install(&path, false).unwrap(), Installed::Created));
        assert_eq!(fs::read_to_string(&path).unwrap(), contents());
        assert!(matches!(
            install(&path, false).unwrap(),
            Installed::Unchanged
        ));
    }

    #[test]
    fn install_refuses_to_overwrite_edits_without_force() {
        let dir = TempDir::new("force");
        let path = dir.skill_path();
        install(&path, false).unwrap();
        fs::write(&path, "my edits").unwrap();
        assert!(install(&path, false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "my edits");
        assert!(matches!(install(&path, true).unwrap(), Installed::Updated));
        assert_eq!(fs::read_to_string(&path).unwrap(), contents());
    }
}
