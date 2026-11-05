//! One Soil-owned operator/inference profile.
//!
//! Config is resolved once by the host and passed to Operator as environment or
//! CLI flags. Operator never chooses an endpoint, credential, or MCP route.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SoilConfig {
    #[serde(default)]
    pub inference: InferenceConfig,
    #[serde(default)]
    pub operator: OperatorConfig,
    #[serde(default)]
    pub geode: GeodeConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct InferenceConfig {
    pub default: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, InferenceProfile>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct InferenceProfile {
    pub provider: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// Opaque credential handle, e.g. `geode://umie`.
    pub credential: Option<String>,
    /// Environment variable containing the bearer token, when not using Geode.
    pub api_key_env: Option<String>,
    /// Mycelium/Unibus service id, e.g. `umie/inference`.
    pub service: Option<String>,
    /// Host-granted `services.json` snapshot.
    pub services: Option<PathBuf>,
    /// Endpoint suffix appended to a discovered service, default `/v1`.
    pub endpoint_suffix: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct OperatorConfig {
    pub operator_bin: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub mcp: McpConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct McpConfig {
    pub url: Option<String>,
    pub url_env: Option<String>,
    pub token_env: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct GeodeConfig {
    /// Native command that implements `get NAME` on stdout.
    pub command: Option<PathBuf>,
    #[serde(default = "default_geode_timeout")]
    pub timeout_ms: u64,
}

fn default_geode_timeout() -> u64 {
    5_000
}

#[derive(Clone, Debug, Default)]
pub struct ResolvedOperator {
    pub provider: Option<String>,
    pub operator_bin: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub credential: Option<String>,
    pub model: Option<String>,
    pub mcp_url: Option<String>,
    pub mcp_token: Option<String>,
}

impl SoilConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("read Soil config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse Soil config {}", path.display()))
    }

    pub fn discover(root: &Path) -> Result<Option<(PathBuf, Self)>> {
        let candidates = [
            root.join(".soil/config.toml"),
            home().join(".config/soil/config.toml"),
        ];
        for path in candidates {
            if path.is_file() {
                return Ok(Some((path.clone(), Self::load(&path)?)));
            }
        }
        Ok(None)
    }

    pub fn resolve(&self, profile: Option<&str>) -> Result<ResolvedOperator> {
        let profile_name = profile
            .map(str::to_string)
            .or_else(|| self.inference.default.clone());
        let profile = profile_name
            .as_deref()
            .and_then(|name| self.inference.profiles.get(name));
        let base_url = profile
            .and_then(|profile| profile.base_url.clone())
            .or_else(|| {
                profile
                    .and_then(|profile| profile.service.clone())
                    .and_then(|service| {
                        let snapshot = profile.and_then(|profile| profile.services.clone())?;
                        find_service_endpoint(&snapshot, &service).ok().flatten()
                    })
            })
            .map(|value| expand_contract(&value));
        let base_url = match (base_url, profile) {
            (Some(url), Some(profile)) if !url.ends_with("/v1") => {
                let suffix = profile.endpoint_suffix.as_deref().unwrap_or("/v1");
                Some(format!("{url}{suffix}"))
            }
            (url, _) => url,
        };
        let api_key = profile
            .and_then(|profile| profile.api_key_env.as_deref())
            .and_then(|name| std::env::var(name).ok());
        Ok(ResolvedOperator {
            provider: profile.and_then(|profile| profile.provider.clone()),
            operator_bin: self.operator.operator_bin.clone(),
            base_url,
            api_key,
            credential: profile.and_then(|profile| profile.credential.clone()),
            model: profile
                .and_then(|profile| profile.model.clone())
                .or_else(|| self.operator.model.clone()),
            mcp_url: self
                .operator
                .mcp
                .url
                .clone()
                .map(|value| expand_contract(&value))
                .or_else(|| {
                    self.operator
                        .mcp
                        .url_env
                        .as_deref()
                        .and_then(|name| std::env::var(name).ok())
                }),
            mcp_token: self
                .operator
                .mcp
                .token_env
                .as_deref()
                .and_then(|name| std::env::var(name).ok()),
        })
    }
}

fn expand_contract(value: &str) -> String {
    if value == "~" {
        return home().to_string_lossy().into_owned();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home().join(rest).to_string_lossy().into_owned();
    }
    value.to_string()
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn find_service_endpoint(snapshot: &Path, service: &str) -> Result<Option<String>> {
    let text = fs::read_to_string(snapshot)
        .with_context(|| format!("read service snapshot {}", snapshot.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("parse service snapshot {}", snapshot.display()))?;
    Ok(find_service_value(&value, service).and_then(first_endpoint))
}

fn find_service_value<'a>(
    value: &'a serde_json::Value,
    service: &str,
) -> Option<&'a serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("service_id").and_then(serde_json::Value::as_str) == Some(service) {
                return Some(value);
            }
            map.values()
                .find_map(|value| find_service_value(value, service))
        }
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| find_service_value(value, service)),
        _ => None,
    }
}

fn first_endpoint(value: &serde_json::Value) -> Option<String> {
    let endpoints = value
        .get("value")
        .and_then(|value| value.get("endpoints"))
        .or_else(|| value.get("endpoints"))?;
    endpoints
        .as_array()?
        .iter()
        .find_map(|endpoint| endpoint.as_str().map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_named_inference_profile_and_mcp_route() {
        let config: SoilConfig = toml::from_str(
            r#"
            [inference]
            default = "umie"

            [inference.profiles.umie]
            provider = "unibus-umie"
            base_url = "http://127.0.0.1:8096"
            model = "laya"

            [operator]
            model = "fallback"
            operator_bin = "operator-tui"

            [operator.mcp]
            url = "http://127.0.0.1:8792/mcp"
            "#,
        )
        .unwrap();
        let resolved = config.resolve(None).unwrap();
        assert_eq!(
            resolved.base_url.as_deref(),
            Some("http://127.0.0.1:8096/v1")
        );
        assert_eq!(resolved.model.as_deref(), Some("laya"));
        assert_eq!(
            resolved.mcp_url.as_deref(),
            Some("http://127.0.0.1:8792/mcp")
        );
    }

    #[test]
    fn resolves_a_service_endpoint_from_a_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("services.json");
        std::fs::write(
            &snapshot,
            r#"{"observations":[{"service_id":"umie/inference","value":{"endpoints":["http://127.0.0.1:8096"]}}]}"#,
        )
        .unwrap();
        let config: SoilConfig = toml::from_str(&format!(
            r#"
            [inference.profiles.umie]
            service = "umie/inference"
            services = "{}"
            "#,
            snapshot.display()
        ))
        .unwrap();
        let resolved = config.resolve(Some("umie")).unwrap();
        assert_eq!(
            resolved.base_url.as_deref(),
            Some("http://127.0.0.1:8096/v1")
        );
    }
}
