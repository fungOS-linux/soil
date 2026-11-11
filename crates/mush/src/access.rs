//! Who is asking, and what the host lets them reach.
//!
//! The principal and its groups come from the host's own environment and config,
//! never from the guest: a wasm Spore cannot set its parent's environment, so
//! this is an authority boundary rather than a claim the shell can forge. That
//! is also why the policy is assembled here and handed to the capability, not
//! passed as an argument the guest could choose.
//!
//! Groups are additive. A principal in `agents` and `build-fleet` gets the union
//! of both origin lists, and a principal in no group falls back to the explicit
//! `MUSH_HTTP_ALLOW` — which, empty, denies everything. That fallback is what
//! keeps a bare `mush` from reaching anywhere while the group policy is being
//! adopted.

use std::collections::BTreeSet;

use crate::config::{self, Config};

#[derive(Debug, Default, Clone)]
pub struct Access {
    pub principal: Option<String>,
    pub groups: Vec<String>,
    /// Origins this principal may fetch, the union across its groups.
    pub origins: BTreeSet<String>,
}

impl Access {
    pub fn describe(&self) -> String {
        let principal = self.principal.as_deref().unwrap_or("(anonymous)");
        let groups = if self.groups.is_empty() {
            "(none)".to_string()
        } else {
            self.groups.join(", ")
        };
        let origins = if self.origins.is_empty() {
            "(nothing)".to_string()
        } else {
            self.origins.iter().cloned().collect::<Vec<_>>().join(", ")
        };
        format!("{principal} groups={groups} fetch={origins}")
    }
}

/// Assemble the caller's access from the environment and the config file.
pub fn from_env() -> Access {
    let path = config::default_path();
    let config = config::load(&path).unwrap_or_else(|error| {
        eprintln!("mush: ignoring {}: {error}", path.display());
        Config::default()
    });
    resolve(&config, |name| std::env::var(name).ok())
}

/// The pure form, so the merge rules can be tested without touching the host.
pub fn resolve(config: &Config, env: impl Fn(&str) -> Option<String>) -> Access {
    let principal = env("MUSH_PRINCIPAL")
        .or_else(|| config.principal.clone())
        .filter(|principal| !principal.is_empty());

    let mut groups = config.groups.clone();
    if let Some(value) = env("MUSH_GROUPS") {
        groups.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|group| !group.is_empty())
                .map(str::to_string),
        );
    }
    groups.sort();
    groups.dedup();

    let mut origins = BTreeSet::new();
    for group in &groups {
        if let Some(list) = config.group_origins.get(group) {
            origins.extend(list.iter().cloned());
        }
    }
    if groups.is_empty()
        && let Some(value) = env("MUSH_HTTP_ALLOW")
    {
        origins.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(str::to_string),
        );
    }

    Access {
        principal,
        groups,
        origins,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(groups: &[&str], policy: &[(&str, &[&str])]) -> Config {
        Config {
            groups: groups.iter().map(|group| group.to_string()).collect(),
            group_origins: policy
                .iter()
                .map(|(group, origins)| {
                    (
                        group.to_string(),
                        origins.iter().map(|origin| origin.to_string()).collect(),
                    )
                })
                .collect(),
            ..Config::default()
        }
    }

    #[test]
    fn a_principal_gets_the_union_of_its_groups() {
        let access = resolve(
            &config(
                &["agents"],
                &[
                    ("agents", &["https://api.github.com"]),
                    ("build-fleet", &["https://internal.example"]),
                ],
            ),
            |name| (name == "MUSH_GROUPS").then(|| "build-fleet".to_string()),
        );
        assert_eq!(access.groups, vec!["agents", "build-fleet"]);
        assert_eq!(
            access.origins.iter().cloned().collect::<Vec<_>>(),
            vec!["https://api.github.com", "https://internal.example"]
        );
    }

    #[test]
    fn a_group_without_origins_adds_nothing() {
        let access = resolve(&config(&["agents"], &[]), |_| None);
        assert!(access.origins.is_empty(), "{:?}", access.origins);
    }

    #[test]
    fn no_groups_falls_back_to_the_explicit_allowlist() {
        let access = resolve(&Config::default(), |name| {
            (name == "MUSH_HTTP_ALLOW").then(|| "https://example.com".to_string())
        });
        assert_eq!(
            access.origins.iter().cloned().collect::<Vec<_>>(),
            vec!["https://example.com"]
        );
    }

    #[test]
    fn groups_suppress_the_fallback_allowlist() {
        // Being in a group is a policy statement: the group's origins are the
        // answer, even when they add up to nothing.
        let access = resolve(&config(&["agents"], &[]), |name| {
            (name == "MUSH_HTTP_ALLOW").then(|| "https://example.com".to_string())
        });
        assert!(access.origins.is_empty(), "{:?}", access.origins);
    }

    #[test]
    fn the_environment_principal_wins_over_the_config() {
        let access = resolve(
            &Config {
                principal: Some("config".into()),
                ..Config::default()
            },
            |name| (name == "MUSH_PRINCIPAL").then(|| "env".to_string()),
        );
        assert_eq!(access.principal.as_deref(), Some("env"));
    }
}
