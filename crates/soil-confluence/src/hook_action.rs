//! The one write a console is allowed to make: rename a registered hook.
//!
//! A console that could edit a hook record directly would be a second
//! definition of what a hook is. So the write is an *action*: the view emits
//! an intent, [`soil_actions_host::ActionHost`] checks the grant, and this
//! router applies it to the `hooks` collection in `soil:records`. The record is
//! the registry's read model; the ingress still owns the credential.
//!
//! Narrow on purpose: one action, one field. Revoking a hook deletes a secret,
//! which is an operator decision with its own confirmation, not a form save.

use std::path::PathBuf;

use serde_json::{Value, json};
use soil_actions_host::{
    ActionCall, ActionDescriptor, ActionError, ActionRouter, Approval, Effect, Idempotency,
};
use soil_records_host::{RecordsBookHost, records};

/// Action id stamped into every call. The `dev.fungos.webhooks` prefix is the
/// Spore that owns the surface, so two consoles cannot pick different names.
pub const HOOK_RENAME: &str = "dev.fungos.webhooks/hook.rename";
/// Grants are exact `(id, version)` pairs; a new version is a new decision.
pub const HOOK_RENAME_VERSION: u32 = 1;

/// The catalog entry a host publishes to a view.
pub fn hook_rename_descriptor() -> ActionDescriptor {
    ActionDescriptor {
        id: HOOK_RENAME.to_owned(),
        version: HOOK_RENAME_VERSION,
        summary: "Rename a registered webhook".to_owned(),
        effect: Effect::Write,
        // Renaming twice with the same key must not append a second change.
        idempotency: Idempotency::Keyed,
        approval: Approval::None,
        input_schema: r#"{"type":"object","required":["slug","name"],"properties":{"slug":{"type":"string","maxLength":64},"name":{"type":"string","maxLength":200}},"additionalProperties":false}"#.to_owned(),
        output_schema: r#"{"type":"object","required":["slug","name","revision"],"properties":{"slug":{"type":"string"},"name":{"type":"string"},"revision":{"type":"integer"}}}"#.to_owned(),
        phases: Vec::new(),
        provider: None,
        catalog_revision: None,
        state_revision: None,
    }
}

/// Applies `hook.rename` to one book's `hooks` collection.
///
/// Holds a root and a book name, not an open store: a router outlives any one
/// call, and a store held open across a host restart is a lock nobody owns.
pub struct HookRecordsRouter {
    root: PathBuf,
    book: String,
    collection: String,
}

impl HookRecordsRouter {
    pub fn new(
        root: impl Into<PathBuf>,
        book: impl Into<String>,
        collection: impl Into<String>,
    ) -> Self {
        Self {
            root: root.into(),
            book: book.into(),
            collection: collection.into(),
        }
    }

    fn book(&self) -> Result<RecordsBookHost, ActionError> {
        RecordsBookHost::open(&self.root, self.book.clone(), self.collection.clone())
            .map_err(|error| ActionError::Unavailable(error.to_string()))
    }
}

