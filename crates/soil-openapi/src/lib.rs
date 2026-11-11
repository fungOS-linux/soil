//! A WIT package, as a wire shape.
//!
//! This crate reads the canonical contracts from [`soil_abi::CONTRACTS`] — the
//! same table `cargo test -p soil-abi` asserts against — and turns one package
//! into the two artifacts a browser client needs: an OpenAPI 3.1 document and a
//! TypeScript module.
//!
//! The rule that decides every mapping below: **WIT is the source of truth and
//! the wire is a rendering of it**, never a second contract. Field names become
//! camelCase because a JS property wants to be; enum and variant *values* stay
//! kebab-case because they are values, not identifiers. A `result` is a union of
//! `{ok}` and `{err}` because that is what the bridge envelope carries. A
//! `resource` is a string id because a handle cannot cross a socket.
//!
//! Two things this deliberately does not do. It does not read the Rust
//! implementation: a hand-written `WireEntry` that drifts from the WIT is a bug
//! the generator exposes rather than repeats. And it does not generate a client
//! from the OpenAPI document: the document cannot correlate a response to a
//! request, while the WIT can, so TypeScript is generated from the same IR that
//! the document is.

pub mod openapi;
pub mod schema;
pub mod typescript;

use std::collections::BTreeMap;

use anyhow::{Context, bail};
use soil_abi::CONTRACTS;
use wit_parser::{Resolve, Type, TypeDefKind, TypeId};

/// One interface, flattened for code generation.
#[derive(Debug, Clone)]
pub struct Api {
    /// `soil:records`
    pub package: String,
    /// `0.1.0`
    pub version: String,
    /// `records` — the interface inside the package.
    pub interface: String,
    pub docs: Option<String>,
    pub types: Vec<ApiType>,
    pub functions: Vec<ApiFunction>,
}

#[derive(Debug, Clone)]
pub struct ApiType {
    /// The WIT name, e.g. `entry`.
    pub name: String,
    /// The generated name, e.g. `RecordsEntry`.
    pub ts: String,
    pub docs: Option<String>,
    pub shape: Shape,
}

#[derive(Debug, Clone)]
pub struct ApiFunction {
    pub name: String,
    pub docs: Option<String>,
    pub params: Vec<ApiParam>,
    /// The `ok` half of the WIT result. `Unit` when the function returns
    /// `result<_, e>` or nothing at all.
    pub ok: Shape,
    /// The `err` half, when the function returns a result.
    pub err: Option<Shape>,
}

#[derive(Debug, Clone)]
pub struct ApiParam {
    pub name: String,
    pub ts: String,
    pub docs: Option<String>,
    pub shape: Shape,
}

#[derive(Debug, Clone)]
pub struct ApiField {
    pub name: String,
    pub ts: String,
    pub docs: Option<String>,
    pub shape: Shape,
}

#[derive(Debug, Clone)]
pub struct ApiCase {
    pub name: String,
    pub docs: Option<String>,
    pub payload: Option<Shape>,
}

#[derive(Debug, Clone)]
pub enum Shape {
    Boolean,
    U8,
    U16,
    U32,
    U64,
    S8,
    S16,
    S32,
    S64,
    F32,
    F64,
    Char,
    Str,
    /// `list<u8>`: bytes. JSON carries them base64, which is what the bridge
    /// does and what OpenAPI's `format: byte` means.
    Bytes,
    /// A reference to a named type in this interface.
    Named(String),
    List(Box<Shape>),
    Optional(Box<Shape>),
    Tuple(Vec<Shape>),
    Result {
        ok: Option<Box<Shape>>,
        err: Option<Box<Shape>>,
    },
    Record(Vec<ApiField>),
    Enum(Vec<ApiCase>),
    Variant(Vec<ApiCase>),
    Flags(Vec<String>),
    Unit,
    /// A host-held handle: an opaque id on the wire.
    Handle,
    /// Something this generator does not model yet. Producing one is an error,
    /// not a silent `{}` in the output.
    Unsupported(String),
}

