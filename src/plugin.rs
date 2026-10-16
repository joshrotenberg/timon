//! `timon plugin build`: writes the plugin's skills from the ones built into the binary, so the
//! plugin and `timon skill show` never disagree. Manifests and the shim are checked-in files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::skills;

/// The files `skills/` in a plugin directory should hold, relative to it, with their text.
pub fn expected() -> Result<BTreeMap<PathBuf, String>, Error> {
    let mut files = BTreeMap::new();
    for skill in skills::builtin() {
        let dir = PathBuf::from("skills").join(&skill.name);
        files.insert(dir.join("SKILL.md"), skill.text()?);
        for reference in skill.references() {
            files.insert(
                dir.join("references").join(format!("{reference}.md")),
                skill.reference(&reference)?,
            );
        }
    }
    Ok(files)
}

/// What differs between `dir/skills` and what the binary would write.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drift {
    pub missing: Vec<PathBuf>,
    pub changed: Vec<PathBuf>,
    pub extra: Vec<PathBuf>,
}

impl Drift {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.changed.is_empty() && self.extra.is_empty()
    }
}

fn existing(dir: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(at) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(relative) = path.strip_prefix(root) {
                out.push(relative.to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &dir.join("skills"), &mut out);
    out.sort();
    out
}

/// Compares `dir/skills` with what the binary would write.
pub fn check(dir: &Path) -> Result<Drift, Error> {
    let expected = expected()?;
    let mut drift = Drift::default();
    for (path, text) in &expected {
        match std::fs::read_to_string(dir.join(path)) {
            Ok(current) if current == *text => {}
            Ok(_) => drift.changed.push(path.clone()),
            Err(_) => drift.missing.push(path.clone()),
        }
    }
    drift.extra = existing(dir)
        .into_iter()
        .filter(|p| !expected.contains_key(p))
        .collect();
    Ok(drift)
}

/// Writes `dir/skills` from the binary and removes files it would not write. Returns the
/// number of files written.
pub fn build(dir: &Path) -> Result<usize, Error> {
    let io = |what: &str, path: &Path, e: std::io::Error| {
        Error::new("io", format!("cannot {what} {}: {e}", path.display()))
    };
    let expected = expected()?;
    for extra in check(dir)?.extra {
        let path = dir.join(&extra);
        std::fs::remove_file(&path).map_err(|e| io("remove", &path, e))?;
    }
    for (relative, text) in &expected {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io("create", parent, e))?;
        }
        std::fs::write(&path, text).map_err(|e| io("write", &path, e))?;
    }
    remove_empty_dirs(&dir.join("skills"));
    Ok(expected.len())
}

fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                remove_empty_dirs(&entry.path());
                let _ = std::fs::remove_dir(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_then_check_is_clean_and_stale_files_go() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let stale = dir.join("skills/old-skill/SKILL.md");
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, "old").unwrap();

        let before = check(dir).unwrap();
        assert!(
            before
                .missing
                .contains(&PathBuf::from("skills/timon/SKILL.md"))
        );
        assert_eq!(before.extra, [PathBuf::from("skills/old-skill/SKILL.md")]);

        build(dir).unwrap();
        assert!(check(dir).unwrap().is_empty());
        assert!(!dir.join("skills/old-skill").exists());
        assert!(dir.join("skills/timon/references/api.md").is_file());

        std::fs::write(dir.join("skills/timon/SKILL.md"), "edited").unwrap();
        assert_eq!(
            check(dir).unwrap().changed,
            [PathBuf::from("skills/timon/SKILL.md")]
        );
    }
}
