//! Derived graph index and `soil:graph` host.
//!
//! The graph is materialized from two record schemas in host-approved books:
//!
//! - `fungos.graph/link@1` — a semantic edge between two locators.
//! - `fungos.graph/reference@1` — an external identifier for one locator.
//!
//! The graph index is a cache. It never grants access, syncs, or mutates the
//! records it reads.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use anyhow::Context;
use fungos_records::{Record, Store};
use wasmtime::component::{HasSelf, Linker};

wasmtime::component::bindgen!({ path: "wit/graph.wit", world: "graph-host" });

use self::soil::graph::graph as graph_api;

use crate::search_api;

pub const LINK_SCHEMA: &str = "fungos.graph/link";
pub const REFERENCE_SCHEMA: &str = "fungos.graph/reference";

const MAX_GRAPH_RECORDS_PER_BOOK: usize = 8_192;
const MAX_REFS_PER_HIT: usize = 16;
const MAX_PAGE: u32 = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reference {
    pub kind: String,
    pub id: String,
    pub rel: Option<String>,
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Edge {
    pub id: String,
    pub from: String,
    pub rel: String,
    pub to: String,
    pub role: Option<String>,
    pub title: Option<String>,
    pub book: String,
}

#[derive(Default)]
pub(crate) struct GraphIndex {
    refs: BTreeMap<String, Vec<Reference>>,
    edges: Vec<Edge>,
    by_from: BTreeMap<String, Vec<usize>>,
    by_to: BTreeMap<String, Vec<usize>>,
    reference_records: u64,
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
            let refs = self
                .references_for_aliases(&candidates(hit))
                .into_iter()
                .map(|reference| search_api::Reference {
                    kind: reference.kind,
                    id: reference.id,
                    rel: reference.rel,
                    title: reference.title,
                })
                .collect();
            hit.references = refs;
        }
    }

    pub(crate) fn references_for_aliases(&self, aliases: &[String]) -> Vec<Reference> {
        let mut refs: Vec<Reference> = Vec::new();
        for alias in aliases {
            let Some(found) = self.refs.get(alias) else {
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
        refs
    }

    pub(crate) fn edges_for_aliases(
        &self,
        aliases: &[String],
        rel: Option<&str>,
        direction: graph_api::Direction,
        offset: u64,
        limit: u32,
    ) -> (Vec<Edge>, Option<u64>) {
        let mut ids = BTreeSet::new();
        if matches!(
            direction,
            graph_api::Direction::Out | graph_api::Direction::Both
        ) {
            for alias in aliases {
                if let Some(found) = self.by_from.get(alias) {
                    ids.extend(found.iter().copied());
                }
            }
        }
        if matches!(
            direction,
            graph_api::Direction::In | graph_api::Direction::Both
        ) {
            for alias in aliases {
                if let Some(found) = self.by_to.get(alias) {
                    ids.extend(found.iter().copied());
                }
            }
        }
        let mut edges: Vec<Edge> = ids
            .into_iter()
            .filter_map(|index| self.edges.get(index))
            .filter(|edge| rel.is_none_or(|rel| edge.rel == rel))
            .cloned()
            .collect();
        edges.sort_by(|a, b| a.id.cmp(&b.id));
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let total = edges.len() as u64;
        let page: Vec<_> = edges.into_iter().skip(start).take(limit as usize).collect();
        let consumed = offset.saturating_add(page.len() as u64);
        (page, (consumed < total).then_some(consumed))
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
        let edge = Edge {
            id: format!("{book}/{}", record.entity_id),
            from: from.to_string(),
            rel: rel.to_string(),
            to: to.to_string(),
            role: field(&record.fields, "role").map(str::to_string),
            title: field(&record.fields, "title").map(str::to_string),
            book: book.to_string(),
        };
        let index = self.edges.len();
        self.edges.push(edge.clone());
        for alias in node_aliases(book, from) {
            self.by_from.entry(alias).or_default().push(index);
        }
        for alias in node_aliases(book, to) {
            self.by_to.entry(alias).or_default().push(index);
        }

        let target = Reference {
            kind: reference_kind(to),
            id: to.to_string(),
            rel: Some(rel.to_string()),
            title: field(&record.fields, "title").map(str::to_string),
        };
        for alias in node_aliases(book, from) {
            self.push(alias, target.clone());
        }
        let source = Reference {
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
        let reference = Reference {
            kind: scheme.to_string(),
            id: value.to_string(),
            rel: field(&record.fields, "rel").map(str::to_string),
            title: field(&record.fields, "title").map(str::to_string),
        };
        self.reference_records = self.reference_records.saturating_add(1);
        for alias in node_aliases(book, node) {
            self.push(alias, reference.clone());
        }
    }

    fn push(&mut self, alias: String, reference: Reference) {
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

pub struct GraphService {
    store: Mutex<Store>,
    books: Vec<String>,
    graph: GraphIndex,
    generation: u64,
}

impl GraphService {
    pub fn open(root: impl AsRef<std::path::Path>, books: Vec<String>) -> anyhow::Result<Self> {
        let root = root.as_ref();
        let device = root.join("device_id");
        let store = Store::open(root, &device).context("open fungos-records")?;
        let graph = GraphIndex::build(&store, &books);
        Ok(Self {
            store: Mutex::new(store),
            books,
            graph,
            generation: 1,
        })
    }

    fn state(&self) -> graph_api::GraphState {
        graph_api::GraphState {
            ready: !self.books.is_empty(),
            generation: self.generation,
            edges: self.graph.edges.len() as u64,
            references: self.graph.reference_records,
        }
    }

    fn refresh(&mut self) -> graph_api::GraphState {
        self.graph = GraphIndex::build(
            &self.store.lock().expect("records store poisoned"),
            &self.books,
        );
        self.generation = self.generation.saturating_add(1);
        self.state()
    }
}

impl graph_api::Host for GraphService {
    fn neighbors(
        &mut self,
        node: String,
        rel: Option<String>,
        direction: graph_api::Direction,
        offset: u64,
        limit: u32,
    ) -> Result<graph_api::EdgePage, graph_api::Error> {
        if node.trim().is_empty() {
            return Err(graph_api::Error::Invalid);
        }
        if limit == 0 || limit > MAX_PAGE {
            return Err(graph_api::Error::OverBudget);
        }
        let (edges, next) = self.graph.edges_for_aliases(
            &query_aliases(&node),
            rel.as_deref(),
            direction,
            offset,
            limit,
        );
        Ok(graph_api::EdgePage {
            edges: edges
                .into_iter()
                .map(|edge| graph_api::Edge {
                    id: edge.id,
                    source: edge.from,
                    rel: edge.rel,
                    target: edge.to,
                    role: edge.role,
                    title: edge.title,
                    book: edge.book,
                })
                .collect(),
            next,
            generation: self.generation,
        })
    }

    fn references(
        &mut self,
        node: String,
        offset: u64,
        limit: u32,
    ) -> Result<graph_api::ReferencePage, graph_api::Error> {
        if node.trim().is_empty() {
            return Err(graph_api::Error::Invalid);
        }
        if limit == 0 || limit > MAX_PAGE {
            return Err(graph_api::Error::OverBudget);
        }
        let references = self.graph.references_for_aliases(&query_aliases(&node));
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let total = references.len() as u64;
        let page: Vec<_> = references
            .into_iter()
            .skip(start)
            .take(limit as usize)
            .map(|reference| graph_api::Reference {
                kind: reference.kind,
                id: reference.id,
                rel: reference.rel,
                title: reference.title,
            })
            .collect();
        let consumed = offset.saturating_add(page.len() as u64);
        Ok(graph_api::ReferencePage {
            references: page,
            next: (consumed < total).then_some(consumed),
            generation: self.generation,
        })
    }

    fn state(&mut self) -> Result<graph_api::GraphState, graph_api::Error> {
        Ok(GraphService::state(self))
    }

    fn refresh(&mut self) -> Result<graph_api::GraphState, graph_api::Error> {
        Ok(GraphService::refresh(self))
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: graph_api::Host + 'static,
{
    graph_api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
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

pub(crate) fn query_aliases(node: &str) -> Vec<String> {
    let mut aliases = vec![node.to_string()];
    if let Some(rest) = node.strip_prefix("record:") {
        if let Some((book, entity)) = rest.split_once('#') {
            aliases.push(entity.to_string());
            aliases.push(format!("{book}/{entity}"));
        }
    } else if let Some((book, entity)) = node.rsplit_once('/') {
        aliases.push(entity.to_string());
        aliases.push(format!("record:{book}#{entity}"));
    }
    aliases.sort();
    aliases.dedup();
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
    candidates.sort();
    candidates.dedup();
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

    fn seed_graph(root: &std::path::Path) -> String {
        let book = "personal/avery/projects";
        let mut store = Store::open(root, &root.join("device_id")).unwrap();
        store
            .put_record(
                book,
                "soil",
                "fungos.project/project",
                1,
                serde_json::json!({"title": "Soil"}),
            )
            .unwrap();
        store
            .put_record(
                book,
                "doc-arch",
                "fungos.doc/document",
                1,
                serde_json::json!({"title": "Architecture"}),
            )
            .unwrap();
        store
            .put_record(
                book,
                "link-repo",
                LINK_SCHEMA,
                1,
                serde_json::json!({
                    "from": "soil",
                    "rel": "references",
                    "to": "repository:mycelium-linux/soil",
                    "title": "Soil repository"
                }),
            )
            .unwrap();
        store
            .put_record(
                book,
                "link-doc",
                LINK_SCHEMA,
                1,
                serde_json::json!({
                    "from": "doc-arch",
                    "rel": "mentions",
                    "to": "soil"
                }),
            )
            .unwrap();
        store
            .put_record(
                book,
                "ref-crate",
                REFERENCE_SCHEMA,
                1,
                serde_json::json!({
                    "node": "soil",
                    "scheme": "purl",
                    "value": "pkg:cargo/soil@0.1.0",
                    "title": "Soil crate"
                }),
            )
            .unwrap();
        book.to_string()
    }

    #[test]
    fn graph_host_answers_direction_and_relationship_queries() {
        use graph_api::Host as _;

        let dir = tempfile::tempdir().unwrap();
        let book = seed_graph(dir.path());
        let mut host = GraphService::open(dir.path(), vec![book.clone()]).unwrap();

        let page = host
            .neighbors(
                format!("record:{book}#soil"),
                None,
                graph_api::Direction::Both,
                0,
                10,
            )
            .unwrap();
        assert_eq!(page.edges.len(), 2);
        assert!(page.edges.iter().any(|edge| {
            edge.source == "soil" && edge.target == "repository:mycelium-linux/soil"
        }));
        assert!(page.edges.iter().any(|edge| {
            edge.source == "doc-arch" && edge.target == "soil" && edge.rel == "mentions"
        }));

        let page = host
            .neighbors(
                format!("record:{book}#soil"),
                Some("references".into()),
                graph_api::Direction::Out,
                0,
                10,
            )
            .unwrap();
        assert_eq!(page.edges.len(), 1);
        assert_eq!(page.edges[0].rel, "references");

        let page = host
            .references(format!("record:{book}#soil"), 0, 10)
            .unwrap();
        assert!(page
            .references
            .iter()
            .any(|reference| reference.kind == "purl"
                && reference.id == "pkg:cargo/soil@0.1.0"));

        let before = GraphService::state(&host).generation;
        let after = GraphService::refresh(&mut host).generation;
        assert_eq!(after, before + 1);
    }

    #[test]
    fn query_aliases_cover_canonical_and_path_ids() {
        assert_eq!(
            query_aliases("record:personal/avery/crm#soil"),
            vec![
                "personal/avery/crm/soil",
                "record:personal/avery/crm#soil",
                "soil"
            ]
        );
        assert_eq!(
            query_aliases("personal/avery/crm/soil"),
            vec![
                "personal/avery/crm/soil",
                "record:personal/avery/crm#soil",
                "soil"
            ]
        );
    }
}