impl ActionRouter for HookRecordsRouter {
    fn invoke(
        &self,
        _descriptor: &ActionDescriptor,
        call: &ActionCall,
    ) -> Result<String, ActionError> {
        let input: Value = serde_json::from_str(&call.input)
            .map_err(|error| ActionError::Invalid(format!("input is not JSON: {error}")))?;
        let slug = input
            .get("slug")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|slug| !slug.is_empty() && slug.len() <= 64 && !slug.contains('/'))
            .ok_or_else(|| ActionError::Invalid("slug must be one path segment".into()))?
            .to_owned();
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.len() <= 200)
            .ok_or_else(|| ActionError::Invalid("name must be 1-200 characters".into()))?
            .to_owned();

        let mut book = self.book()?;
        let current = book
            .get_entry(&slug)
            .map_err(|error| ActionError::Unavailable(error.to_string()))?
            .ok_or_else(|| ActionError::Invalid(format!("no hook named {slug}")))?;
        let mut body: Value = serde_json::from_slice(&current.body)
            .map_err(|error| ActionError::Invalid(format!("hook record is not JSON: {error}")))?;
        if body.get("slug").and_then(Value::as_str) != Some(slug.as_str()) {
            // The record id and the record's own name disagree: refuse rather
            // than write a rename onto whatever this row actually is.
            return Err(ActionError::Invalid(format!(
                "record {slug} does not describe hook {slug}"
            )));
        }
        body["name"] = Value::String(name.clone());

        let entry = records::Entry {
            key: current.key.clone(),
            schema: current.schema.clone(),
            // Local upsert assigns revision and timestamp; a caller that sent
            // them would be claiming an order it does not own.
            revision: 0,
            updated_at_ms: 0,
            origin: String::new(),
            deleted: false,
            body: serde_json::to_vec(&body)
                .map_err(|error| ActionError::Invalid(error.to_string()))?,
        };
        let saved = records::Host::upsert(&mut book, entry).map_err(|error| {
            ActionError::Unavailable(format!("records upsert failed: {error:?}"))
        })?;

        Ok(json!({
            "slug": slug,
            "name": name,
            "revision": saved.revision,
        })
        .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soil_actions_host::{ActionGrants, ActionHost, IdempotencyStore};

    fn seed(root: &std::path::Path, slug: &str, name: &str) {
        let mut book = RecordsBookHost::open(root, "confluence", "hooks").unwrap();
        let body = json!({
            "slug": slug,
            "name": name,
            "provider": "github",
            "tenant": "fpl",
            "kind": "confluence.webhook.received.v1",
        });
        let entry = records::Entry {
            key: records::Key {
                collection: "hooks".to_owned(),
                id: slug.to_owned(),
            },
            schema: "dev.fungos.webhooks/hook@1".to_owned(),
            revision: 0,
            updated_at_ms: 0,
            origin: String::new(),
            deleted: false,
            body: serde_json::to_vec(&body).unwrap(),
        };
        records::Host::upsert(&mut book, entry).unwrap();
    }

    fn host(root: &std::path::Path, idempotency: &std::path::Path) -> ActionHost {
        let mut grants = ActionGrants::default();
        grants.allow(HOOK_RENAME, HOOK_RENAME_VERSION);
        ActionHost::new(
            [hook_rename_descriptor()],
            grants,
            std::sync::Arc::new(HookRecordsRouter::new(root, "confluence", "hooks")),
            IdempotencyStore::open(idempotency).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn renaming_one_hook_changes_the_record_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let records_root = dir.path().join("records");
        seed(&records_root, "gh-fpl", "github fpl");
        seed(&records_root, "gh-other", "github other");
        let host = host(&records_root, &dir.path().join("idempotency.db"));

        let receipt = host
            .invoke(&ActionCall {
                action: HOOK_RENAME.to_owned(),
                version: HOOK_RENAME_VERSION,
                input: json!({ "slug": "gh-fpl", "name": "fungOS soil" }).to_string(),
                idempotency_key: Some("rename-gh-fpl".to_owned()),
            })
            .unwrap();
        assert!(receipt.output.contains("fungOS soil"), "{}", receipt.output);

        let mut book = RecordsBookHost::open(&records_root, "confluence", "hooks").unwrap();
        let renamed = book.get_entry("gh-fpl").unwrap().unwrap();
        let body: Value = serde_json::from_slice(&renamed.body).unwrap();
        assert_eq!(body["name"], json!("fungOS soil"));
        // The other hook is untouched: a rename addresses one record.
        let other = book.get_entry("gh-other").unwrap().unwrap();
        let other: Value = serde_json::from_slice(&other.body).unwrap();
        assert_eq!(other["name"], json!("github other"));
    }

    #[test]
    fn an_unknown_slug_is_refused_rather_than_created() {
        let dir = tempfile::tempdir().unwrap();
        let records_root = dir.path().join("records");
        seed(&records_root, "gh-fpl", "github fpl");
        let host = host(&records_root, &dir.path().join("idempotency.db"));

        let error = host
            .invoke(&ActionCall {
                action: HOOK_RENAME.to_owned(),
                version: HOOK_RENAME_VERSION,
                input: json!({ "slug": "gh-nope", "name": "x" }).to_string(),
                idempotency_key: Some("rename-gh-nope".to_owned()),
            })
            .unwrap_err();
        assert!(matches!(error, ActionError::Invalid(_)), "{error:?}");

        let mut book = RecordsBookHost::open(&records_root, "confluence", "hooks").unwrap();
        assert!(book.get_entry("gh-nope").unwrap().is_none());
    }

    #[test]
    fn the_same_key_does_not_apply_twice() {
        let dir = tempfile::tempdir().unwrap();
        let records_root = dir.path().join("records");
        seed(&records_root, "gh-fpl", "github fpl");
        let host = host(&records_root, &dir.path().join("idempotency.db"));
        let call = ActionCall {
            action: HOOK_RENAME.to_owned(),
            version: HOOK_RENAME_VERSION,
            input: json!({ "slug": "gh-fpl", "name": "once" }).to_string(),
            idempotency_key: Some("same-key".to_owned()),
        };
        let first = host.invoke(&call).unwrap();
        // A different body under the same key replays the first receipt rather
        // than writing a second time.
        let second = host
            .invoke(&ActionCall {
                input: json!({ "slug": "gh-fpl", "name": "twice" }).to_string(),
                ..call
            })
            .unwrap();
        assert!(second.replayed);
        assert_eq!(first.output, second.output);
    }
}
