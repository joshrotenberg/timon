//! The skill library: skills built into the binary, plus `skills/` in the user and project
//! layers. A skill is a directory with `SKILL.md` and optional `references/NAME.md`.

use std::path::PathBuf;

use include_dir::{Dir, include_dir};

use crate::error::Error;
use crate::layers::{Kind, Layers};
use crate::suggest;

static BUILTIN: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/skills");

/// References generated from the module declarations rather than stored as files.
const GENERATED: &[(&str, &str)] = &[("timon", "api")];

/// One skill in reach.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub layer: Kind,
    pub description: String,
    source: Source,
}

#[derive(Debug, Clone)]
enum Source {
    Builtin(&'static Dir<'static>),
    Disk(PathBuf),
}

impl Skill {
    /// The text of `SKILL.md`.
    pub fn text(&self) -> Result<String, Error> {
        self.file("SKILL.md")
            .ok_or_else(|| Error::new("not_found", format!("{} has no SKILL.md", self.name)))
    }

    /// The names of the skill's references, sorted.
    pub fn references(&self) -> Vec<String> {
        let mut names: Vec<String> = match &self.source {
            Source::Builtin(dir) => dir
                .get_dir(dir.path().join("references"))
                .map(|refs| {
                    refs.files()
                        .filter_map(|f| f.path().file_stem()?.to_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            Source::Disk(root) => std::fs::read_dir(root.join("references"))
                .map(|entries| {
                    entries
                        .flatten()
                        .filter_map(|e| {
                            let p = e.path();
                            (p.extension()? == "md")
                                .then(|| p.file_stem()?.to_str().map(str::to_owned))?
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        if self.layer == Kind::Builtin {
            for (skill, reference) in GENERATED {
                if *skill == self.name {
                    names.push((*reference).to_owned());
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// The text of one reference.
    pub fn reference(&self, name: &str) -> Result<String, Error> {
        if self.layer == Kind::Builtin
            && GENERATED.iter().any(|(s, r)| *s == self.name && *r == name)
        {
            return Ok(crate::docs::reference());
        }
        if let Some(text) = self.file(&format!("references/{name}.md")) {
            return Ok(text);
        }
        let names = self.references();
        let err = Error::new(
            "not_found",
            format!("skill {} has no reference \"{name}\"", self.name),
        );
        Err(
            match suggest::closest(name, names.iter().map(String::as_str)) {
                Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                None if names.is_empty() => err.with_hint("it has no references"),
                None => err.with_hint(format!("its references are {}", names.join(", "))),
            },
        )
    }

    fn file(&self, relative: &str) -> Option<String> {
        match &self.source {
            Source::Builtin(dir) => dir
                .get_file(dir.path().join(relative))
                .and_then(|f| f.contents_utf8())
                .map(str::to_owned),
            Source::Disk(root) => std::fs::read_to_string(root.join(relative)).ok(),
        }
    }
}

/// The `description` from a skill's frontmatter.
fn description(text: &str) -> String {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    lines
        .take_while(|l| l.trim() != "---")
        .find_map(|l| l.strip_prefix("description:"))
        .map(|d| d.trim().trim_matches('"').to_owned())
        .unwrap_or_default()
}

/// The skills built into the binary.
pub fn builtin() -> Vec<Skill> {
    BUILTIN
        .dirs()
        .filter_map(|dir| {
            let name = dir.path().file_name()?.to_str()?.to_owned();
            let text = dir.get_file(dir.path().join("SKILL.md"))?.contents_utf8()?;
            Some(Skill {
                name,
                layer: Kind::Builtin,
                description: description(text),
                source: Source::Builtin(dir),
            })
        })
        .collect()
}

/// Every skill in reach: project, then user, then built in. A name a higher layer has hides
/// the lower ones.
pub fn list(layers: &Layers) -> Vec<Skill> {
    let mut found: Vec<Skill> = Vec::new();
    for layer in layers.active() {
        let Ok(entries) = std::fs::read_dir(layer.root.join("skills")) else {
            continue;
        };
        for entry in entries.flatten() {
            let root = entry.path();
            let Ok(text) = std::fs::read_to_string(root.join("SKILL.md")) else {
                continue;
            };
            let Some(name) = root.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if found.iter().any(|s| s.name == name) {
                continue;
            }
            found.push(Skill {
                name: name.to_owned(),
                layer: layer.kind,
                description: description(&text),
                source: Source::Disk(root),
            });
        }
    }
    for skill in builtin() {
        if !found.iter().any(|s| s.name == skill.name) {
            found.push(skill);
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Finds a skill by name.
pub fn find(layers: &Layers, name: &str) -> Result<Skill, Error> {
    let skills = list(layers);
    if let Some(skill) = skills.iter().find(|s| s.name == name) {
        return Ok(skill.clone());
    }
    let err = Error::new("not_found", format!("no skill named \"{name}\""));
    Err(
        match suggest::closest(name, skills.iter().map(|s| s.name.as_str())) {
            Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
            None => err.with_hint("timon skill list shows them"),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_skills_have_descriptions() {
        let skills = builtin();
        assert!(skills.iter().any(|s| s.name == "timon"));
        for skill in &skills {
            assert!(
                !skill.description.is_empty(),
                "{} has no description",
                skill.name
            );
            assert!(
                skill
                    .text()
                    .unwrap()
                    .contains(&format!("name: {}", skill.name))
            );
        }
    }

    #[test]
    fn the_api_reference_is_generated() {
        let timon = builtin().into_iter().find(|s| s.name == "timon").unwrap();
        assert_eq!(timon.references(), ["api"]);
        let api = timon.reference("api").unwrap();
        assert!(api.contains("json.query(value, expression) -> value"));
        assert_eq!(
            timon.reference("apy").unwrap_err().hint.as_deref(),
            Some("did you mean \"api\"?")
        );
    }
}
