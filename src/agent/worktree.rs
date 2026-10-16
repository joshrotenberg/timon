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

    /// What the record says about the worktree. A kept one also records its head commit and
    /// a hash of its uncommitted state, so a resume can tell whether it still holds what the
    /// run left.
    pub fn to_json(&self, kept: bool) -> Json {
        let mut out = json!({
            "path": self.path,
            "branch": self.branch,
            "kept": kept,
        });
        if kept && let Some((head, state)) = fingerprint(&self.path) {
            out["head"] = json!(head);
            out["state"] = json!(state);
        }
        out
    }
}

/// The head commit of the worktree at `path`, and a sha256 of its uncommitted changes:
/// `git status`, the diff against HEAD, and the contents of untracked files.
pub fn fingerprint(path: &Path) -> Option<(String, String)> {
    use sha2::{Digest, Sha256};
    let head = git(path, &["rev-parse", "HEAD"]).ok()?;
    let status = git(path, &["status", "--porcelain=v1", "-uall"]).ok()?;
    let diff = git(path, &["diff", "HEAD", "--binary"]).ok()?;
    let mut hash = Sha256::new();
    hash.update(status.as_bytes());
    hash.update(diff.as_bytes());
    for untracked in status.lines().filter_map(|l| l.strip_prefix("?? ")) {
        if let Ok(bytes) = std::fs::read(path.join(untracked)) {
            hash.update(&bytes);
        }
    }
    Some((head, hex::encode(hash.finalize())))
}
