//! Derived graph references for global search.
//!
//! The graph is materialized from two record schemas in host-approved books:
//!
//! - `fungos.graph/link@1` — a semantic edge between two locators.
//! - `fungos.graph/reference@1` — an external identifier for one locator.
//!
//! The graph index is a cache. It never grants access, syncs, or mutates the
//! records it reads.

use std::collections::BTreeMap;

use fungos_records::{Record, Store};

use crate::search_api;

pub(crate) const LINK_SCHEMA: &str = "fungos.graph/link";
pub(crate) const REFERENCE_SCHEMA: &str = "fungos.graph/reference";

const MAX_GRAPH_RECORDS_PER_BOOK: usize = 8_192;
const MAX_REFS_PER_HIT: usize = 16;

#[derive(Default)]
pub(crate) struct GraphIndex {
    refs: BTreeMap<String, Vec<search_api::Reference>>,
}

impl GraphIndex {
    pub(crate) fn build(store: &Store, books: &[String]) -> Self {
        let mut graph = Self::default();
        for book in books {
            if let Ok(links) = store.list_schema(book, LINK_SCHEMA, MAX_GRAPH_RECORDS_PER_BOOK) {
                for link in links {
                    graph.add_link(book, &link);
                }
            }
            if let Ok(refs) = store.list_schema(book, REFERENCE_SCHEMA, MAX_GRAPH_RECORDS_PER_BOOK)
            {
                for reference in refs {
                    graph.add_reference(book, &reference);
                }
            }
        }
        graph
    }

    pub(crate) fn enrich(&self, hits: &mut [search_api::Hit]) {
        for hit in hits {
            let mut refs: Vec<search_api::Reference> = Vec::new();
            for alias in candidates(hit) {
                let Some(found) = self.refs.get(&alias) else {
                    continue;
                };
                for reference in found {
                    if refs.iter().any(|current| {
                        current.kind == reference.kind
                            && current.id == reference.id
                            && current.rel == reference.rel
                            && current.title == reference.title
                    }) {
                        continue;
                    }
                    refs.push(reference.clone());
                }
            }
            refs.sort_by(|a, b| {
                a.kind
                    .cmp(&b.kind)
                    .then_with(|| a.id.cmp(&b.id))
                    .then_with(|| a.rel.cmp(&b.rel))
            });
            refs.truncate(MAX_REFS_PER_HIT);
            hit.references = refs;
        }
    }

    fn add_link(&mut self, book: &str, record: &Record) {
        let Some(from) = field(&record.fields, "from") else {
            return;
        };
        let Some(to) = field(&record.fields, "to") else {
            return;
        };
        let Some(rel) = field(&record.fields, "rel") else {
            return;
        };
        let title = field(&record.fields, "title");
        let target = search_api::Reference {
            kind: reference_kind(to),
            id: to.to_string(),
            rel: Some(rel.to_string()),
            title: title.map(str::to_string),
        };
        for alias in node_aliases(book, from) {
            self.push(alias, target.clone());
        }
        let source = search_api::Reference {
            kind: "record".to_string(),
            id: format!("{book}/{from}"),
            rel: Some(rel.to_string()),
            title: None,
        };
        for alias in node_aliases(book, to) {
            self.push(alias, source.clone());
        }
    }

    fn add_reference(&mut self, book: &str, record: &Record) {
        let Some(node) = field(&record.fields, "node") else {
            return;
        };
        let Some(scheme) = field(&record.fields, "scheme") else {
            return;
        };
        let Some(value) = field(&record.fields, "value") else {
            return;
        };
        let reference = search_api::Reference {
            kind: scheme.to_string(),
            id: value.to_string(),
            rel: field(&record.fields, "rel").map(str::to_string),
            title: field(&record.fields, "title").map(str::to_string),
        };
        for alias in node_aliases(book, node) {
            self.push(alias, reference.clone());
        }
    }

    fn push(&mut self, alias: String, reference: search_api::Reference) {
        let refs = self.refs.entry(alias).or_default();
        if refs.iter().any(|current| {
            current.kind == reference.kind
                && current.id == reference.id
                && current.rel == reference.rel
                && current.title == reference.title
        }) {
            return;
        }
        refs.push(reference);
    }
}

fn field<'a>(fields: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    fields.get(key)?.as_str().filter(|value| !value.is_empty())
}

fn node_aliases(book: &str, value: &str) -> Vec<String> {
    let mut aliases = vec![value.to_string()];
    if !has_locator_scheme(value) {
        let qualified = format!("{book}/{value}");
        if qualified != value {
            aliases.push(qualified);
        }
        let canonical = format!("record:{book}#{value}");
        if canonical != value {
            aliases.push(canonical);
        }
    }
    aliases
}

fn candidates(hit: &search_api::Hit) -> Vec<String> {
    let mut candidates = vec![hit.id.clone()];
    match hit.kind {
        search_api::Kind::RecordEntry => {
            candidates.push(format!("record:{}", hit.id));
            if let Some((book, entity)) = hit.id.rsplit_once('/') {
                candidates.push(entity.to_string());
                candidates.push(format!("record:{book}#{entity}"));
            }
        }
        search_api::Kind::Repository => candidates.push(format!("repository:{}", hit.id)),
        search_api::Kind::File => candidates.push(format!("vfs:{}", hit.id)),
        _ => {}
    }
    candidates
}

fn has_locator_scheme(value: &str) -> bool {
    value.contains(':') && !value.starts_with('/')
}

fn reference_kind(value: &str) -> String {
    if let Some((scheme, _)) = value.split_once(':') {
        match scheme {
            "record" | "repository" | "vfs" | "file" | "url" | "purl" | "cpe" | "doi" | "git"
            | "sha256" => scheme.to_string(),
            _ => "external".to_string(),
        }
    } else {
        "record".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locator_schemes_are_preserved() {
        assert_eq!(reference_kind("repository:acme/dock"), "repository");
        assert_eq!(reference_kind("purl:pkg:cargo/serde"), "purl");
        assert_eq!(reference_kind("soil"), "record");
    }

    #[test]
    fn aliases_cover_local_and_qualified_record_ids() {
        assert_eq!(
            node_aliases("personal/avery/crm", "soil"),
            vec![
                "soil",
                "personal/avery/crm/soil",
                "record:personal/avery/crm#soil"
            ]
        );
        assert_eq!(
            node_aliases("personal/avery/crm", "repository:acme/dock"),
            vec!["repository:acme/dock"]
        );
    }
}
