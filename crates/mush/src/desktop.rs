//! Local Desktop registry for Mush.
//!
//! A Desktop is the durable top-level namespace for a body of work. This module
//! owns only the local registry file and the VFS root it points at. It does not
//! store project history or thread state; those live in the VFS and records.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub struct Desktop {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub theme: String,
    pub projects: Vec<String>,
}

pub fn registry_path() -> PathBuf {
    crate::config::home_dir().join(".config/fungos/desktops")
}

pub fn load(id: &str) -> Result<Desktop> {
    load_at(&registry_path(), id)
}

pub fn load_at(base: &Path, id: &str) -> Result<Desktop> {
    validate_id(id)?;
    let path = base.join(format!("{id}.toml"));
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read Desktop config {}", path.display()))?;
    parse(id, &text)
}

pub fn list() -> Result<Vec<Desktop>> {
    list_at(&registry_path())
}

pub fn list_at(base: &Path) -> Result<Vec<Desktop>> {
    if !base.is_dir() {
        return Ok(Vec::new());
    }
    let mut desktops = Vec::new();
    let mut read = std::fs::read_dir(base)
        .with_context(|| format!("read Desktop registry {}", base.display()))?;
    while let Some(entry) = read.next() {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        desktops.push(load_at(base, stem)?);
    }
    desktops.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(desktops)
}

pub fn create(id: &str, name: &str, theme: &str) -> Result<Desktop> {
    create_at(&registry_path(), id, name, theme)
}

pub fn create_at(base: &Path, id: &str, name: &str, theme: &str) -> Result<Desktop> {
    validate_id(id)?;
    let name = if name.trim().is_empty() {
        id
    } else {
        name.trim()
    };
    let theme = if theme.trim().is_empty() {
        "deep"
    } else {
        theme.trim()
    };
    let root = crate::config::home_dir()
        .join(".local/share/fungos/desktops")
        .join(id);
    let desktop = Desktop {
        id: id.to_string(),
        name: name.to_string(),
        root: root.clone(),
        theme: theme.to_string(),
        projects: Vec::new(),
    };
    let path = base.join(format!("{id}.toml"));
    if path.exists() {
        bail!("Desktop {id} already exists");
    }
    std::fs::create_dir_all(base).with_context(|| format!("create {}", base.display()))?;
    std::fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
    std::fs::create_dir_all(root.join("projects"))
        .with_context(|| format!("create {}", root.join("projects").display()))?;
    let mut text = String::new();
    text.push_str(&format!("id = \"{}\"\n", escape(&desktop.id)));
    text.push_str(&format!("name = \"{}\"\n", escape(&desktop.name)));
    text.push_str(&format!(
        "root = \"{}\"\n",
        escape(&desktop.root.display().to_string())
    ));
    text.push_str(&format!("theme = \"{}\"\n", escape(&desktop.theme)));
    text.push_str("projects = \"\"\n");
    std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
    Ok(desktop)
}

fn parse(id: &str, text: &str) -> Result<Desktop> {
    let mut name = id.to_string();
    let mut root = crate::config::home_dir()
        .join(".local/share/fungos/desktops")
        .join(id);
    let mut theme = "deep".to_string();
    let mut projects = Vec::new();
    for (line_number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!(
                "Desktop {id} line {}: expected key = value",
                line_number + 1
            );
        };
        let value = unquote(value.trim());
        match key.trim() {
            "id" => {}
            "name" => name = value,
            "root" => root = crate::config::expand_home(&value),
            "theme" => theme = value,
            "projects" => {
                projects = value
                    .split(',')
                    .map(str::trim)
                    .filter(|project| !project.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            other => bail!(
                "Desktop {id} line {}: unknown key {other:?}",
                line_number + 1
            ),
        }
    }
    Ok(Desktop {
        id: id.to_string(),
        name,
        root,
        theme,
        projects,
    })
}

pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || id == "."
        || id == ".."
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("invalid Desktop id {id:?}: use 1-64 letters, digits, '-', '_' or '.'");
    }
    Ok(())
}

fn unquote(value: &str) -> String {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_desktop_config() {
        let desktop = parse(
            "acme",
            r#"
            name = "Acme"
            root = "~/fungos/acme"
            theme = "ocean"
            projects = "soil, marbles"
            "#,
        )
        .unwrap();
        assert_eq!(desktop.name, "Acme");
        assert!(desktop.root.ends_with("fungos/acme"));
        assert_eq!(desktop.theme, "ocean");
        assert_eq!(desktop.projects, vec!["soil", "marbles"]);
    }

    #[test]
    fn rejects_path_traversal_ids() {
        assert!(validate_id("../escape").is_err());
        assert!(validate_id("acme-1").is_ok());
    }
}
