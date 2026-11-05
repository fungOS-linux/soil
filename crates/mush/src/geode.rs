//! Native credential resolver for Geode handles.
//!
//! The first adapter is deliberately small: a host-configured executable is
//! invoked as `geode get NAME` and must print the secret to stdout. The command
//! is declared by the host; it is never selected by a Spore or Operator. An
//! environment bridge is retained for development.

use std::{
    collections::BTreeMap,
    process::{Command, Stdio},
    sync::Mutex,
    time::Duration,
};

use anyhow::{Context, Result};
use wait_timeout::ChildExt;

use crate::config::GeodeConfig;

pub struct GeodeResolver {
    config: GeodeConfig,
    cache: Mutex<BTreeMap<String, String>>,
}

impl GeodeResolver {
    pub fn new(config: GeodeConfig) -> Self {
        Self {
            config,
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn resolve(&self, handle: &str) -> Result<Option<String>> {
        let Some(name) = handle.strip_prefix("geode://") else {
            return Ok(None);
        };
        if let Some(value) = self.cache.lock().expect("geode cache poisoned").get(name) {
            return Ok(Some(value.clone()));
        }
        if let Ok(value) = std::env::var(format!("GEODE_{}", name.to_ascii_uppercase())) {
            self.remember(name, &value);
            return Ok(Some(value));
        }
        let Some(command) = &self.config.command else {
            return Ok(None);
        };
        let mut child = Command::new(command)
            .arg("get")
            .arg(name)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("start Geode command {}", command.display()))?;
        let status = child
            .wait_timeout(Duration::from_millis(self.config.timeout_ms.max(100)))?
            .context("Geode command timed out")?;
        let output = child.wait_with_output()?;
        if !status.success() {
            anyhow::bail!(
                "Geode command failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let value = String::from_utf8(output.stdout)
            .context("Geode returned non-UTF-8 output")?
            .trim_end_matches(['\r', '\n'])
            .to_string();
        if value.is_empty() {
            anyhow::bail!("Geode returned an empty credential");
        }
        self.remember(name, &value);
        Ok(Some(value))
    }

    fn remember(&self, name: &str, value: &str) {
        self.cache
            .lock()
            .expect("geode cache poisoned")
            .insert(name.to_string(), value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn resolves_a_declared_geode_command() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("geode");
        std::fs::write(
            &script,
            "#!/bin/sh\n[ \"$1\" = get ] && [ \"$2\" = umie ] && printf 'secret-value\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let resolver = GeodeResolver::new(GeodeConfig {
            command: Some(script),
            timeout_ms: 1_000,
        });
        assert_eq!(
            resolver.resolve("geode://umie").unwrap().as_deref(),
            Some("secret-value")
        );
    }

    #[test]
    fn unknown_handles_are_not_credentials() {
        let resolver = GeodeResolver::new(GeodeConfig::default());
        assert_eq!(resolver.resolve("env:TOKEN").unwrap(), None);
    }
}
