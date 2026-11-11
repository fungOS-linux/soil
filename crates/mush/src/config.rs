//! Small, dependency-free config reader for `mush`.
//!
//! The file is intentionally a flat `key = "value"` format. Missing keys use
//! the existing environment/default fallbacks. CLI flags still win.

use std::{collections::BTreeMap, path::{Path, PathBuf}};

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
    /// Groups the principal belongs to. `MUSH_GROUPS` adds to these.
    pub groups: Vec<String>,
    /// Origin allowlist per group, unioned for the principal's groups.
    pub group_origins: BTreeMap<String, Vec<String>>,
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
            "groups" => config.groups = split_list(&value),
            other if other.starts_with("group.") => {
                // group.<name>.fetch = "https://a,http://127.0.0.1:8080"
                let Some((name, capability)) = other["group.".len()..].split_once('.') else {
                    anyhow::bail!(
                        "mush config line {}: expected group.<name>.<capability>",
                        line_number + 1
                    );
                };
                if capability != "fetch" {
                    anyhow::bail!(
                        "mush config line {}: {capability:?} is not policy-scoped yet (use fetch)",
                        line_number + 1
                    );
                }
                if name.is_empty() {
                    anyhow::bail!("mush config line {}: empty group name", line_number + 1);
                }
                config
                    .group_origins
                    .insert(name.to_string(), split_list(&value));
            }
            other => anyhow::bail!(
                "mush config line {}: unknown key {other:?}",
                line_number + 1
            ),
        }
    }
    Ok(config)
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
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
    fn parses_groups_and_their_origins() {
        let config = parse(
            r#"
            groups = agents, build-fleet
            group.agents.fetch = "https://api.github.com, http://127.0.0.1:8080"
            group.build-fleet.fetch = https://internal.example
            "#,
        )
        .unwrap();
        assert_eq!(config.groups, vec!["agents", "build-fleet"]);
        assert_eq!(
            config.group_origins["agents"],
            vec!["https://api.github.com", "http://127.0.0.1:8080"]
        );
        assert_eq!(
            config.group_origins["build-fleet"],
            vec!["https://internal.example"]
        );
    }

    #[test]
    fn refuses_a_capability_that_is_not_policy_scoped() {
        let error = parse("group.agents.exec = cargo").unwrap_err();
        assert!(error.to_string().contains("not policy-scoped"), "{error}");
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(parse("typo = true").is_err());
    }
}
