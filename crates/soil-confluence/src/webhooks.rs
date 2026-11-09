//! A registry of webhooks: one entry per endpoint a provider posts to.
//!
//! An entry is a small JSON document on disk, named by its slug, with the
//! secret written `0600` beside it. Keeping one file per hook means creating a
//! hook never rewrites another one, revoking is a delete, and an operator can
//! read the directory without a tool.
//!
//! The URL is derived, never stored: `slot` is scope, not URL.

use std::{fs, io, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::EVENT_KIND;

/// Where a hook's secret is kept relative to its entry.
const SECRET_FILE: &str = "secret";

/// One webhook endpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Webhook {
    /// Path segment: `/webhooks/<provider>/<slug>`.
    pub slug: String,
    pub name: String,
    /// Provider family, e.g. `github`. Chosen by the caller, not inferred.
    pub provider: String,
    pub tenant: String,
    /// `soil:unibus` kind the Spore publishes for this hook.
    pub kind: String,
    pub created_at_ms: u64,
}

/// What a caller gets back exactly once, when a hook is created.
#[derive(Clone, Debug)]
pub struct Created {
    pub webhook: Webhook,
    pub secret: String,
}

/// A directory of webhook entries.
#[derive(Clone, Debug)]
pub struct Registry {
    dir: PathBuf,
}

impl Registry {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Create a hook and return its secret. The secret is written to the entry's
    /// `secret` file and is never returned by [`Self::list`].
    pub fn create(
        &self,
        name: &str,
        provider: &str,
        tenant: &str,
        slug: Option<&str>,
        now_ms: u64,
    ) -> Result<Created> {
        let provider = slugify(provider)?;
        let slug = match slug {
            Some(slug) => slugify(slug)?,
            None => random_slug()?,
        };
        let entry = self.entry(&slug);
        if entry.exists() {
            bail!("a webhook named {slug} already exists");
        }
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("create registry {}", self.dir.display()))?;

        let secret = random_secret()?;
        let webhook = Webhook {
            slug: slug.clone(),
            name: name.to_owned(),
            provider,
            tenant: tenant.to_owned(),
            kind: EVENT_KIND.to_owned(),
            created_at_ms: now_ms,
        };
        let document = serde_json::to_vec_pretty(&webhook)?;
        write_private(&entry, &document)?;
        // The secret lives beside the entry so a reader with the directory can
        // not learn it from a listing, and deleting the entry deletes both.
        write_private(&self.secret_path(&slug), secret.as_bytes())?;
        Ok(Created { webhook, secret })
    }

    pub fn list(&self) -> Result<Vec<Webhook>> {
        let mut hooks = Vec::new();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(hooks),
            Err(error) => return Err(error).context("read registry"),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let text = fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            hooks.push(
                serde_json::from_str(&text)
                    .with_context(|| format!("parse {}", path.display()))?,
            );
        }
        hooks.sort_by(|a, b| a.name.cmp(&b.name).then(a.slug.cmp(&b.slug)));
        Ok(hooks)
    }

    pub fn get(&self, slug: &str) -> Result<Option<Webhook>> {
        match fs::read_to_string(self.entry(slug)) {
            Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("read webhook"),
        }
    }

    pub fn revoke(&self, slug: &str) -> Result<bool> {
        let mut removed = false;
        for path in [self.entry(slug), self.secret_path(slug)] {
            match fs::remove_file(&path) {
                Ok(()) => removed = true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
            }
        }
        Ok(removed)
    }

    /// Every secret, keyed by the credential name the Spore resolves.
    ///
    /// The name is the slug: the Spore asks `soil:creds` for the path segment it
    /// was called on, so no hook needs a restart to be addressed.
    pub fn secrets(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let mut secrets = Vec::new();
        for webhook in self.list()? {
            let path = self.secret_path(&webhook.slug);
            let secret = fs::read(&path)
                .with_context(|| format!("read secret for {}", webhook.slug))?;
            secrets.push((webhook.slug, secret));
        }
        Ok(secrets)
    }

    fn entry(&self, slug: &str) -> PathBuf {
        self.dir.join(format!("{slug}.json"))
    }

    fn secret_path(&self, slug: &str) -> PathBuf {
        self.dir.join(slug).join(SECRET_FILE)
    }
}

/// Lowercase alphanumeric with single hyphens, so a slug is a safe path segment.
fn slugify(value: &str) -> Result<String> {
    let slug: String = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        bail!("{value:?} has no usable characters for a slug");
    }
    if slug.len() > 64 {
        bail!("{value:?} is too long for a slug");
    }
    Ok(slug)
}

fn random_slug() -> Result<String> {
    Ok(random_bytes(8)?.iter().map(|b| format!("{b:02x}")).collect())
}

fn random_secret() -> Result<String> {
    use base64::Engine as _;
    let bytes = random_bytes(32)?;
    Ok(format!(
        "whsec_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

fn random_bytes(len: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; len];
    getrandom::getrandom(&mut bytes).map_err(|error| anyhow::anyhow!("random: {error}"))?;
    Ok(bytes)
}

fn write_private(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
