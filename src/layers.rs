//! Definition layers: the user layer (`~/.config/timon`) and the project layer (`.timon/`),
//! in the same format, over what is built into the binary.
//!
//! ```text
//! <layer>/
//! ├── init.lua        settings: defaults, limits, serve
//! ├── tasks.lua       tasks, or tasks.fnl
//! ├── scripts/        tasks too: timon NAME
//! ├── skills/<name>/  SKILL.md, references/
//! └── lib/            on the module path
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::dirs::{Dirs, PROJECT_DIR, Root, write_error};
use crate::error::Error;

/// Which layer a definition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Project,
    User,
    Builtin,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Project => "project",
            Kind::User => "user",
            Kind::Builtin => "builtin",
        }
    }
}

/// One directory layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub kind: Kind,
    /// The layer directory: `~/.config/timon` or `<project>/.timon`.
    pub root: PathBuf,
    /// Project layers load only after `timon trust`; the user layer is always trusted.
    pub trusted: bool,
}

/// The layers in reach of a working directory.
#[derive(Debug, Clone)]
pub struct Layers {
    pub dirs: Dirs,
    /// The project root (the directory holding `.timon`), when there is one.
    pub project_root: Option<PathBuf>,
    pub project: Option<Layer>,
    pub user: Layer,
}

/// A script found in a layer's `scripts/` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub name: String,
    pub path: PathBuf,
    pub layer: Kind,
    /// The script's leading comment, first line.
    pub description: String,
}

impl Layers {
    /// Finds the layers for `cwd`.
    pub fn discover(dirs: Dirs, cwd: &Path) -> Self {
        let project_root = dirs.project_root(cwd);
        let project = project_root.as_ref().map(|root| Layer {
            kind: Kind::Project,
            root: root.join(PROJECT_DIR),
            trusted: is_trusted(&dirs, root),
        });
        let user = Layer {
            kind: Kind::User,
            root: dirs.config.clone(),
            trusted: true,
        };
        Self {
            dirs,
            project_root,
            project,
            user,
        }
    }

    /// Trusted layers in lookup order: project, then user.
    pub fn active(&self) -> Vec<&Layer> {
        self.project
            .iter()
            .filter(|l| l.trusted)
            .chain(std::iter::once(&self.user))
            .collect()
    }

