//! The generator's own tests.
//!
//! Two failure modes matter more than coverage: an artifact that drifted from
//! the WIT it claims to describe, and a document whose `$ref`s point at nothing.
//! Both are checked here, and both are checked against what the CLI actually
//! writes rather than a re-implementation of it.

use std::fs;
use std::path::PathBuf;

use soil_openapi::{Shape, describe, openapi, schema, typescript};

fn generated_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../soil-web-bridge/generated")
}

fn records() -> soil_openapi::Api {
    describe("soil:records").expect("soil:records is a canonical package")
}

#[test]
fn the_checked_in_artifacts_match_the_wit() {
    let api = records();
    let (document, client) = soil_openapi::artifacts(&api).unwrap();
    let dir = generated_dir();

    let on_disk_document = fs::read_to_string(dir.join("openapi.json")).expect(
        "crates/soil-web-bridge/generated/openapi.json is missing; \
         run `cargo run -p soil-openapi -- --package soil:records --out crates/soil-web-bridge/generated`",
    );
    let on_disk_client =
        fs::read_to_string(dir.join("client.ts")).expect("generated/client.ts is missing");

    assert_eq!(
        on_disk_document, document,
        "openapi.json is stale: regenerate it from the WIT"
    );
    assert_eq!(
        on_disk_client, client,
        "client.ts is stale: regenerate it from the WIT"
    );
}

#[test]
fn every_reference_resolves_to_a_component() {
    let document = openapi::document(&records());
    let mut references = Vec::new();
    openapi::references(&document, &mut references);
    assert!(
        !references.is_empty(),
        "a document with no refs describes nothing"
    );

    let schemas = document["components"]["schemas"]
        .as_object()
        .expect("components.schemas");
    for reference in references {
        let name = reference
            .strip_prefix("#/components/schemas/")
            .unwrap_or_else(|| panic!("{reference} is not a component reference"));
        assert!(schemas.contains_key(name), "{reference} does not resolve");
    }
}

#[test]
fn a_byte_list_is_base64_and_not_an_array_of_numbers() {
    let api = records();
    let entry = api
        .types
        .iter()
        .find(|ty| ty.name == "entry")
        .expect("entry is declared");
    let body = match &entry.shape {
        Shape::Record(fields) => fields
            .iter()
            .find(|field| field.ts == "body")
            .expect("entry.body"),
        other => panic!("entry is not a record: {other:?}"),
    };
    assert!(matches!(body.shape, Shape::Bytes), "{:?}", body.shape);
    assert_eq!(schema::ts(&body.shape), "string");
    assert_eq!(schema::json(&body.shape)["format"], "byte");
}

#[test]
fn the_client_types_what_the_bridge_writes() {
    let client = typescript::client(&records());

    // Kebab field names become camelCase, and a declaration is its body rather
    // than a reference to itself.
    assert!(client.contains("export interface RecordsEntry {"));
    assert!(client.contains("  updatedAtMs: number;"));
    assert!(!client.contains("export type RecordsKey = RecordsKey"));

    // Enum values stay kebab: they are values, not identifiers.
    assert!(client.contains(r#"export type RecordsError = "denied" | "not-found" | "over-budget" | "invalid" | "stale" | "io" | "unsupported";"#));

    // The call table correlates request and response, which the OpenAPI
    // document cannot.
    assert!(client.contains(
        r#""soil:records.upsert": { params: { entry: NewRecordsEntry }; ok: RecordsEntry };"#
    ));
    assert!(client.contains("upsert(params: { entry: NewRecordsEntry })"));
}

#[test]
fn an_unknown_package_is_refused_with_the_known_ones() {
    let error = describe("soil:nope").unwrap_err().to_string();
    assert!(error.contains("soil:records"), "{error}");
}
