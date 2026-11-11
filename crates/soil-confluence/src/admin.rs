//! Hook administration: the seam behind `fungos:webhooks/admin`.
//!
//! The WIT is the contract a console and the CLI both speak. This is the
//! implementation they share, so "create a hook" has exactly one behaviour:
//! a unique slug, a secret shown once, and a revoke that is final. Two callers
//! inventing their own rules is how a console and a CLI end up disagreeing
//! about what a hook is.
//!
//! The credential is still a stand-in here (a `0600` file beside the entry).
//! The shape is the one `soil:creds` describes - name and handle at the
//! boundary, bytes behind it - so moving to Geode is an implementation change,
//! not a contract change.

use crate::webhooks::Registry;

/// A registered endpoint. The secret is deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    pub slug: String,
    pub provider: String,
    pub tenant: String,
    pub created_at_ms: u64,
}

/// The only value that ever carries a secret.
#[derive(Clone, Debug)]
pub struct Created {
    pub hook: Hook,
    pub secret: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A name, provider or tenant the registry cannot accept.
    Invalid,
    /// The slug is already taken: slugs are identity, not preference.
    Conflict,
    /// The caller may not create hooks for this tenant.
    Denied,
    /// The registry could not answer.
    Unavailable,
}

impl Error {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Conflict => "conflict",
            Self::Denied => "denied",
            Self::Unavailable => "unavailable",
        }
    }
}

/// The verbs, shared by every caller.
pub trait Admin {
    fn create(
        &self,
        name: &str,
        provider: &str,
        tenant: &str,
        slug: Option<&str>,
        now_ms: u64,
    ) -> Result<Created, Error>;

    fn revoke(&self, slug: &str) -> Result<bool, Error>;

    fn hooks(&self) -> Result<Vec<Hook>, Error>;
}

impl Admin for Registry {
    fn create(
        &self,
        name: &str,
        provider: &str,
        tenant: &str,
        slug: Option<&str>,
        now_ms: u64,
    ) -> Result<Created, Error> {
        if [name, provider, tenant]
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err(Error::Invalid);
        }
        // A caller-chosen slug must not silently replace an existing hook: the
        // endpoint somebody else's provider is already posting to would start
        // accepting under a new secret.
        if let Some(slug) = slug
            && self.get(slug).map_err(|_| Error::Unavailable)?.is_some()
        {
            return Err(Error::Conflict);
        }
        let created = self
            .create(name, provider, tenant, slug, now_ms)
            .map_err(|_| Error::Unavailable)?;
        Ok(Created {
            hook: hook_of(created.webhook),
            secret: created.secret,
        })
    }

    fn revoke(&self, slug: &str) -> Result<bool, Error> {
        self.revoke(slug).map_err(|_| Error::Unavailable)
    }

    fn hooks(&self) -> Result<Vec<Hook>, Error> {
        Ok(self
            .list()
            .map_err(|_| Error::Unavailable)?
            .into_iter()
            .map(hook_of)
            .collect())
    }
}

fn hook_of(webhook: crate::webhooks::Webhook) -> Hook {
    Hook {
        slug: webhook.slug,
        provider: webhook.provider,
        tenant: webhook.tenant,
        created_at_ms: webhook.created_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (Registry, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "soil-confluence-admin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).expect("scratch registry");
        (Registry::new(&dir), dir)
    }

    #[test]
    fn creating_a_hook_lists_it_and_shows_the_secret_once() {
        let (registry, dir) = registry();
        let created = Admin::create(
            &registry,
            "github avery/soil",
            "github",
            "default",
            Some("avery-soil"),
            1_000,
        )
        .expect("create");
        assert_eq!(created.hook.slug, "avery-soil");
        assert!(!created.secret.is_empty());

        let listed = Admin::hooks(&registry).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].slug, "avery-soil");
        // The listing is the thing a console renders. It must never carry the
        // secret, or the console becomes a credential server.
        assert_eq!(listed[0].provider, "github");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_taken_slug_is_a_conflict_not_a_replacement() {
        let (registry, dir) = registry();
        Admin::create(
            &registry,
            "first",
            "github",
            "default",
            Some("shared"),
            1_000,
        )
        .expect("create");
        assert_eq!(
            Admin::create(
                &registry,
                "second",
                "github",
                "default",
                Some("shared"),
                2_000
            )
            .expect_err("must not replace"),
            Error::Conflict
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn revoking_twice_is_not_an_error() {
        let (registry, dir) = registry();
        Admin::create(&registry, "hook", "gitea", "default", Some("gone"), 1_000).expect("create");
        assert!(Admin::revoke(&registry, "gone").expect("revoke"));
        assert!(!Admin::revoke(&registry, "gone").expect("second revoke"));
        assert!(Admin::hooks(&registry).expect("list").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_empty_field_is_invalid() {
        let (registry, dir) = registry();
        assert_eq!(
            Admin::create(&registry, "", "github", "default", None, 1_000).expect_err("no name"),
            Error::Invalid
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