    /// Every script in reach, project first; a name in a lower layer that a higher one also
    /// has is left out.
    pub fn scripts(&self) -> Vec<Script> {
        let mut found: Vec<Script> = Vec::new();
        for layer in self.active() {
            for script in scripts_in(layer) {
                if !found.iter().any(|s| s.name == script.name) {
                    found.push(script);
                }
            }
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }

    /// The script `name` resolves to. A script only an untrusted project layer has is an error
    /// that says how to trust it.
    pub fn script(&self, name: &str) -> Result<Script, Error> {
        if let Some(script) = self.scripts().into_iter().find(|s| s.name == name) {
            return Ok(script);
        }
        if let (Some(project), Some(root)) = (&self.project, &self.project_root)
            && !project.trusted
            && scripts_in(project).iter().any(|s| s.name == name)
        {
            return Err(Error::new(
                "untrusted",
                format!(
                    "{name} is in {}, which is not trusted",
                    project.root.display()
                ),
            )
            .with_hint(format!(
                "review it, then run: timon trust {}",
                root.display()
            )));
        }
        let names: Vec<String> = self.scripts().into_iter().map(|s| s.name).collect();
        let err = Error::new("not_found", format!("no script named \"{name}\""));
        Err(
            match crate::suggest::closest(name, names.iter().map(String::as_str)) {
                Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                None => err.with_hint("timon scripts lists them"),
            },
        )
    }

    /// `lib/` directories of the trusted layers, in lookup order.
    pub fn lib_dirs(&self) -> Vec<PathBuf> {
        self.active()
            .into_iter()
            .map(|l| l.root.join("lib"))
            .filter(|p| p.is_dir())
            .collect()
    }

    /// `contracts/` directories of the trusted layers, in lookup order.
    pub fn contract_dirs(&self) -> Vec<PathBuf> {
        self.active()
            .into_iter()
            .map(|l| l.root.join("contracts"))
            .filter(|p| p.is_dir())
            .collect()
    }

    /// `init.lua` files of the trusted layers, lowest first, so later ones win when merged.
    pub fn init_files(&self) -> Vec<(Kind, PathBuf)> {
        let mut files: Vec<(Kind, PathBuf)> = self
            .active()
            .into_iter()
            .map(|l| (l.kind, l.root.join("init.lua")))
            .filter(|(_, p)| p.is_file())
            .collect();
        files.reverse();
        files
    }
}

/// The scripts in one layer's `scripts/` directory.
pub fn scripts_in(layer: &Layer) -> Vec<Script> {
    let Ok(entries) = std::fs::read_dir(layer.root.join("scripts")) else {
        return Vec::new();
    };
    let mut scripts = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if !matches!(ext, Some("lua" | "fnl")) || !path.is_file() {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        scripts.push(Script {
            name: name.to_owned(),
            description: description(&path),
            path: path.clone(),
            layer: layer.kind,
        });
    }
    scripts
}

/// The first line of a script's leading comment, skipping a shebang.
fn description(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let marker = if path.extension().is_some_and(|e| e == "fnl") {
        ";"
    } else {
        "--"
    };
    text.lines()
        .skip_while(|l| l.starts_with("#!"))
        .map(str::trim)
        .find(|l| !l.is_empty())
        .and_then(|l| l.strip_prefix(marker))
        .map(|l| {
            l.trim_start_matches(marker.chars().next().unwrap_or('-'))
                .trim()
                .to_owned()
        })
        .unwrap_or_default()
}

fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

fn trusted_roots(dirs: &Dirs) -> Vec<PathBuf> {
    std::fs::read_to_string(dirs.trust_file())
        .map(|text| {
            text.lines()
                .filter(|l| !l.trim().is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the project at `root` (the directory holding `.timon`) is trusted.
pub fn is_trusted(dirs: &Dirs, root: &Path) -> bool {
    trusted_roots(dirs).contains(&canonical(root))
}

/// Records `root` as trusted, or removes it with `revoke`. Returns the recorded path.
pub fn set_trust(dirs: &Dirs, root: &Path, revoke: bool) -> Result<PathBuf, Error> {
    let root = canonical(root);
    if !revoke && !root.join(PROJECT_DIR).is_dir() {
        return Err(Error::new(
            "not_found",
            format!("{} has no {PROJECT_DIR} directory", root.display()),
        ));
    }
    let mut roots = trusted_roots(dirs);
    roots.retain(|r| *r != root);
    if !revoke {
        roots.push(root.clone());
    }
    let file = dirs.trust_file();
    let io = |e| write_error(Root::State, &dirs.state, "write", &file, e);
    create_private_dir(&dirs.state).map_err(io)?;
    let mut out = String::new();
    for r in &roots {
        out.push_str(&r.display().to_string());
        out.push('\n');
    }
    let mut f = private_file(&file).map_err(io)?;
    f.write_all(out.as_bytes()).map_err(io)?;
    Ok(root)
}

/// Creates a directory, and its parents, readable only by the user.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        dirs: Dirs,
        project: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let dirs = Dirs::resolve(|name| match name {
            "TIMON_HOME" => Some(root.join("timon")),
            "HOME" => Some(root.join("home")),
            _ => None,
        })
        .unwrap();
        let project = root.join("home").join("proj");
        let write = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            dirs.config.join("scripts/shared.lua"),
            "-- From the user layer\nreturn 1",
        );
        write(
            dirs.config.join("scripts/both.lua"),
            "-- user version\nreturn 'user'",
        );
        write(
            project.join(".timon/scripts/both.lua"),
            "#!/usr/bin/env timon\n-- project version\nreturn 'project'",
        );
        write(
            project.join(".timon/scripts/local.fnl"),
            ";; A Fennel script\n42",
        );
        Fixture {
            _tmp: tmp,
            dirs,
            project,
        }
    }

    #[test]
    fn an_untrusted_project_layer_is_left_out() {
        let f = fixture();
        let layers = Layers::discover(f.dirs.clone(), &f.project);
        assert!(!layers.project.as_ref().unwrap().trusted);
        let names: Vec<String> = layers.scripts().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["both", "shared"]);
        assert_eq!(layers.script("both").unwrap().layer, Kind::User);
        let err = layers.script("local").unwrap_err();
        assert_eq!(err.kind, "untrusted");
        assert!(err.hint.unwrap().contains("timon trust"));
    }

    #[test]
    fn a_trusted_project_layer_shadows_the_user_layer() {
        let f = fixture();
        set_trust(&f.dirs, &f.project, false).unwrap();
        let layers = Layers::discover(f.dirs.clone(), &f.project.join(".timon"));
        let scripts = layers.scripts();
        let both = scripts.iter().find(|s| s.name == "both").unwrap();
        assert_eq!(
            (both.layer, both.description.as_str()),
            (Kind::Project, "project version")
        );
        let local = scripts.iter().find(|s| s.name == "local").unwrap();
        assert_eq!(local.description, "A Fennel script");
        assert!(f.dirs.trust_file().starts_with(&f.dirs.state));

        set_trust(&f.dirs, &f.project, true).unwrap();
        let layers = Layers::discover(f.dirs.clone(), &f.project);
        assert!(!layers.project.unwrap().trusted);
    }

    #[test]
    fn unknown_scripts_suggest_a_name() {
        let f = fixture();
        let layers = Layers::discover(f.dirs.clone(), &f.project);
        assert_eq!(
            layers.script("shard").unwrap_err().hint.as_deref(),
            Some("did you mean \"shared\"?")
        );
    }
}