impl Shape {
    /// The first unsupported construct, if any, with the path that found it.
    pub fn unsupported(&self, path: &str, into: &mut Vec<String>) {
        match self {
            Shape::Unsupported(what) => into.push(format!("{path}: {what}")),
            Shape::List(inner) | Shape::Optional(inner) => inner.unsupported(path, into),
            Shape::Tuple(items) => {
                for (index, item) in items.iter().enumerate() {
                    item.unsupported(&format!("{path}[{index}]"), into);
                }
            }
            Shape::Result { ok, err } => {
                if let Some(ok) = ok {
                    ok.unsupported(&format!("{path}.ok"), into);
                }
                if let Some(err) = err {
                    err.unsupported(&format!("{path}.err"), into);
                }
            }
            Shape::Record(fields) => {
                for field in fields {
                    field
                        .shape
                        .unsupported(&format!("{path}.{}", field.ts), into);
                }
            }
            Shape::Variant(cases) => {
                for case in cases {
                    if let Some(payload) = &case.payload {
                        payload.unsupported(&format!("{path}.{}", case.name), into);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The two artifacts, exactly as they are written to disk.
///
/// Shared by the CLI and the drift test so "what the tool writes" and "what the
/// test compares" cannot disagree.
pub fn artifacts(api: &Api) -> anyhow::Result<(String, String)> {
    let document = format!(
        "{}\n",
        serde_json::to_string_pretty(&openapi::document(api))?
    );
    Ok((document, typescript::client(api)))
}

struct Resolved {
    resolve: Resolve,
    packages: BTreeMap<String, wit_parser::PackageId>,
}

fn resolved() -> anyhow::Result<Resolved> {
    let mut resolve = Resolve::default();
    let mut packages = BTreeMap::new();
    for contract in CONTRACTS {
        let id = resolve
            .push_str(contract.file, contract.source)
            .with_context(|| format!("{} must be valid WIT", contract.file))?;
        packages.insert(contract.package.to_owned(), id);
    }
    Ok(Resolved { resolve, packages })
}

/// Every package name this build knows, newest contract list.
pub fn packages() -> anyhow::Result<Vec<String>> {
    Ok(resolved()?.packages.into_keys().collect())
}

/// Turn `soil:records@0.1.0` (or just `soil:records`) into an [`Api`].
pub fn describe(requested: &str) -> anyhow::Result<Api> {
    let Resolved { resolve, packages } = resolved()?;
    let (qualified, package_id) = packages
        .iter()
        .find(|(name, _)| *name == requested || name.split('@').next() == Some(requested))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no such package {requested:?}; known packages: {}",
                packages.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?;
    let package = &resolve.packages[*package_id];
    let (package_name, version) = match &package.name.version {
        Some(version) => (
            format!("{}:{}", package.name.namespace, package.name.name),
            version.to_string(),
        ),
        None => bail!("{qualified} has no version"),
    };

    let (interface_name, interface_id) = package
        .interfaces
        .iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("{package_name} has no interface"))?;
    if package.interfaces.len() > 1 {
        bail!(
            "{package_name} has {} interfaces; the bridge serves one per path today",
            package.interfaces.len()
        );
    }
    let interface = &resolve.interfaces[*interface_id];
    let prefix = pascal(interface_name);

    // Names first: a shape may refer to a type declared later, and a reference
    // has to resolve to the component name rather than inlining a second copy.
    let mut names = BTreeMap::new();
    let mut types = Vec::new();
    for (name, type_id) in &interface.types {
        let ts = format!("{prefix}{}", pascal(name));
        names.insert(*type_id, ts.clone());
        types.push(ApiType {
            name: name.clone(),
            ts,
            docs: docs(&resolve, *type_id),
            shape: Shape::Unit,
        });
    }
    for (index, (_, type_id)) in interface.types.iter().enumerate() {
        // The root of a declaration is its body, not a reference to itself.
        types[index].shape = shape_id(&resolve, &names, *type_id);
    }

    let mut functions = Vec::new();
    for (name, function) in &interface.functions {
        let params = function
            .params
            .iter()
            .map(|(name, ty)| ApiParam {
                name: name.clone(),
                ts: camel(name),
                docs: None,
                shape: shape(&resolve, &names, ty),
            })
            .collect();
        let (ok, err) = match &function.result {
            None => (Shape::Unit, None),
            Some(Type::Id(id)) => match &resolve.types[*id].kind {
                TypeDefKind::Result(result) => (
                    result
                        .ok
                        .as_ref()
                        .map(|ty| shape(&resolve, &names, ty))
                        .unwrap_or(Shape::Unit),
                    result.err.as_ref().map(|ty| shape(&resolve, &names, ty)),
                ),
                _ => (shape(&resolve, &names, &Type::Id(*id)), None),
            },
            Some(ty) => (shape(&resolve, &names, ty), None),
        };
        functions.push(ApiFunction {
            name: name.clone(),
            docs: function.docs.contents.clone(),
            params,
            ok,
            err,
        });
    }

    let api = Api {
        package: package_name,
        version,
        interface: interface_name.clone(),
        docs: interface.docs.contents.clone(),
        types,
        functions,
    };

    let mut problems = Vec::new();
    for ty in &api.types {
        ty.shape.unsupported(&ty.ts, &mut problems);
    }
    for function in &api.functions {
        function.ok.unsupported(&function.name, &mut problems);
        if let Some(err) = &function.err {
            err.unsupported(&format!("{}.err", function.name), &mut problems);
        }
        for param in &function.params {
            param
                .shape
                .unsupported(&format!("{}({})", function.name, param.ts), &mut problems);
        }
    }
    if !problems.is_empty() {
        bail!(
            "{requested} uses constructs this generator does not model:\n  {}",
            problems.join("\n  ")
        );
    }

    Ok(api)
}

fn docs(resolve: &Resolve, type_id: TypeId) -> Option<String> {
    resolve.types[type_id].docs.contents.clone()
}

fn shape(resolve: &Resolve, names: &BTreeMap<TypeId, String>, ty: &Type) -> Shape {
    match ty {
        Type::Bool => Shape::Boolean,
        Type::U8 => Shape::U8,
        Type::U16 => Shape::U16,
        Type::U32 => Shape::U32,
        Type::U64 => Shape::U64,
        Type::S8 => Shape::S8,
        Type::S16 => Shape::S16,
        Type::S32 => Shape::S32,
        Type::S64 => Shape::S64,
        Type::F32 => Shape::F32,
        Type::F64 => Shape::F64,
        Type::Char => Shape::Char,
        Type::String => Shape::Str,
        Type::Id(id) => {
            if let Some(name) = names.get(id) {
                return Shape::Named(name.clone());
            }
            shape_id(resolve, names, *id)
        }
        Type::ErrorContext => Shape::Unsupported("error-context".into()),
    }
}

/// The body of a type declaration, never a reference to its own name.
fn shape_id(resolve: &Resolve, names: &BTreeMap<TypeId, String>, id: TypeId) -> Shape {
    match &resolve.types[id].kind {
        TypeDefKind::List(inner) => {
            if matches!(inner, Type::U8) {
                Shape::Bytes
            } else {
                Shape::List(Box::new(shape(resolve, names, inner)))
            }
        }
        TypeDefKind::Option(inner) => Shape::Optional(Box::new(shape(resolve, names, inner))),
        TypeDefKind::Tuple(tuple) => Shape::Tuple(
            tuple
                .types
                .iter()
                .map(|ty| shape(resolve, names, ty))
                .collect(),
        ),
        TypeDefKind::Result(result) => Shape::Result {
            ok: result
                .ok
                .as_ref()
                .map(|ty| Box::new(shape(resolve, names, ty))),
            err: result
                .err
                .as_ref()
                .map(|ty| Box::new(shape(resolve, names, ty))),
        },
        TypeDefKind::Record(record) => Shape::Record(
            record
                .fields
                .iter()
                .map(|field| ApiField {
                    name: field.name.clone(),
                    ts: camel(&field.name),
                    docs: field.docs.contents.clone(),
                    shape: shape(resolve, names, &field.ty),
                })
                .collect(),
        ),
        TypeDefKind::Enum(enumeration) => Shape::Enum(
            enumeration
                .cases
                .iter()
                .map(|case| ApiCase {
                    name: case.name.clone(),
                    docs: case.docs.contents.clone(),
                    payload: None,
                })
                .collect(),
        ),
        TypeDefKind::Variant(variant) => Shape::Variant(
            variant
                .cases
                .iter()
                .map(|case| ApiCase {
                    name: case.name.clone(),
                    docs: case.docs.contents.clone(),
                    payload: case.ty.as_ref().map(|ty| shape(resolve, names, ty)),
                })
                .collect(),
        ),
        TypeDefKind::Flags(flags) => {
            Shape::Flags(flags.flags.iter().map(|flag| flag.name.clone()).collect())
        }
        TypeDefKind::Type(alias) => shape(resolve, names, alias),
        TypeDefKind::Resource | TypeDefKind::Handle(_) => Shape::Handle,
        other => Shape::Unsupported(other.as_str().to_owned()),
    }
}

/// `updated-at-ms` -> `updatedAtMs`.
pub fn camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for ch in name.chars() {
        if ch == '-' || ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// `surface-import` -> `SurfaceImport`.
pub fn pascal(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = true;
    for ch in name.chars() {
        if ch == '-' || ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}
