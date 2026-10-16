//! Files and directories private to the user. Copied from bottega-core's `src/private.rs`
//! (bottega d89c451), with trovador's `make_private` from `src/paths.rs`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Creates a directory of trovador's state private to the user (0700), and tightens one
/// that already exists. The index, summaries, and saved replies hold transcript text,
/// secrets included, copied from files Claude writes 0600; trovador made them 0644 in a
/// 0755 directory (pre-release review).
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        builder.mode(0o700);
        builder.create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    builder.create(dir)
}

/// Options that create a file readable only by the user (0600).
pub fn private_file() -> std::fs::OpenOptions {
    #[allow(unused_mut)]
    let mut opts = std::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts
}

/// Writes a file readable only by the user, replacing it atomically through a temp file.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    {
        let mut f = private_file()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)?;
    #[cfg(unix)]
    std::fs::File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?
    .sync_all()?;
    Ok(())
}

/// Makes an existing file readable only by the user (0600); a missing file is fine.
pub fn make_private(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
