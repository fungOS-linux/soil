//! Small, dependency-free config reader for `mush`.
//!
//! The file is intentionally a flat `key = "value"` format. Missing keys use
//! the existing environment/default fallbacks. CLI flags still win.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Default)]
pub struct Config {
    pub root: Option<PathBuf>,
    pub command_root: Option<PathBuf>,
    pub records_root: Option<PathBuf>,
    pub records_book: Option<String>,
    pub records_collection: Option<String>,
    pub principal: Option<String>,
    pub socket: Option<PathBuf>,
    pub desktop: Option<String>,
    /// Host command whose stdout becomes the prompt, for themed prompts.
    pub prompt_command: Option<String>,
}

pub fn default_path() -> PathBuf {
    std::env::var_os("MUSH_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config/fungos/mush/config.toml"))
}

pub fn load(path: &Path) -> Result<Config> {
    if !path.is_file() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read Mush config {}", path.display()))?;
    parse(&text)
}

fn parse(text: &str) -> Result<Config> {
    let mut config = Config::default();
    for (line_number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            anyhow::bail!("mush config line {}: expected key = value", line_number + 1);
        };
        let value = unquote(value.trim());
        match key.trim() {
            "root" => config.root = Some(expand_home(&value)),
            "command_root" => config.command_root = Some(expand_home(&value)),
            "records_root" => config.records_root = Some(expand_home(&value)),
            "records_book" => config.records_book = Some(value),
            "records_collection" => config.records_collection = Some(value),
            "principal" => config.principal = Some(value),
            "socket" => config.socket = Some(expand_home(&value)),
            "desktop" => config.desktop = Some(value),
            "prompt_command" => config.prompt_command = Some(value),
            other => anyhow::bail!(
                "mush config line {}: unknown key {other:?}",
                line_number + 1
            ),
        }
    }
    Ok(config)
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

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return home_dir();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_config() {
        let config = parse(
            r#"
            root = "~/fungos-node"
            command_root = ~/.local/share/fungos/commands
            records_book = personal/avery/marbles
            principal = mycelium:agent:ajmwagar/codex-1
            "#,
        )
        .unwrap();
        assert!(config.root.unwrap().ends_with("fungos-node"));
        assert_eq!(
            config.records_book.as_deref(),
            Some("personal/avery/marbles")
        );
        assert_eq!(
            config.principal.as_deref(),
            Some("mycelium:agent:ajmwagar/codex-1")
        );
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(parse("typo = true").is_err());
    }
}
