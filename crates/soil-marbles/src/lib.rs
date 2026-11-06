//! Loose Marbles adapter.
//!
//! Soil calls the stable `mb ... --json` CLI contract. There is no compile-time
//! Marbles dependency, daemon socket, or copied task database. The host owns
//! project scope, endpoint, token file and actor.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_OUTPUT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub issue_type: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct MarblesClient {
    binary: PathBuf,
    project: Option<String>,
    url: Option<String>,
    token_file: Option<PathBuf>,
    actor: Option<String>,
}

impl MarblesClient {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            project: None,
            url: None,
            token_file: None,
            actor: None,
        }
    }

    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        self.project = Some(project.into());
        self
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn with_token_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.token_file = Some(path.into());
        self
    }

    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    pub fn list(&self) -> Result<Vec<Issue>> {
        self.run_json(&["list"])
    }

    pub fn ready(&self) -> Result<Vec<Issue>> {
        self.run_json(&["ready"])
    }

    pub fn show(&self, id: &str) -> Result<Issue> {
        let mut issues: Vec<Issue> = self.run_json(&["show", id])?;
        issues
            .pop()
            .with_context(|| format!("Marbles issue {id} not found"))
    }

    pub fn create(&self, title: &str, description: &str) -> Result<String> {
        let mut args = vec!["create".to_string(), title.to_string()];
        if !description.is_empty() {
            args.push("--description".into());
            args.push(description.into());
        }
        let value: Value = self.run_json_owned(&args)?;
        value
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("Marbles create returned no id")
    }

    pub fn claim(&self, id: &str) -> Result<Value> {
        self.run_json(&["claim", id])
    }

    pub fn close(&self, id: &str, reason: &str) -> Result<Issue> {
        let mut args = vec!["close".to_string(), id.to_string()];
        if !reason.is_empty() {
            args.push("--reason".into());
            args.push(reason.into());
        }
        self.run_json_owned(&args)
    }

    fn run_json<T>(&self, args: &[&str]) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        self.run_json_owned(&args)
    }

    fn run_json_owned<T>(&self, args: &[String]) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let output = self.command(args).output().context("run mb")?;
        if !output.status.success() || output.stdout.len() > MAX_OUTPUT {
            anyhow::bail!(
                "mb {} failed",
                args.first().map(String::as_str).unwrap_or("call")
            );
        }
        serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "parse mb {} json",
                args.first().map(String::as_str).unwrap_or("call")
            )
        })
    }

    fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(&self.binary);
        command.args(args).arg("--json");
        if let Some(project) = &self.project {
            command.arg("--project").arg(project);
        }
        if let Some(url) = &self.url {
            command.arg("--url").arg(url);
        }
        if let Some(token_file) = &self.token_file {
            command.arg("--token-file").arg(token_file);
        }
        if let Some(actor) = &self.actor {
            command.arg("--actor").arg(actor);
        }
        command
    }
}

/// Host-granted Marbles snapshot used by search and mention autocomplete.
#[derive(Debug, Clone)]
pub struct MarblesIndex {
    pub store_id: String,
    pub company: String,
    pub issues: Vec<Issue>,
}

impl MarblesIndex {
    pub fn new(
        store_id: impl Into<String>,
        company: impl Into<String>,
        issues: Vec<Issue>,
    ) -> Self {
        Self {
            store_id: store_id.into(),
            company: company.into(),
            issues,
        }
    }

    pub fn canonical_ref(&self, issue: &Issue) -> String {
        format!(
            "marbles://{}/{}/{}/{}",
            self.store_id, self.company, issue.project, issue.id
        )
    }

    pub fn search(&self, text: &str, limit: usize) -> Vec<(Issue, u32)> {
        let needle = text.to_ascii_lowercase();
        let mut hits = self
            .issues
            .iter()
            .filter_map(|issue| {
                let haystack = format!(
                    "{} {} {} {} {} {}",
                    issue.id,
                    issue.title,
                    issue.description,
                    issue.project,
                    issue.assignee.as_deref().unwrap_or_default(),
                    issue.labels.join(" ")
                )
                .to_ascii_lowercase();
                if !haystack.contains(&needle) {
                    return None;
                }
                let mut score = 900_000u32;
                if issue.title.to_ascii_lowercase().contains(&needle) {
                    score += 10_000;
                }
                if issue.id.eq_ignore_ascii_case(text) {
                    score += 20_000;
                }
                Some((issue.clone(), score))
            })
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| left.0.title.cmp(&right.0.title))
        });
        hits.truncate(limit);
        hits
    }

    pub fn from_snapshot(path: impl AsRef<Path>, store_id: &str, company: &str) -> Result<Self> {
        let bytes = std::fs::read(path.as_ref())
            .with_context(|| format!("read {}", path.as_ref().display()))?;
        let issues: Vec<Issue> =
            serde_json::from_slice(&bytes).context("parse Marbles snapshot")?;
        Ok(Self::new(store_id, company, issues))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_ref_is_store_company_project_issue() {
        let index = MarblesIndex::new("store", "fpl", Vec::new());
        let issue = Issue {
            id: "ACME-42".into(),
            project: "acme".into(),
            ..Issue::default()
        };
        assert_eq!(
            index.canonical_ref(&issue),
            "marbles://store/fpl/acme/ACME-42"
        );
    }

    #[test]
    fn search_matches_title_and_labels() {
        let index = MarblesIndex::new(
            "store",
            "fpl",
            vec![Issue {
                id: "ACME-42".into(),
                title: "Fix SSO".into(),
                labels: vec!["auth".into()],
                project: "acme".into(),
                ..Issue::default()
            }],
        );
        assert_eq!(index.search("sso", 10).len(), 1);
        assert_eq!(index.search("auth", 10).len(), 1);
        assert!(index.search("missing", 10).is_empty());
    }

    #[cfg(unix)]
    fn fake_mb(dir: &std::path::Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("mb");
        let script = r#"#!/bin/sh
case "$1" in
  list|ready) echo '[{"id":"ACME-42","title":"Fix SSO","status":"open","project":"acme","updated_at":1700000000}]' ;;
  show) echo '[{"id":"ACME-42","title":"Fix SSO","status":"open","project":"acme","updated_at":1700000000}]' ;;
  create) echo '{"id":"ACME-43"}' ;;
  claim) echo '{"id":"ACME-42","assignee":"avery","actor_kind":"human"}' ;;
  close) echo '{"id":"ACME-42","title":"Fix SSO","status":"closed","project":"acme"}' ;;
  *) echo '{}' ;;
esac
"#;
        std::fs::write(&path, script).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn cli_adapter_uses_the_json_contract() {
        let dir = tempfile::tempdir().unwrap();
        let client = MarblesClient::new(fake_mb(dir.path()))
            .with_project("acme")
            .with_actor("avery");
        assert_eq!(client.list().unwrap()[0].id, "ACME-42");
        assert_eq!(client.show("ACME-42").unwrap().title, "Fix SSO");
        assert_eq!(client.create("Fix SSO", "details").unwrap(), "ACME-43");
        assert_eq!(client.claim("ACME-42").unwrap()["assignee"], "avery");
        assert_eq!(client.close("ACME-42", "done").unwrap().status, "closed");
    }
}
