//! Where timon reads definitions and writes state. Resolved in one place; resolving a path
//! never touches the filesystem.

use std::path::{Path, PathBuf};

use crate::error::Error;

/// Directory holding a project's definitions, at the project root.
pub const PROJECT_DIR: &str = ".timon";

/// The three roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// The user layer: `$XDG_CONFIG_HOME/timon`, default `~/.config/timon`.
    pub config: PathBuf,
    /// What timon writes and keeps: `$XDG_STATE_HOME/timon`, default `~/.local/state/timon`.
    pub state: PathBuf,
    /// What timon writes and can rebuild: `$XDG_CACHE_HOME/timon`, default `~/.cache/timon`.
    pub cache: PathBuf,
    /// The home directory, where the search for a project layer stops.
    pub home: Option<PathBuf>,
}

impl Dirs {
    /// Resolves the roots from the environment. `TIMON_HOME` puts all three under one directory.
    pub fn from_env() -> Result<Self, Error> {
        Self::resolve(|name| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
    }

    /// Resolves the roots from `var`, which looks up an environment variable.
    pub fn resolve(var: impl Fn(&str) -> Option<PathBuf>) -> Result<Self, Error> {
        let home = var("HOME");
        if let Some(root) = var("TIMON_HOME") {
            return Ok(Self {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
                home,
            });
        }
        let base = |xdg: &str, default: &[&str]| -> Result<PathBuf, Error> {
            if let Some(dir) = var(xdg) {
                return Ok(dir.join("timon"));
            }
            let home = home.as_ref().ok_or_else(|| {
                Error::new("not_found", "no home directory is set")
                    .with_hint(format!("set HOME, {xdg} or TIMON_HOME"))
            })?;
            Ok(default
                .iter()
                .fold(home.clone(), |p, part| p.join(part))
                .join("timon"))
        };
        Ok(Self {
            config: base("XDG_CONFIG_HOME", &[".config"])?,
            state: base("XDG_STATE_HOME", &[".local", "state"])?,
            cache: base("XDG_CACHE_HOME", &[".cache"])?,
            home: home.clone(),
        })
    }

    /// The nearest ancestor of `start` (itself included) holding a `.timon` directory, stopping
    /// before the home directory and the filesystem root.
    pub fn project_root(&self, start: &Path) -> Option<PathBuf> {
        for dir in start.ancestors() {
            if Some(dir) == self.home.as_deref() || dir.parent().is_none() {
                return None;
            }
            if dir.join(PROJECT_DIR).is_dir() {
                return Some(dir.to_path_buf());
            }
        }
        None
    }

    /// The file listing trusted project roots, one per line.
    pub fn trust_file(&self) -> PathBuf {
        self.state.join("trusted")
    }
}

/// One of the directories timon writes to, for the hint on a write it is not allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    /// [`Dirs::state`].
    State,
    /// [`Dirs::cache`].
    Cache,
}

impl Root {
    fn holds(self) -> &'static str {
        match self {
            Root::State => "the trust store, run records, script runs and worktrees",
            Root::Cache => "the session index and summaries",
        }
    }

    fn variable(self) -> &'static str {
        match self {
            Root::State => "XDG_STATE_HOME",
            Root::Cache => "XDG_CACHE_HOME",
        }
    }
}

/// An `io` error for a failed `action` (`write`, `create`) on `path` under `root`, the
/// directory `dir`, with [`write_hint`].
pub fn write_error(root: Root, dir: &Path, action: &str, path: &Path, e: std::io::Error) -> Error {
    Error::new("io", format!("cannot {action} {}: {e}", path.display()))
        .with_hint_opt(write_hint(root, dir, &e))
}

/// For a denied write under `root` (EACCES, EPERM, or EROFS from a read-only mount), the
/// directory `dir`: what timon keeps there and how a sandboxed host can allow it.
pub fn write_hint(root: Root, dir: &Path, e: &std::io::Error) -> Option<String> {
    use std::io::ErrorKind::{PermissionDenied, ReadOnlyFilesystem};
    matches!(e.kind(), PermissionDenied | ReadOnlyFilesystem).then(|| {
        format!(
            "timon keeps {} in {}; a sandboxed host must allow writes there (for Codex, \
             sandbox_workspace_write.writable_roots), or set TIMON_HOME or {} to a writable \
             directory",
            root.holds(),
            dir.display(),
            root.variable(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<PathBuf> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| PathBuf::from(v))
        }
    }

    #[test]
    fn defaults_follow_xdg_under_home() {
        let dirs = Dirs::resolve(env(&[("HOME", "/h")])).unwrap();
        assert_eq!(dirs.config, PathBuf::from("/h/.config/timon"));
        assert_eq!(dirs.state, PathBuf::from("/h/.local/state/timon"));
        assert_eq!(dirs.cache, PathBuf::from("/h/.cache/timon"));
    }

    #[test]
    fn xdg_variables_win_and_timon_home_wins_over_everything() {
        let dirs = Dirs::resolve(env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/c")])).unwrap();
        assert_eq!(dirs.config, PathBuf::from("/c/timon"));
        let dirs = Dirs::resolve(env(&[("HOME", "/h"), ("TIMON_HOME", "/t")])).unwrap();
        assert_eq!(
            (dirs.config, dirs.state, dirs.cache),
            ("/t/config".into(), "/t/state".into(), "/t/cache".into())
        );
    }

    #[test]
    fn only_a_permission_error_names_the_directory_and_how_to_allow_it() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let err = write_error(
            Root::State,
            Path::new("/s"),
            "write",
            Path::new("/s/trusted"),
            denied,
        );
        assert_eq!(err.kind, "io");
        assert!(err.message.starts_with("cannot write /s/trusted: "));
        let hint = err.hint.unwrap();
        assert!(
            hint.contains("trust store") && hint.contains("in /s;"),
            "{hint}"
        );
        assert!(hint.contains("TIMON_HOME or XDG_STATE_HOME"), "{hint}");
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        let err = write_error(
            Root::Cache,
            Path::new("/c"),
            "create",
            Path::new("/c"),
            missing,
        );
        assert_eq!(err.hint, None);
    }

    #[test]
    fn the_project_layer_is_the_nearest_timon_directory_below_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let project = home.join("code").join("proj");
        let deep = project.join("src").join("deep");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(project.join(PROJECT_DIR)).unwrap();
        std::fs::create_dir_all(home.join(PROJECT_DIR)).unwrap();
        let dirs = Dirs::resolve(|name| (name == "HOME").then(|| home.clone())).unwrap();
        assert_eq!(dirs.project_root(&deep), Some(project.clone()));
        assert_eq!(dirs.project_root(&home.join("code")), None);
    }
}
