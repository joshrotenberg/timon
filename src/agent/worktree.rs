//! `worktree = true`: a run in its own git worktree on branch `timon/<run id>`, under the state
//! directory. Removed afterwards when the run left it unchanged; kept otherwise.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value as Json, json};

use crate::dirs::{Root, write_error};
use crate::error::Error;

/// A worktree made for one run.
#[derive(Debug, Clone)]
pub struct Worktree {
    pub repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    /// The directory the run was asked to work in; the worktree stands in for it.
    pub from: PathBuf,
    base: String,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, Error> {
    let out = git_bytes(dir, args)?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

/// What git printed, byte for byte.
fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>, Error> {
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
    Ok(out.stdout)
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
            .map_err(|e| write_error(Root::State, state, "create", &parent, e))?;
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
            from: std::path::absolute(cwd).unwrap_or_else(|_| cwd.to_path_buf()),
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
            "from": self.from,
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
/// `git status`, the diff against HEAD, and the contents of untracked files. None when any of
/// it cannot be read, so a resume does not reuse what it could not check.
pub fn fingerprint(path: &Path) -> Option<(String, String)> {
    use sha2::{Digest, Sha256};
    let head = git(path, &["rev-parse", "HEAD"]).ok()?;
    // NUL-separated, so names come as they are rather than quoted.
    let status = git_bytes(path, &["status", "--porcelain=v1", "-z", "-uall"]).ok()?;
    // No external diff or textconv: the hash depends on the content alone, not on git config.
    let diff = git_bytes(
        path,
        &["diff", "HEAD", "--binary", "--no-ext-diff", "--no-textconv"],
    )
    .ok()?;
    let mut hash = Sha256::new();
    hash.update(&status);
    hash.update(&diff);
    for untracked in status
        .split(|&b| b == 0)
        .filter_map(|e| e.strip_prefix(b"?? "))
    {
        let bytes = std::fs::read(path.join(raw_path(untracked)?)).ok()?;
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    Some((head, hex::encode(hash.finalize())))
}

/// A path as git printed it.
#[cfg(unix)]
fn raw_path(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(not(unix))]
fn raw_path(bytes: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(bytes).ok().map(PathBuf::from)
}
