//! One [`Shape`] rendered as JSON Schema and as TypeScript.
//!
//! The two renderings are kept side by side because they must agree: a
//! document that says `string` where the client says `number` is worse than no
//! document. Neither one is derived from the other.

use serde_json::{Value, json};

use crate::{ApiCase, ApiField, Shape};

/// OpenAPI 3.1 schema for one shape.
pub fn json(shape: &Shape) -> Value {
    match shape {
        Shape::Boolean => json!({ "type": "boolean" }),
        Shape::U8 => json!({ "type": "integer", "minimum": 0, "maximum": 255 }),
        Shape::U16 => json!({ "type": "integer", "minimum": 0, "maximum": 65535 }),
        Shape::U32 => json!({ "type": "integer", "minimum": 0, "maximum": 4294967295u64 }),
        // A u64 does not fit a JSON number that every client can hold; the
        // format is the warning, and TypeScript says `number` regardless.
        Shape::U64 => json!({ "type": "integer", "format": "uint64", "minimum": 0 }),
        Shape::S8 => json!({ "type": "integer", "minimum": -128, "maximum": 127 }),
        Shape::S16 => json!({ "type": "integer", "minimum": -32768, "maximum": 32767 }),
        Shape::S32 => {
            json!({ "type": "integer", "minimum": -2147483648i64, "maximum": 2147483647 })
        }
        Shape::S64 => json!({ "type": "integer", "format": "int64" }),
        Shape::F32 => json!({ "type": "number", "format": "float" }),
        Shape::F64 => json!({ "type": "number", "format": "double" }),
        Shape::Char => json!({ "type": "string", "minLength": 1, "maxLength": 1 }),
        Shape::Str => json!({ "type": "string" }),
        // The component-model JSON mapping for `list<u8>`, and what the bridge
        // writes: standard base64 in a string.
        Shape::Bytes => json!({ "type": "string", "format": "byte", "contentEncoding": "base64" }),
        Shape::Named(name) => json!({ "$ref": format!("#/components/schemas/{name}") }),
        Shape::List(inner) => json!({ "type": "array", "items": json(inner) }),
        Shape::Optional(inner) => json!({ "anyOf": [json(inner), { "type": "null" }] }),
        Shape::Tuple(items) => json!({
            "type": "array",
            "prefixItems": items.iter().map(json).collect::<Vec<_>>(),
            "minItems": items.len(),
            "maxItems": items.len(),
        }),
        Shape::Result { ok, err } => json!({
            "oneOf": [
                half("ok", ok.as_deref()),
                half("err", err.as_deref()),
            ]
        }),
        Shape::Record(fields) => record(fields),
        Shape::Enum(cases) => json!({
            "type": "string",
            "enum": cases.iter().map(|case| case.name.as_str()).collect::<Vec<_>>(),
        }),
        Shape::Variant(cases) => json!({
            "oneOf": cases.iter().map(variant_case).collect::<Vec<_>>()
        }),
        Shape::Flags(flags) => json!({
            "type": "array",
            "items": { "type": "string", "enum": flags },
            "uniqueItems": true,
        }),
        Shape::Unit => json!({ "type": "object", "additionalProperties": false }),
        Shape::Handle => json!({
            "type": "string",
            "description": "opaque host-held handle; the id is valid only for this session",
        }),
        // `describe` refuses to build an Api containing one, so reaching here is
        // a generator bug rather than a description of the contract.
        Shape::Unsupported(what) => json!({ "description": format!("unsupported: {what}") }),
    }
}

fn half(name: &str, shape: Option<&Shape>) -> Value {
    match shape {
        Some(shape) => json!({
            "type": "object",
            "required": [name],
            "additionalProperties": false,
            "properties": { name: json(shape) },
        }),
        None => json!({
            "type": "object",
            "required": [name],
            "additionalProperties": false,
            "properties": { name: { "type": "null" } },
        }),
    }
}

fn record(fields: &[ApiField]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for field in fields {
        let mut schema = json(&field.shape);
        if let Some(docs) = &field.docs
            && let Some(object) = schema.as_object_mut()
        {
            object.insert("description".into(), Value::String(docs.clone()));
        }
        properties.insert(field.ts.clone(), schema);
        required.push(Value::String(field.ts.clone()));
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    })
}

fn variant_case(case: &ApiCase) -> Value {
    let payload = match &case.payload {
        Some(shape) => json(shape),
        None => json!({ "type": "null" }),
    };
    json!({
        "type": "object",
        "required": [case.name],
        "additionalProperties": false,
        "properties": { case.name.clone(): payload },
    })
}

/// TypeScript for one shape.
pub fn ts(shape: &Shape) -> String {
    match shape {
        Shape::Boolean => "boolean".into(),
        Shape::U8 | Shape::U16 | Shape::U32 | Shape::U64 => "number".into(),
        Shape::S8 | Shape::S16 | Shape::S32 | Shape::S64 => "number".into(),
        Shape::F32 | Shape::F64 => "number".into(),
        Shape::Char | Shape::Str | Shape::Handle => "string".into(),
        Shape::Bytes => "string".into(),
        Shape::Named(name) => name.clone(),
        Shape::List(inner) => format!("{}[]", atom(inner)),
        Shape::Optional(inner) => format!("{} | null", atom(inner)),
        Shape::Tuple(items) => format!("[{}]", items.iter().map(ts).collect::<Vec<_>>().join(", ")),
        Shape::Result { ok, err } => format!(
            "{{ ok: {} }} | {{ err: {} }}",
            ok.as_deref().map(ts).unwrap_or_else(|| "null".into()),
            err.as_deref().map(ts).unwrap_or_else(|| "null".into()),
        ),
        Shape::Record(fields) => {
            if fields.is_empty() {
                return "Record<string, never>".into();
            }
            let body = fields
                .iter()
                .map(|field| format!("{}: {}", field.ts, ts(&field.shape)))
                .collect::<Vec<_>>()
                .join("; ");
            format!("{{ {body} }}")
        }
        Shape::Enum(cases) => cases
            .iter()
            .map(|case| format!("\"{}\"", case.name))
            .collect::<Vec<_>>()
            .join(" | "),
        Shape::Variant(cases) => cases
            .iter()
            .map(|case| match &case.payload {
                Some(payload) => format!("{{ \"{}\": {} }}", case.name, ts(payload)),
                None => format!("{{ \"{}\": null }}", case.name),
            })
            .collect::<Vec<_>>()
            .join(" | "),
        Shape::Flags(flags) => format!(
            "Array<{}>",
            flags
                .iter()
                .map(|flag| format!("\"{flag}\""))
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        Shape::Unit => "Record<string, never>".into(),
        Shape::Unsupported(what) => format!("never /* unsupported: {what} */"),
    }
}

/// A shape safe to suffix with `[]` without changing its meaning.
fn atom(shape: &Shape) -> String {
    let rendered = ts(shape);
    match shape {
        Shape::Optional(_) | Shape::Result { .. } | Shape::Enum(_) | Shape::Variant(_) => {
            format!("({rendered})")
        }
        _ => rendered,
    }
}
