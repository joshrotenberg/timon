//! `worktree = true`: a run in its own git worktree on branch `timon/<run id>`, under the state
//! directory. Removed afterwards when the run left it unchanged; kept otherwise.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value as Json, json};

use crate::error::Error;

/// A worktree made for one run.
#[derive(Debug, Clone)]
pub struct Worktree {
    pub repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    base: String,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| {
            Error::new("not_installed", format!("cannot run git: {e}"))
                .with_hint("worktree = true needs git on PATH")
        })?;
    if !out.status.success() {
        return Err(Error::new(
            "git_failed",
            format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

impl Worktree {
    /// Creates a worktree of the repository containing `cwd`, at its current commit.
    pub fn create(cwd: &Path, state: &Path, id: &str) -> Result<Self, Error> {
        let repo = git(cwd, &["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
            .map_err(|_| {
                Error::new(
                    "bad_option",
                    format!(
                        "worktree = true needs a git repository, and {} is not in one",
                        cwd.display()
                    ),
                )
            })?;
        let base = git(&repo, &["rev-parse", "HEAD"])?;
        let parent = state.join("worktrees");
        crate::layers::create_private_dir(&parent)
            .map_err(|e| Error::new("io", format!("cannot create {}: {e}", parent.display())))?;
        let path = parent.join(id);
        let branch = format!("timon/{id}");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                &path.display().to_string(),
                &base,
            ],
        )?;
        Ok(Self {
            repo,
            path,
            branch,
            base,
        })
    }

    /// Removes the worktree and its branch if the run changed nothing; returns whether it was
    /// kept.
    pub fn finish(&self) -> Result<bool, Error> {
        let dirty = !git(&self.path, &["status", "--porcelain"])?.is_empty();
        let moved = git(&self.path, &["rev-parse", "HEAD"])? != self.base;
        if dirty || moved {
            return Ok(true);
        }
        git(
            &self.repo,
            &[
                "worktree",
                "remove",
                "--force",
                &self.path.display().to_string(),
            ],
        )?;
        git(&self.repo, &["branch", "-D", &self.branch])?;
        Ok(false)
    }

    /// What the record says about the worktree.
    pub fn to_json(&self, kept: bool) -> Json {
        json!({
            "path": self.path,
            "branch": self.branch,
            "kept": kept,
        })
    }
}
