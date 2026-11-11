//! Spore package format: manifest, lock, deterministic tar, PGP sidecar.
//!
//! A Spore is a module (WASM), a UI (Lua), and a manifest that names every
//! asset and grant. Soil builds and verifies the package. It does not publish,
//! host, or resolve: distribution is a tar over IPFS, and trust roots come from
//! Mycelium, which binds a PGP key to a user or accepts an external keyring.
//!
//! ```text
//! markdown/
//!   spore.toml
//!   module.wasm
//!   ui/editor.lua
//!   assets/site-map.png
//!   signatures/<keyid>.asc   (excluded from the lock and the tar)
//!   spore.lock               (generated, included in the tar)
//! ```
//!
//! The lock is the package identity: sorted `sha256 size path` lines plus a
//! digest over that canonical text. Sign the tar; verify the tar, then the lock.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

pub mod identity;
pub mod lease;
pub mod manual;
pub mod publisher;

pub const SCHEMA: u32 = 1;
pub const LOCK_NAME: &str = "spore.lock";
pub const SIGNATURE_DIR: &str = "signatures";
pub const LOCK_HEADER: &str = "spore-lock 1";

/// Largest package Soil will read or write.
pub const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;

/// Largest single file inside a package.
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Longest listing paragraph a manifest may carry.
pub const MAX_DESCRIPTION_CHARS: usize = 1000;
/// Most synopsis lines a manual may declare.
pub const MAX_USAGE_LINES: usize = 8;
/// Most keywords a listing may carry.
pub const MAX_KEYWORDS: usize = 12;
/// Most screenshots a listing may carry.
pub const MAX_SCREENSHOTS: usize = 10;
/// Longest screenshot caption (alt text).
pub const MAX_CAPTION_CHARS: usize = 200;

#[derive(Debug)]
pub enum Error {
    Io(String),
    Toml(String),
    Invalid(String),
    UnsafePath(String),
    Lock(String),
    Signature(String),
    Tool(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "io: {message}"),
            Self::Toml(message) => write!(f, "manifest: {message}"),
            Self::Invalid(message) => write!(f, "invalid spore: {message}"),
            Self::UnsafePath(path) => write!(f, "unsafe path: {path}"),
            Self::Lock(message) => write!(f, "lock: {message}"),
            Self::Signature(message) => write!(f, "signature: {message}"),
            Self::Tool(message) => write!(f, "tool: {message}"),
        }
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

// ---- manifest -------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub spore: SporeMeta,
    pub engine: Engine,
    /// Absent for a Spore that is a composition rather than a program: chrome,
    /// the gallery, the storefront. `ui.entry` is then the whole of it, and any
    /// host capability it might want arrives with a module later.
    #[serde(default)]
    pub module: Option<Module>,
    /// The Lua entrypoint.
    ///
    /// Optional, and a first-class entrypoint in its own right: the host JITs
    /// this file, so a Spore does not need a wasm build to exist. A package
    /// with neither a module nor a UI nor a chrome region does nothing, and
    /// validation refuses it.
    #[serde(default)]
    pub ui: Option<Ui>,
    pub launch: Launch,
    /// Build-time dependencies and the exact version each was built against.
    ///
    /// A composed artifact pins its imports by construction, so a range would be
    /// a promise the bytes cannot keep. Recording the exact version is what lets
    /// a reviewer see which SDK produced the artifact, and lets a builder refuse
    /// to compose against a different one.
    #[serde(default)]
    pub deps: BTreeMap<String, String>,
    #[serde(default)]
    pub grants: BTreeMap<String, String>,
    #[serde(default)]
    pub assets: BTreeMap<String, Asset>,
    /// Declared absences. Reviewed like grants: intent is written down.
    #[serde(default)]
    pub denied: BTreeMap<String, Vec<String>>,
    /// Declares a host-system surface. The host must still allowlist the exact
    /// Spore id and signed digest; this flag is not a grant.
    #[serde(default)]
    pub system: Option<System>,
    /// This Spore's opt-in anycast claim: the name and service kind it asks to
    /// be reachable under. Absent means the Spore is local-only, which is the
    /// default; a claim is only honoured when the package's publisher grant
    /// authorizes it.
    #[serde(default)]
    pub anycast: Option<Anycast>,
    /// The names this Spore answers on, and what it will accept there.
    ///
    /// A claim is routing, not privilege. Claiming `contacts.myc` says where
    /// requests arrive; it says nothing about what the Spore may do with them,
    /// which stays in `[grants]`. The host refuses two live claims on one name
    /// unless an operator pins the name, because a silently stolen name is the
    /// failure that never shows up in a unit test.
    #[serde(default)]
    pub http: Option<Http>,
    /// Chrome regions this Spore draws, when it draws any.
    #[serde(default)]
    pub chrome: Option<Chrome>,
    /// Launcher identity: a mark and the colour it is drawn in.
    #[serde(default)]
    pub presentation: Option<Presentation>,
    /// What `man` prints: synopsis lines and an authored body.
    #[serde(default)]
    pub docs: Option<Docs>,
    /// What a public listing may show. Every rich artifact is a CID: a package
    /// carries the reference, never the bytes.
    #[serde(default)]
    pub catalog: Option<Catalog>,
    /// Declared actions: what this Spore offers to the action bus. A view that
    /// could perform an effect is a second authority; an action is the one
    /// place a capability is named, reviewed and routed.
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Agent skills this Spore ships. Each one is declared by path, so the lock
    /// and the tar cover it like any other asset: what teaches an agent to use
    /// the Spore is prose, and the prose has to travel with the package.
    #[serde(default)]
    pub skills: Vec<Skill>,
    /// Views this package ships, declared so a host can refuse one it cannot
    /// present before it opens anything.
    ///
    /// Empty is the default and the pre-declaration behaviour: the host renders
    /// the package's `[ui]`/`[module]` in whatever kind `[launch] surfaces`
    /// allows. Absent means every manifest written before surfaces existed
    /// keeps working untouched.
    #[serde(default)]
    pub surfaces: Vec<Surface>,
    /// Command-line programs this Spore installs into a shell's virtual PATH.
    ///
    /// A command is not an action: an action takes one JSON request, a command
    /// takes argv and stdin and answers on stdout with an exit code. Both are
    /// declared here because the package owns both faces; the shell decides
    /// which of them it exposes.
    #[serde(default)]
    pub commands: Vec<SporeCommand>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub title: String,
    /// Schema of the action's input, e.g. `mail/thread-ref@1`.
    pub input: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub callers: Vec<String>,
    #[serde(default)]
    pub expose: Vec<String>,
    #[serde(default)]
    pub idempotent: bool,
}

/// One program a Spore installs into a shell's virtual PATH.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SporeCommand {
    /// The name a shell resolves. A slug, so it cannot be mistaken for a path.
    pub name: String,
    pub summary: String,
    /// A `wasi:cli` command component, relative to the package root.
    pub module: String,
}

/// One agent skill: a `SKILL.md` whose frontmatter names it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skill {
    /// Registry identity; must match the frontmatter and the directory name.
    pub name: String,
    /// Path to the `SKILL.md`, relative to the package root.
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct System {
    pub role: String,
}

/// Man-page material that is not already implied by the manifest.
///
/// The page itself is generated: name, version, licence, grants and launch
/// surfaces are structured data, so they are never retyped here. What cannot
/// be derived is the synopsis and the prose, and those live in the package
/// because `man` must work offline.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Docs {
    /// Lines a shell would accept, including the Spore name and without a
    /// prompt.
    #[serde(default)]
    pub usage: Vec<String>,
    /// Markdown body shipped in the package. Small prose, not an asset.
    #[serde(default)]
    pub man: Option<String>,
}

/// The public listing a Spore is allowed to claim.
///
/// A store renders this; a package never carries the bytes it points at. The
/// thumbnail rule is deliberate: a listing without screenshots is a listing
/// nobody can judge, so the packager refuses to call it publishable.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub categories: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub screenshots: Vec<Screenshot>,
    /// CID of the rich listing record: long prose, changelog, provenance.
    #[serde(default)]
    pub record: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Screenshot {
    /// `ipfs://<cid>`. A path here would put the bytes back in the package.
    pub cid: String,
    /// Alt text. A screenshot nobody can describe is not accessible.
    pub caption: String,
}

/// A Spore that draws a slice of the shell instead of, or as well as, a window.
///
/// Chrome is a region, not a window: the host owns the extent, the input routing
/// and the fallback. A chrome Spore that fails to render must never leave a hole
/// in the desktop, so every region it claims has a built-in implementation that
/// takes over.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chrome {
    /// `top-bar`, `dock`, `control-center`.
    pub regions: Vec<String>,
    /// Composed Lua for a region, when the region's artifact is not the Spore's
    /// own `[ui] entry`.
    ///
    /// The common case is one region and one UI file, so this is usually empty:
    /// repeating `[ui] entry` under `[chrome.sources]` only creates two places
    /// that can disagree. It stays for the package that ships a distinct
    /// composed artifact per region. The host still does not compose at runtime;
    /// whichever path wins is what the lock covers.
    #[serde(default)]
    pub sources: BTreeMap<String, String>,
    /// Logical height this package authors the region at.
    ///
    /// Absent means the host's default. The number is declared rather than
    /// inferred because the host has to reserve the region *before* the package
    /// runs: it draws the built-in fallback at this height, then hands over. A
    /// package that disagrees with the fallback is a package that shifts the
    /// desktop when it loads, which is why the shell reads this instead of
    /// carrying a constant of its own.
    #[serde(default)]
    pub height: Option<u32>,
}

impl Chrome {
    /// The height the host reserves for this package's region.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height.unwrap_or(chrome::DEFAULT_HEIGHT)
    }
}

/// Shell metrics that belong to the region contract rather than to a renderer.
///
/// These lived in a graphics crate, which made the shell depend on a drawing
/// library to know how tall its own bar is. A chrome package may narrow them in
/// its manifest; the host uses the same numbers for the fallback it draws before
/// any package loads.
pub mod chrome {
    /// Logical height of the bar region.
    pub const DEFAULT_HEIGHT: u32 = 44;
    /// Outer padding inside the bar.
    pub const DEFAULT_PADDING: u32 = 8;
    /// Label size in the bar, in logical pixels.
    pub const DEFAULT_TEXT_SIZE: u32 = 14;
    /// Height of an interactive control in the bar; the whole row, so the target
    /// is the row rather than the glyph.
    pub const DEFAULT_CONTROL_HEIGHT: u32 = DEFAULT_HEIGHT;
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presentation {
    /// One or two characters. A monogram needs no asset and cannot go missing.
    pub badge: String,
    /// `#rrggbb`, the app's own colour rather than a theme role.
    pub color: String,
    /// CID of a richer mark drawn by a store or a launcher that has fetched it.
    /// Never a path: the fallback badge is what works with no network.
    #[serde(default)]
    pub icon: Option<String>,
}

/// Categories a listing may claim. A fixed vocabulary is what makes browsing
/// and ranking comparable across publishers; an unlisted name is a typo or a
/// new category, and both should arrive as a change to this list.
pub const CATEGORIES: [&str; 12] = [
    "communication",
    "developer-tools",
    "education",
    "games",
    "graphics",
    "media",
    "network",
    "productivity",
    "science",
    "security",
    "system",
    "utilities",
];

/// Regions a Spore may claim. An unlisted name is a typo or a claim the host
/// cannot honour, and both are better refused at install than discovered later.
pub const CHROME_REGIONS: [&str; 8] = [
    "top-bar",
    "dock",
    "launcher",
    "switcher",
    "control-center",
    // Transient surfaces: the shell's own voice. Kept as regions rather than
    // windows so the host still owns where they appear and when they dismiss.
    "notifications",
    "attention",
    // The frame around a window a Spore owns: title bar, grip, resize edges.
    "window",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SporeMeta {
    pub id: String,
    /// The package directory and registry slug. Stable and lower-case; it is
    /// identity, not a label.
    pub name: String,
    /// How the app names itself in a window title, a Dock, or a launcher row.
    /// Optional so every manifest written before this field keeps working; a
    /// host falls back to title-casing `name`.
    #[serde(default)]
    pub title: Option<String>,
    pub version: String,
    pub license: String,
    pub summary: String,
    /// One paragraph for a listing or `spore info`. `summary` is the row;
    /// this is the paragraph under it.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Engine {
    pub soil: String,
    pub target: String,
    #[serde(default)]
    pub abi: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub path: String,
    /// Filled by `lock` when absent; checked when present.
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    pub entry: String,
    #[serde(default = "default_true")]
    pub jit: bool,
    #[serde(default = "default_true")]
    pub hot_reload: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    /// The surface a bare launch opens.
    ///
    /// A kind from `surfaces` (`canvas`, `native`, `web`) while the Spore
    /// declares no `[[surfaces]]`, and the name of a declared surface when it
    /// does. One field because it is one fact: what the host opens when the
    /// caller names nothing. This is `default-surface` on the wire.
    pub default: String,
    /// Kinds this package can be rendered as.
    ///
    /// Required while the Spore declares no `[[surfaces]]`, because the kind
    /// list is then the whole statement of what it can be launched as. Once
    /// surfaces carry their own kind this is only an outer bound: every
    /// declared kind must appear here, and a package whose one artifact renders
    /// on more engines than it names surfaces for may list more.
    #[serde(default)]
    pub surfaces: Vec<String>,
    /// The composed Lua module Canvas opens, when the launcher starts a view
    /// rather than the module. Composition writes it into the runtime modules
    /// directory; the entry point is a window, not a file in the package.
    #[serde(default)]
    pub canvas_module: Option<String>,
}

/// A Spore's opt-in claim on an anycast name.
///
/// This is the claim only. Whether the claim is allowed to route is decided by
/// the publisher grant in [`publisher`], never by this table: a manifest can be
/// edited by anyone, a signature cannot.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anycast {
    /// Service name the claim is published under, e.g. `confluence`.
    pub name: String,
    /// Service kind the router joins on, matching the gateway's route table.
    pub kind: String,
    /// Application protocols this Spore answers on. `soil/http/1` is HTTP.
    #[serde(default)]
    pub alpns: Vec<String>,
    /// Lease lifetime in seconds. Mycelium drops a lease after 300, so a
    /// longer claim could never be honoured; it is rejected rather than
    /// silently clamped, because a clamp would hide a broken renewer.
    #[serde(default = "default_lease_secs")]
    pub ttl_secs: u64,
}

pub const MAX_LEASE_SECS: u64 = 300;

/// A command name: a slug a shell resolves, never a path or a flag.
fn command_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        // It must start like a word, not like a flag: `--force` in a PATH is a
        // name a shell would hand to the wrong program.
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A hostname a vhost could be granted: lowercase labels, no scheme, no port.
fn http_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && !value.starts_with('.')
        && !value.ends_with('.')
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
}

/// Most hostnames one Spore may claim. A Spore serving a handful of names is
/// ordinary; one claiming dozens is a registry, not a Spore.
pub const MAX_HTTP_HOSTS: usize = 16;

/// Most routes one Spore may declare.
pub const MAX_HTTP_ROUTES: usize = 32;

/// Largest request body a claimed route may accept.
pub const MAX_HTTP_BODY: usize = 1024 * 1024;

/// Methods a route may name.
///
/// A closed set: an unknown method is a typo or a fresh idea, and both should
/// arrive as a change to this list rather than as a route that silently never
/// matches.
pub const HTTP_METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Where a Spore answers, declared by the Spore so an install needs no sidecar
/// config.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Http {
    /// Hostnames, as a vhost would match them: no scheme, no port, no path.
    pub hosts: Vec<String>,
    #[serde(default = "default_max_body")]
    pub max_body: usize,
    #[serde(default)]
    pub origin_auth: OriginAuth,
    pub routes: Vec<HttpRoute>,
}

/// One URL prefix this Spore will service, and the methods it accepts there.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRoute {
    pub methods: Vec<String>,
    /// Prefix, beginning with `/`. A grant is a prefix rather than a pattern:
    /// the vhost already decides which Spore sees the request.
    pub path: String,
}

/// Transport truth for a listener.
///
/// Most Spores verify a provider's own signature. A declared terminator says
/// the reverse: only that peer may assert a verified identity, because it saw
/// the client certificate and this process did not.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OriginAuth {
    #[default]
    ProviderSignature,
    TrustedTerminator,
}

const fn default_max_body() -> usize {
    64 * 1024
}

const fn default_lease_secs() -> u64 {
    240
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub path: String,
    pub media_type: String,
}

const fn default_true() -> bool {
    true
}

/// The kind names a `[launch] surfaces` list is drawn from. `native` is here
/// because it has always been launchable, not because a surface can declare it:
/// see [`SurfaceKind`].
pub const SURFACES: [&str; 3] = ["web", "native", "canvas"];

/// The page a `web` surface opens when its manifest does not say otherwise.
pub const DEFAULT_WEB_ENTRY: &str = "web/index.html";

/// A view a package ships, declared in `[[surfaces]]`.
///
/// A Spore declares a surface when the host could not guess it: a bundled web
/// page, or a Canvas artifact that is not the package's own `[ui] entry`. The
/// declaration is what lets a host answer "this Spore needs a browser and I
/// have none" before it opens anything.
///
/// The entry point is a path, not a content id. The package is the unit of
/// identity and provenance — `spore.lock` and the package digest already cover
/// every byte the package ships — so a per-asset content id would be a second
/// version of the same fact, kept in a second place, drifting against the
/// first. Do not add one.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Surface {
    /// The name `launch(id, surface)` takes and [`Launch::default`] may name.
    pub name: String,
    pub kind: SurfaceKind,
    /// Entry point relative to the package root, and the whole of the default:
    /// `web/index.html` for a [`SurfaceKind::Web`] surface, the package's own
    /// `[ui] entry` for a canvas one.
    ///
    /// Written down only when the artifact is not the conventional one, so a
    /// Spore that ships `web/index.html` says `kind = "web"` and nothing else.
    #[serde(default)]
    pub entry: Option<String>,
    /// Engine or host capability this surface needs, if any.
    ///
    /// The kind is already a requirement, so most surfaces need nothing here.
    /// It exists for the case where one host can present a kind and another
    /// cannot — a specific browser engine, a particular decoder.
    #[serde(default)]
    pub requires: Option<String>,
}

impl Surface {
    /// The artifact this surface opens, relative to the package root.
    ///
    /// A declared `entry` wins; otherwise the kind's convention, which is
    /// `web/index.html` for a web page and the package's own [`Ui`] entry for a
    /// Canvas one. Resolved here rather than in each host so there is one
    /// answer.
    #[must_use]
    pub fn entry<'a>(&'a self, ui: Option<&'a Ui>) -> Option<&'a str> {
        match (&self.entry, self.kind) {
            (Some(entry), _) => Some(entry),
            (None, SurfaceKind::Web) => Some(DEFAULT_WEB_ENTRY),
            (None, SurfaceKind::Canvas) => ui.map(|ui| ui.entry.as_str()),
        }
    }
}

/// How a host presents a surface.
///
/// `canvas` is the host's own Lua renderer. `web` needs a browser view — the
/// `browser` kind in `soil:surface-import` — which a host is allowed not to
/// have. Native rendering has no variant: it is the default, it is what
/// `[module]` and `[ui]` already are, and a package that needs nothing extra
/// should not have to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SurfaceKind {
    Canvas,
    Web,
}

impl SurfaceKind {
    /// Every kind, in the order a host lists them when it refuses a request.
    pub const ALL: [Self; 2] = [Self::Canvas, Self::Web];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Canvas => "canvas",
            Self::Web => "web",
        }
    }

    /// The kind a surface declaration may carry.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// The kind a `[launch] surfaces` entry names.
    ///
    /// `native` is `None` on purpose: it is the host rendering the package's own
    /// artifacts, so there is no capability a host could be missing.
    fn from_launch_kind(value: &str) -> Option<Option<Self>> {
        match value {
            "native" => Some(None),
            other => Self::parse(other).map(Some),
        }
    }

    #[must_use]
    pub fn names() -> String {
        Self::ALL.map(Self::as_str).join(", ")
    }
}

impl fmt::Display for SurfaceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SurfaceKind {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown surface kind {raw:?}; a surface kind is one of {}",
                Self::names()
            ))
        })
    }
}

/// What a host opens for one `launch(id, surface)` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchTarget<'a> {
    /// The name the host asked for; what `launched.surface` echoes back.
    pub surface: &'a str,
    /// The renderer that presents it. `None` for a legacy `native` launch, where
    /// the host draws the package itself.
    pub kind: Option<SurfaceKind>,
    /// Package-relative entry point. `None` when the host renders the package's
    /// own `[ui]` artifact, which is what a Spore that declares no surfaces
    /// means.
    pub entry: Option<&'a str>,
}

/// Why a launch request cannot be honoured.
///
/// These are the two `soil:spore/catalog.error` members a host needs here;
/// [`LaunchError::code`] is that mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// The package declares no surface by that name.
    NotFound {
        requested: String,
        declared: Vec<String>,
    },
    /// The surface exists, but this host cannot present its kind.
    Unsupported {
        surface: String,
        kind: SurfaceKind,
        host: Vec<SurfaceKind>,
    },
}

impl LaunchError {
    /// The `soil:spore/catalog.error` member this maps to.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not-found",
            Self::Unsupported { .. } => "unsupported",
        }
    }
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound {
                requested,
                declared,
            } => write!(
                f,
                "surface {requested:?} is not declared; this Spore declares {declared:?}"
            ),
            Self::Unsupported {
                surface,
                kind,
                host,
            } => write!(
                f,
                "surface {surface:?} needs a {kind} view, and this host presents only {host:?}"
            ),
        }
    }
}

impl std::error::Error for LaunchError {}

impl Manifest {
    pub fn parse(source: &str) -> Result<Self> {
        let manifest: Self =
            toml::from_str(source).map_err(|error| Error::Toml(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("spore.toml");
        let source = fs::read_to_string(&path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        let manifest = Self::parse(&source)?;
        manifest.check_skills(root)?;
        Ok(manifest)
    }

    /// Skills are files, so only `load` can check them: `parse` has no root.
    /// A declared skill that is missing, unnamed, or misnamed is a packaging
    /// error, not a runtime surprise.
    fn check_skills(&self, root: &Path) -> Result<()> {
        for skill in &self.skills {
            let file = root.join(&skill.path);
            let source = fs::read_to_string(&file)
                .map_err(|error| Error::Io(format!("{}: {error}", file.display())))?;
            let (name, description) = skill_frontmatter(&source).ok_or_else(|| {
                Error::Invalid(format!(
                    "{} needs `---` frontmatter with name and description",
                    file.display()
                ))
            })?;
            if name != skill.name {
                return Err(Error::Invalid(format!(
                    "skill {:?} declares name {name:?} in its frontmatter",
                    skill.name
                )));
            }
            if description.trim().is_empty() {
                return Err(Error::Invalid(format!(
                    "skill {name:?} needs a description"
                )));
            }
            let dirname = file
                .parent()
                .and_then(|parent| parent.file_name())
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            if dirname != name {
                return Err(Error::Invalid(format!(
                    "skill {name:?} must live in a directory of the same name, not {dirname:?}"
                )));
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(Error::Invalid(format!(
                "schema {} is not {SCHEMA}",
                self.schema
            )));
        }
        if !identifier(&self.spore.id) || !self.spore.id.contains('.') {
            return Err(Error::Invalid(format!(
                "id {:?} must be a stable reverse-DNS identifier",
                self.spore.id
            )));
        }
        for (label, value) in [
            ("name", &self.spore.name),
            ("version", &self.spore.version),
            ("license", &self.spore.license),
            ("summary", &self.spore.summary),
        ] {
            if value.trim().is_empty() {
                return Err(Error::Invalid(format!("{label} must not be empty")));
            }
        }
        if !version(&self.spore.version) {
            return Err(Error::Invalid(format!(
                "version {:?} is not semver-ish",
                self.spore.version
            )));
        }
        for skill in &self.skills {
            if !identifier(&skill.name) {
                return Err(Error::Invalid(format!(
                    "skill name {:?} must be lowercase letters, digits, - or _",
                    skill.name
                )));
            }
            let path = safe_relative(&skill.path)?;
            if path
                .file_name()
                .map(|name| name.to_string_lossy() != "SKILL.md")
                != Some(true)
            {
                return Err(Error::Invalid(format!(
                    "skill {:?} must point at a SKILL.md, not {:?}",
                    skill.name, skill.path
                )));
            }
        }

        match &self.module {
            Some(module) => {
                safe_relative(&module.path)?;
                if !module.path.ends_with(".wasm") {
                    return Err(Error::Invalid("module.path must end in .wasm".into()));
                }
            }
            // A Spore without a module still has to do something. Any entrypoint
            // counts, and Lua is one of them: the host JITs `[ui] entry`, so a
            // Lua-only Spore is a program, not a second-class attachment to a
            // wasm build. `chrome` counts because a host draws the region.
            None if self.ui.is_none() && self.chrome.is_none() => {
                return Err(Error::Invalid(
                    "a Spore needs an entrypoint: a module, a Lua UI, or a chrome region".into(),
                ));
            }
            None => {}
        }
        if let Some(ui) = &self.ui {
            safe_relative(&ui.entry)?;
            if !ui.entry.ends_with(".lua") {
                return Err(Error::Invalid("ui.entry must end in .lua".into()));
            }
        }

        if self.surfaces.is_empty() {
            // No declarations: the kind list is the whole statement of what
            // this package can be launched as, exactly as before.
            if self.launch.surfaces.is_empty() {
                return Err(Error::Invalid("launch.surfaces must not be empty".into()));
            }
            for surface in &self.launch.surfaces {
                if !SURFACES.contains(&surface.as_str()) {
                    return Err(Error::Invalid(format!(
                        "unknown surface {surface:?}: launch.surfaces is drawn from {SURFACES:?}"
                    )));
                }
            }
            if !self.launch.surfaces.contains(&self.launch.default) {
                return Err(Error::Invalid(format!(
                    "launch.default {:?} is not in launch.surfaces {:?}",
                    self.launch.default, self.launch.surfaces
                )));
            }
        } else {
            let mut seen = BTreeSet::new();
            for surface in &self.surfaces {
                if !identifier(&surface.name) {
                    return Err(Error::Invalid(format!(
                        "surface name {:?} must be lowercase letters, digits, - or _",
                        surface.name
                    )));
                }
                if !seen.insert(surface.name.as_str()) {
                    return Err(Error::Invalid(format!(
                        "surface {:?} is declared twice",
                        surface.name
                    )));
                }
                let Some(entry) = surface.entry(self.ui.as_ref()) else {
                    return Err(Error::Invalid(format!(
                        "surface {:?} is {} but this Spore has no [ui] entry to present; name one with `entry`",
                        surface.name, surface.kind
                    )));
                };
                safe_relative(entry)?;
                let expected = match surface.kind {
                    SurfaceKind::Canvas => ".lua",
                    SurfaceKind::Web => ".html",
                };
                if !entry.ends_with(expected) {
                    return Err(Error::Invalid(format!(
                        "surface {:?} is {}, so its entry {entry:?} must end in {expected}",
                        surface.name, surface.kind
                    )));
                }
                if let Some(requires) = &surface.requires
                    && !identifier(requires)
                {
                    return Err(Error::Invalid(format!(
                        "surface {:?} requires {requires:?}, which must be lowercase letters, digits, - or _",
                        surface.name
                    )));
                }
                // A kind list, when given, is an outer bound: one artifact can
                // render on more engines than the package names surfaces for,
                // never fewer.
                let kind = surface.kind.as_str();
                if !self.launch.surfaces.is_empty()
                    && !self.launch.surfaces.iter().any(|listed| listed == kind)
                {
                    return Err(Error::Invalid(format!(
                        "surface {:?} is {kind}, which launch.surfaces {:?} does not list",
                        surface.name, self.launch.surfaces
                    )));
                }
            }
            // Declared surfaces are the authority for what opens, so the
            // default has to name one of them.
            let declared = self.surface_names();
            if !declared.contains(&self.launch.default.as_str()) {
                return Err(Error::Invalid(format!(
                    "launch.default {:?} names no declared surface; [[surfaces]] declares {declared:?}",
                    self.launch.default
                )));
            }
        }

        if let Some(system) = &self.system
            && !identifier(&system.role)
        {
            return Err(Error::Invalid(format!(
                "system role {:?} is not an identifier",
                system.role
            )));
        }

        for (name, version) in &self.deps {
            if !identifier(name) {
                return Err(Error::Invalid(format!(
                    "dependency {name:?} must be a lowercase identifier such as `fungos.ui`"
                )));
            }
            if !version_is_exact(version) {
                return Err(Error::Invalid(format!(
                    "dependency {name:?} = {version:?} must be one exact version, not a range: a composed artifact cannot float"
                )));
            }
        }

        if let Some(anycast) = &self.anycast {
            for (label, value) in [
                ("anycast.name", &anycast.name),
                ("anycast.kind", &anycast.kind),
            ] {
                if !lease_label(value) {
                    return Err(Error::Invalid(format!(
                        "{label} {value:?} must be lowercase [a-z0-9-_./:], at most 256 bytes"
                    )));
                }
            }
            if anycast.alpns.len() > 8 {
                return Err(Error::Invalid(
                    "anycast.alpns has more than 8 entries".into(),
                ));
            }
            for alpn in &anycast.alpns {
                if alpn.is_empty() || alpn.len() > 64 || !alpn.is_ascii() {
                    return Err(Error::Invalid(format!(
                        "anycast alpn {alpn:?} must be 1..=64 ascii bytes"
                    )));
                }
            }
            // Mycelium drops a lease older than 300 s and never renews it, so a
            // longer claim is a broken renewer, not a longer lease.
            if anycast.ttl_secs == 0 || anycast.ttl_secs > MAX_LEASE_SECS {
                return Err(Error::Invalid(format!(
                    "anycast.ttl_secs {} must be 1..={MAX_LEASE_SECS}",
                    anycast.ttl_secs
                )));
            }
        }

        if let Some(http) = &self.http {
            if self.module.is_none() {
                return Err(Error::Invalid(
                    "[http] needs a module: a Spore that answers on a name must be able to answer"
                        .into(),
                ));
            }
            if http.hosts.is_empty() || http.hosts.len() > MAX_HTTP_HOSTS {
                return Err(Error::Invalid(format!(
                    "[http] needs 1..={MAX_HTTP_HOSTS} hosts"
                )));
            }
            for host in &http.hosts {
                if !http_host(host) {
                    return Err(Error::Invalid(format!(
                        "http host {host:?} must be a lowercase hostname, no scheme, port or path"
                    )));
                }
            }
            if http.max_body == 0 || http.max_body > MAX_HTTP_BODY {
                return Err(Error::Invalid(format!(
                    "http.max_body {} must be 1..={MAX_HTTP_BODY}",
                    http.max_body
                )));
            }
            if http.routes.is_empty() || http.routes.len() > MAX_HTTP_ROUTES {
                return Err(Error::Invalid(format!(
                    "[http] needs 1..={MAX_HTTP_ROUTES} routes"
                )));
            }
            for route in &http.routes {
                if !route.path.starts_with('/')
                    || route.path.len() > 128
                    || route
                        .path
                        .chars()
                        .any(|c| c.is_whitespace() || c.is_control())
                {
                    return Err(Error::Invalid(format!(
                        "route path {:?} must start with `/`, be at most 128 bytes, and contain no whitespace",
                        route.path
                    )));
                }
                if route.methods.is_empty() {
                    return Err(Error::Invalid(format!(
                        "route {} names no methods",
                        route.path
                    )));
                }
                for method in &route.methods {
                    if !HTTP_METHODS.contains(&method.as_str()) {
                        return Err(Error::Invalid(format!(
                            "route {} names method {method:?}; allowed: {}",
                            route.path,
                            HTTP_METHODS.join(", ")
                        )));
                    }
                }
            }
        }

        let mut command_names = std::collections::BTreeSet::new();
        for command in &self.commands {
            if !command_slug(&command.name) {
                return Err(Error::Invalid(format!(
                    "command name {:?} must be lowercase [a-z0-9-] and at most 32 bytes",
                    command.name
                )));
            }
            if !command_names.insert(command.name.clone()) {
                return Err(Error::Invalid(format!(
                    "command {:?} is declared twice",
                    command.name
                )));
            }
            if command.summary.trim().is_empty() {
                return Err(Error::Invalid(format!(
                    "command {:?} has no summary, so a shell cannot list it",
                    command.name
                )));
            }
            safe_relative(&command.module)?;
            if !command.module.ends_with(".wasm") {
                return Err(Error::Invalid(format!(
                    "command {:?} module must end in .wasm",
                    command.name
                )));
            }
        }

        for capability in self.grants.keys() {
            if !grants(capability) {
                return Err(Error::Invalid(format!(
                    "grant {capability:?} is not a soil:/wasi:/fungos: capability"
                )));
            }
        }
        if let Some(chrome) = &self.chrome {
            if chrome.regions.is_empty() {
                return Err(Error::Invalid("chrome needs at least one region".into()));
            }
            for region in &chrome.regions {
                if !CHROME_REGIONS.contains(&region.as_str()) {
                    return Err(Error::Invalid(format!(
                        "chrome region {region:?} is not one of {CHROME_REGIONS:?}"
                    )));
                }
                let Some(source) = self.region_source(region) else {
                    return Err(Error::Invalid(format!(
                        "chrome region {region:?} has no source: give the Spore a [ui] entry or name one in [chrome.sources]"
                    )));
                };
                safe_relative(source)?;
                if !source.ends_with(".lua") {
                    return Err(Error::Invalid(format!(
                        "chrome source {source:?} must be a .lua artifact"
                    )));
                }
            }
        }
        if let Some(presentation) = &self.presentation {
            let glyphs = presentation.badge.chars().count();
            if !(1..=2).contains(&glyphs) {
                return Err(Error::Invalid(
                    "presentation.badge must be one or two characters".into(),
                ));
            }
            let hex = presentation.color.trim_start_matches('#');
            if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(Error::Invalid("presentation.color must be #rrggbb".into()));
            }
            if let Some(icon) = &presentation.icon {
                reference(icon)?;
            }
        }
        if let Some(description) = &self.spore.description {
            if description.trim().is_empty() {
                return Err(Error::Invalid(
                    "spore.description must not be empty when present".into(),
                ));
            }
            if description.chars().count() > MAX_DESCRIPTION_CHARS {
                return Err(Error::Invalid(format!(
                    "spore.description is longer than {MAX_DESCRIPTION_CHARS} characters"
                )));
            }
        }
        if let Some(docs) = &self.docs {
            if docs.usage.len() > MAX_USAGE_LINES {
                return Err(Error::Invalid(format!(
                    "docs.usage has more than {MAX_USAGE_LINES} lines"
                )));
            }
            for line in &docs.usage {
                if line.trim().is_empty() || line.chars().any(char::is_control) {
                    return Err(Error::Invalid(
                        "docs.usage entries must be plain lines".into(),
                    ));
                }
            }
            if let Some(man) = &docs.man {
                safe_relative(man)?;
                if !man.ends_with(".md") {
                    return Err(Error::Invalid("docs.man must name a Markdown file".into()));
                }
            }
        }
        if let Some(catalog) = &self.catalog {
            if catalog.categories.is_empty() {
                return Err(Error::Invalid(
                    "catalog.categories must not be empty".into(),
                ));
            }
            let mut seen = std::collections::BTreeSet::new();
            for category in &catalog.categories {
                if !CATEGORIES.contains(&category.as_str()) {
                    return Err(Error::Invalid(format!(
                        "catalog category {category:?} is not one of {CATEGORIES:?}"
                    )));
                }
                if !seen.insert(category) {
                    return Err(Error::Invalid(format!(
                        "catalog category {category:?} is repeated"
                    )));
                }
            }
            if catalog.keywords.len() > MAX_KEYWORDS {
                return Err(Error::Invalid(format!(
                    "catalog.keywords has more than {MAX_KEYWORDS} entries"
                )));
            }
            for word in &catalog.keywords {
                if !keyword(word) {
                    return Err(Error::Invalid(format!(
                        "catalog keyword {word:?} must be lowercase letters, digits or dashes"
                    )));
                }
            }
            if catalog.screenshots.len() > MAX_SCREENSHOTS {
                return Err(Error::Invalid(format!(
                    "catalog.screenshots has more than {MAX_SCREENSHOTS} entries"
                )));
            }
            for screenshot in &catalog.screenshots {
                reference(&screenshot.cid)?;
                if screenshot.caption.trim().is_empty()
                    || screenshot.caption.chars().count() > MAX_CAPTION_CHARS
                {
                    return Err(Error::Invalid(format!(
                        "catalog screenshot captions must be 1..={MAX_CAPTION_CHARS} characters"
                    )));
                }
            }
            if let Some(record) = &catalog.record {
                reference(record)?;
            }
        }
        for action in &self.actions {
            if !identifier(&action.id) {
                return Err(Error::Invalid(format!(
                    "action id {:?} is not an identifier",
                    action.id
                )));
            }
            if action.title.trim().is_empty() || action.input.trim().is_empty() {
                return Err(Error::Invalid(format!(
                    "action {:?} needs a title and an input schema",
                    action.id
                )));
            }
            for capability in &action.requires {
                if !grants(capability) {
                    return Err(Error::Invalid(format!(
                        "action {:?} requires {capability:?}, which is not a capability",
                        action.id
                    )));
                }
            }
        }
        for (name, asset) in &self.assets {
            if !identifier(name) {
                return Err(Error::Invalid(format!("asset name {name:?}")));
            }
            safe_relative(&asset.path)?;
            if asset.media_type.trim().is_empty() || !asset.media_type.contains('/') {
                return Err(Error::Invalid(format!("asset {name:?} needs a media type")));
            }
        }
        Ok(())
    }

    /// Every file the manifest declares explicitly.
    pub fn declared_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(ui) = &self.ui {
            paths.push(ui.entry.clone());
        }
        if let Some(module) = &self.module {
            paths.push(module.path.clone());
        }
        if let Some(chrome) = &self.chrome {
            for region in &chrome.regions {
                if let Some(source) = chrome.sources.get(region) {
                    paths.push(source.clone());
                }
            }
        }
        paths.extend(self.assets.values().map(|asset| asset.path.clone()));
        paths.extend(self.skills.iter().map(|skill| skill.path.clone()));
        paths.extend(self.commands.iter().map(|command| command.module.clone()));
        if let Some(man) = self.docs.as_ref().and_then(|docs| docs.man.clone()) {
            paths.push(man);
        }
        for surface in &self.surfaces {
            if let Some(entry) = surface.entry(self.ui.as_ref()) {
                paths.push(entry.to_owned());
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        paths
            .into_iter()
            .filter(|path| safe_relative(path).is_ok())
            .map(PathBuf::from)
            .filter(|path| seen.insert(path.clone()))
            .collect()
    }

    /// Every file the package ships, relative to the package root.
    ///
    /// A package is what the Spore needs to run: its manifest, its Lua UI, its
    /// module, its declared assets and its manual. Build inputs (`src/`, `wit/`,
    /// `Cargo.*`), tests, and repo documentation are not part of it -- shipping
    /// them would put source and screenshots in every download for no runtime
    /// benefit. `spore.lock` is added when packing.
    pub fn package_paths(&self) -> Vec<PathBuf> {
        let mut paths = self.declared_paths();
        paths.push(PathBuf::from("spore.toml"));
        paths.sort();
        paths.dedup();
        paths
    }

    /// The declared surface `name`, if this Spore declares one.
    #[must_use]
    pub fn surface(&self, name: &str) -> Option<&Surface> {
        self.surfaces.iter().find(|surface| surface.name == name)
    }

    /// The names a launch may ask for: declared surfaces when there are any,
    /// the `[launch] surfaces` kinds when there are none.
    #[must_use]
    pub fn surface_names(&self) -> Vec<&str> {
        if self.surfaces.is_empty() {
            self.launch.surfaces.iter().map(String::as_str).collect()
        } else {
            self.surfaces
                .iter()
                .map(|surface| surface.name.as_str())
                .collect()
        }
    }

    /// What a host opens for `requested`, given the kinds it can present.
    ///
    /// This is the decision `soil:spore/catalog.launch` makes before it launches
    /// anything. `host` is what this host can actually render, so a request for
    /// a [`SurfaceKind::Web`] surface on a host with no browser view is
    /// [`LaunchError::Unsupported`] — and never a launch that cannot present.
    ///
    /// A Spore that declares no surfaces keeps the old behaviour: the kinds in
    /// `[launch] surfaces` are the names, so this changes nothing about how an
    /// existing package launches.
    pub fn resolve_launch<'a>(
        &'a self,
        requested: &'a str,
        host: &[SurfaceKind],
    ) -> std::result::Result<LaunchTarget<'a>, LaunchError> {
        let Some(surface) = self.surface(requested) else {
            let launchable = self.surfaces.is_empty()
                && self.launch.surfaces.iter().any(|kind| kind == requested);
            if !launchable {
                return Err(LaunchError::NotFound {
                    requested: requested.to_owned(),
                    declared: self
                        .surface_names()
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                });
            }
            let kind = SurfaceKind::from_launch_kind(requested).flatten();
            if let Some(kind) = kind
                && !host.contains(&kind)
            {
                return Err(LaunchError::Unsupported {
                    surface: requested.to_owned(),
                    kind,
                    host: host.to_vec(),
                });
            }
            return Ok(LaunchTarget {
                surface: requested,
                kind,
                entry: None,
            });
        };

        if !host.contains(&surface.kind) {
            return Err(LaunchError::Unsupported {
                surface: surface.name.clone(),
                kind: surface.kind,
                host: host.to_vec(),
            });
        }
        Ok(LaunchTarget {
            surface: &surface.name,
            kind: Some(surface.kind),
            entry: surface.entry(self.ui.as_ref()),
        })
    }

    /// The Lua a host loads for a chrome `region`.
    ///
    /// An explicit per-region artifact wins; otherwise the Spore's single `[ui]
    /// entry` is the region's Lua. One region plus one UI file is the common
    /// shape, and naming the same path twice only creates two places to disagree.
    #[must_use]
    pub fn region_source(&self, region: &str) -> Option<&str> {
        let chrome = self.chrome.as_ref()?;
        if !chrome.regions.iter().any(|claimed| claimed == region) {
            return None;
        }
        chrome
            .sources
            .get(region)
            .map(String::as_str)
            .or_else(|| self.ui.as_ref().map(|ui| ui.entry.as_str()))
    }
}

/// The two frontmatter fields a skill registry needs. Everything else in a
/// SKILL.md is prose, so it is not parsed here.
fn skill_frontmatter(source: &str) -> Option<(String, String)> {
    let mut lines = source.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut name = None;
    let mut description = None;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_string());
        }
        if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_string());
        }
    }
    Some((name?, description?))
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// One exact version: no `^`, `~`, `>=`, `*` or range separators.
fn version_is_exact(value: &str) -> bool {
    version(value) && !value.contains(['^', '~', '*', '>', '<', ' ', ','])
}

fn version(value: &str) -> bool {
    let core = value.split(['-', '+']).next().unwrap_or(value);
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn lease_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.:/".contains(&b))
}

fn grants(value: &str) -> bool {
    let Some((package, interface)) = value.split_once(':') else {
        return false;
    };
    matches!(package, "soil" | "wasi" | "fungos")
        && !interface.is_empty()
        && interface.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_' | '/')
        })
}

fn keyword(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A listing artifact is content-addressed or it is not a listing artifact.
///
/// Accepting a path here is how screenshots and logos end up inside the bundle
/// that moves around, so the only accepted form is `ipfs://<CIDv1 base32>`.
/// The bytes live wherever the CID resolves; the package carries the claim.
pub fn reference(value: &str) -> Result<()> {
    let Some(cid) = value.strip_prefix("ipfs://") else {
        return Err(Error::Invalid(format!(
            "{value:?} is not an ipfs:// reference; a store artifact is a CID, never a path"
        )));
    };
    let valid = (20..=128).contains(&cid.len())
        && cid.starts_with('b')
        && cid.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7'));
    if !valid {
        return Err(Error::Invalid(format!(
            "{value:?} is not an ipfs:// CIDv1 (base32) reference"
        )));
    }
    Ok(())
}

/// Resolve a manifest path without letting it leave the package.
pub fn safe_relative(path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.len() > 255 || path.starts_with('/') || path.contains('\\') {
        return Err(Error::UnsafePath(path.to_owned()));
    }
    if path.chars().any(char::is_control) {
        return Err(Error::UnsafePath(path.to_owned()));
    }
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return Err(Error::UnsafePath(path.to_owned())),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(Error::UnsafePath(path.to_owned()));
    }
    Ok(out)
}

// ---- lock -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lock {
    pub id: String,
    pub version: String,
    pub files: Vec<FileEntry>,
    pub digest: String,
}

impl Lock {
    pub fn new(id: &str, version: &str, mut files: Vec<FileEntry>) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let body = body(id, version, &files);
        Self {
            id: id.to_owned(),
            version: version.to_owned(),
            files,
            digest: hex(&Sha256::digest(body.as_bytes())),
        }
    }

    /// Canonical text, including the digest line.
    pub fn render(&self) -> String {
        let mut out = body(&self.id, &self.version, &self.files);
        out.push_str(&format!("digest {}\n", self.digest));
        out
    }

    pub fn parse(source: &str) -> Result<Self> {
        let mut id = None;
        let mut version = None;
        let mut files = Vec::new();
        let mut digest = None;

        for line in source.lines() {
            if line.is_empty() || line == LOCK_HEADER {
                continue;
            }
            let (key, rest) = line
                .split_once(' ')
                .ok_or_else(|| Error::Lock(format!("malformed line {line:?}")))?;
            match key {
                "spore" => id = Some(rest.to_owned()),
                "version" => version = Some(rest.to_owned()),
                "digest" => digest = Some(rest.to_owned()),
                "file" => {
                    let mut parts = rest.splitn(3, ' ');
                    let sha256 = parts.next().unwrap_or_default();
                    let size = parts.next().unwrap_or_default();
                    let path = parts.next().unwrap_or_default();
                    if sha256.len() != 64
                        || !sha256.chars().all(|c| c.is_ascii_hexdigit())
                        || path.is_empty()
                    {
                        return Err(Error::Lock(format!("malformed file line {line:?}")));
                    }
                    files.push(FileEntry {
                        path: path.to_owned(),
                        sha256: sha256.to_ascii_lowercase(),
                        size: size
                            .parse()
                            .map_err(|_| Error::Lock(format!("bad size {size:?}")))?,
                    });
                }
                other => return Err(Error::Lock(format!("unknown field {other:?}"))),
            }
        }

        let id = id.ok_or_else(|| Error::Lock("missing spore".into()))?;
        let version = version.ok_or_else(|| Error::Lock("missing version".into()))?;
        let expected = digest.ok_or_else(|| Error::Lock("missing digest".into()))?;
        let lock = Self::new(&id, &version, files);
        if lock.digest != expected {
            return Err(Error::Lock(format!(
                "digest mismatch: declared {expected}, computed {}",
                lock.digest
            )));
        }
        Ok(lock)
    }

    pub fn file(&self, path: &str) -> Option<&FileEntry> {
        self.files.iter().find(|entry| entry.path == path)
    }
}

fn body(id: &str, version: &str, files: &[FileEntry]) -> String {
    let mut out = String::new();
    out.push_str(LOCK_HEADER);
    out.push('\n');
    out.push_str(&format!("spore {id}\n"));
    out.push_str(&format!("version {version}\n"));
    for file in files {
        out.push_str(&format!(
            "file {} {} {}\n",
            file.sha256, file.size, file.path
        ));
    }
    out
}

// ---- package reading ------------------------------------------------------

/// Every package file except `signatures/` and `spore.lock`, sorted by path.
///
/// This is the whole tree, build inputs included. `collect_package` is what
/// ships; this stays for callers that need to inspect a directory as it is.
pub fn collect(root: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = BTreeMap::new();
    walk(root, root, &mut files)?;
    Ok(files.into_iter().collect())
}

/// The files a package actually ships, sorted by path.
///
/// Declared files must exist: a package that names an asset it does not carry
/// is refused here rather than at launch. The Lua UI is a directory artifact --
/// every `.lua` under `ui/` is part of it, because how an author splits a view
/// is not something the manifest should have to enumerate.
pub fn collect_package(root: &Path, manifest: &Manifest) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = BTreeMap::new();
    for path in manifest.package_paths() {
        let full = root.join(&path);
        let metadata = fs::symlink_metadata(&full)
            .map_err(|error| Error::Io(format!("{}: {error}", full.display())))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::Invalid(format!(
                "{} is declared but is not a regular file",
                path.display()
            )));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(Error::Invalid(format!(
                "{} exceeds {MAX_FILE_BYTES} bytes",
                path.display()
            )));
        }
        let name = path
            .to_str()
            .ok_or_else(|| Error::UnsafePath(path.display().to_string()))?
            .replace('\\', "/");
        let bytes =
            fs::read(&full).map_err(|error| Error::Io(format!("{}: {error}", full.display())))?;
        files.insert(name, bytes);
    }
    let ui = root.join("ui");
    if ui.is_dir() {
        walk_dir(root, &ui, Some("lua"), &mut files)?;
    }
    // A declared surface is a directory artifact for the same reason the Lua UI
    // is: how an author splits a web page across modules, styles and fonts is
    // not something the manifest should have to enumerate. The package carries
    // the files; the surface names the entry point into them.
    for surface in &manifest.surfaces {
        let Some(entry) = surface.entry(manifest.ui.as_ref()) else {
            continue;
        };
        let Some(dir) = Path::new(entry).parent() else {
            continue;
        };
        // An entry at the package root names no directory of its own, and
        // walking the root here would ship the build inputs this function
        // exists to leave out.
        if dir.as_os_str().is_empty() {
            continue;
        }
        let full = root.join(dir);
        if full.is_dir() {
            walk_dir(root, &full, None, &mut files)?;
        }
    }
    Ok(files.into_iter().collect())
}

/// Add every regular file under `dir`, when `extension` is `None`, or only the
/// files with that extension.
fn walk_dir(
    root: &Path,
    dir: &Path,
    extension: Option<&str>,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    for entry in
        fs::read_dir(dir).map_err(|error| Error::Io(format!("{}: {error}", dir.display())))?
    {
        let entry = entry.map_err(|error| Error::Io(error.to_string()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        if metadata.file_type().is_symlink() {
            return Err(Error::UnsafePath(format!(
                "{} is a symlink; packages contain regular files",
                path.display()
            )));
        }
        if metadata.is_dir() {
            walk_dir(root, &path, extension, out)?;
            continue;
        }
        let wanted =
            extension.is_none_or(|wanted| path.extension().is_some_and(|found| found == wanted));
        if !metadata.is_file() || !wanted {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| Error::UnsafePath(path.display().to_string()))?;
        let name = relative
            .to_str()
            .ok_or_else(|| Error::UnsafePath(relative.display().to_string()))?
            .replace('\\', "/");
        let bytes =
            fs::read(&path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        out.insert(name, bytes);
    }
    Ok(())
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    let entries =
        fs::read_dir(dir).map_err(|error| Error::Io(format!("{}: {error}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::Io(error.to_string()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        if metadata.file_type().is_symlink() {
            return Err(Error::UnsafePath(format!(
                "{} is a symlink; packages contain regular files",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| Error::UnsafePath(path.display().to_string()))?;
        let name = relative
            .to_str()
            .ok_or_else(|| Error::UnsafePath(relative.display().to_string()))?
            .replace('\\', "/");

        if metadata.is_dir() {
            if name == SIGNATURE_DIR {
                continue;
            }
            walk(root, &path, out)?;
            continue;
        }
        if !metadata.is_file() || name == LOCK_NAME {
            continue;
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(Error::Invalid(format!(
                "{name} exceeds {MAX_FILE_BYTES} bytes"
            )));
        }
        let bytes =
            fs::read(&path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        out.insert(name, bytes);
    }
    Ok(())
}

/// Compute the lock for a directory's current contents.
pub fn compute_lock(root: &Path, manifest: &Manifest) -> Result<Lock> {
    let files = collect_package(root, manifest)?;
    Ok(lock_of(&manifest.spore.id, &manifest.spore.version, &files))
}

fn lock_of(id: &str, version: &str, files: &[(String, Vec<u8>)]) -> Lock {
    let entries = files
        .iter()
        .map(|(path, bytes)| FileEntry {
            path: path.clone(),
            sha256: hex(&Sha256::digest(bytes)),
            size: bytes.len() as u64,
        })
        .collect();
    Lock::new(id, version, entries)
}

/// Check a directory: manifest, declared files, and the lock if present.
/// The WIT packages a component imports, e.g. `wasi:http`, `soil:records`.
///
/// A package is the unit a manifest declares, so this is what `[grants]` must
/// cover: an import nobody declared is a capability the package author did not
/// write down.
pub fn imported_packages(bytes: &[u8]) -> Result<BTreeSet<String>> {
    // `wit_component::decode` also accepts a bare core module and reports an
    // empty world for it, which would make the manifest rule vacuous. A Spore
    // module is a component by definition, so check the preamble first: a
    // component starts `\0asm\x0d\x00`, a core module `\0asm\x01\x00`.
    if !(bytes.len() >= 8 && &bytes[..4] == b"\0asm" && bytes[4] == 0x0d && bytes[5] == 0x00) {
        return Err(Error::Invalid(
            "module is a core wasm module, not a component".into(),
        ));
    }
    let decoded = wit_component::decode(bytes)
        .map_err(|error| Error::Invalid(format!("module is not a component: {error}")))?;
    let (resolve, world) = match decoded {
        wit_component::DecodedWasm::Component(resolve, world) => (resolve, world),
        wit_component::DecodedWasm::WitPackage(..) => {
            return Err(Error::Invalid(
                "module is a WIT package, not a component".into(),
            ));
        }
    };
    let mut packages = BTreeSet::new();
    for key in resolve.worlds[world].imports.keys() {
        let wit_parser::WorldKey::Interface(id) = key else {
            continue;
        };
        let Some(package) = resolve.interfaces[*id].package else {
            continue;
        };
        let name = &resolve.packages[package].name;
        packages.insert(format!("{}:{}", name.namespace, name.name));
    }
    Ok(packages)
}

fn undeclared_packages(
    imported: &BTreeSet<String>,
    grants: &BTreeMap<String, String>,
) -> Vec<String> {
    imported
        .iter()
        .filter(|package| !grants.contains_key(package.as_str()))
        .cloned()
        .collect()
}

/// Run the module's real imports past the manifest.
///
/// The manifest is intent; this makes it checkable. A module that imports
/// `soil:records` while the manifest lists only WASI fails here instead of at
/// review time.
fn check_declared_grants(root: &Path, manifest: &Manifest) -> Result<()> {
    // A chrome-only Spore has no module, so there is nothing to import.
    let Some(module) = &manifest.module else {
        return Ok(());
    };
    let path = root.join(&module.path);
    let bytes =
        fs::read(&path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    let imported = imported_packages(&bytes)?;
    let undeclared = undeclared_packages(&imported, &manifest.grants);
    if !undeclared.is_empty() {
        return Err(Error::Invalid(format!(
            "{} imports {} but [grants] does not declare {}",
            module.path,
            undeclared.join(", "),
            if undeclared.len() == 1 { "it" } else { "them" },
        )));
    }
    Ok(())
}

pub fn check(root: &Path) -> Result<Lock> {
    let manifest = Manifest::load(root)?;
    for path in manifest.declared_paths() {
        let full = root.join(&path);
        if !full.is_file() {
            return Err(Error::Invalid(format!(
                "{} is declared but missing",
                path.display()
            )));
        }
    }
    check_declared_grants(root, &manifest)?;
    let lock = compute_lock(root, &manifest)?;
    let path = root.join(LOCK_NAME);
    if path.is_file() {
        let declared = fs::read_to_string(&path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        let declared = Lock::parse(&declared)?;
        if declared != lock {
            return Err(Error::Lock(format!(
                "{LOCK_NAME} is stale; run `soil-spore lock`"
            )));
        }
    }
    Ok(lock)
}

/// Mechanical admission rules for a public listing.
///
/// These are the ones a program can check. They exist so the review queue
/// starts from a package that already has the shape of a listing: a paragraph,
/// a readable manual, a declared category set, and screenshots someone can look
/// at. Everything else -- authorship, intent, whether the grants are honest --
/// stays a human review. Empty means the package is ready to be reviewed, not
/// that it is approved.
pub fn publish_blockers(manifest: &Manifest) -> Vec<String> {
    let mut blockers = Vec::new();
    if manifest
        .spore
        .description
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        blockers.push("spore.description is missing".into());
    }
    if manifest
        .spore
        .homepage
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        blockers.push("spore.homepage is missing; a listing must say where it comes from".into());
    }
    let usage = manifest
        .docs
        .as_ref()
        .map(|docs| docs.usage.len())
        .unwrap_or(0);
    if usage == 0 {
        blockers.push("docs.usage is missing; nobody can try it without a synopsis".into());
    }
    if manifest
        .docs
        .as_ref()
        .and_then(|docs| docs.man.as_ref())
        .is_none()
    {
        blockers.push("docs.man is missing; a listing needs a manual".into());
    }
    if manifest
        .presentation
        .as_ref()
        .is_none_or(|presentation| presentation.icon.is_none())
    {
        blockers.push("presentation.icon is missing; a listing needs a mark".into());
    }
    match manifest.catalog.as_ref() {
        None => blockers.push("catalog is missing; there is nothing to list".into()),
        Some(catalog) => {
            if catalog.screenshots.len() < 2 {
                blockers.push(
                    "catalog.screenshots needs at least two images; one screen is not a listing"
                        .into(),
                );
            }
            if catalog.keywords.is_empty() {
                blockers.push("catalog.keywords is empty".into());
            }
        }
    }
    for (capability, reason) in &manifest.grants {
        if reason.trim().is_empty() {
            blockers.push(format!("grant {capability:?} has no written reason"));
        }
    }
    blockers
}

/// Write a fresh lock.
pub fn write_lock(root: &Path) -> Result<Lock> {
    let manifest = Manifest::load(root)?;
    let lock = compute_lock(root, &manifest)?;
    let path = root.join(LOCK_NAME);
    fs::write(&path, lock.render())
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    Ok(lock)
}

// ---- tar ------------------------------------------------------------------

/// Pack a directory into a deterministic tar.
///
/// Sorted entries, zero mtime/uid/gid, fixed modes. Repacking the same tree
/// yields identical bytes, so an IPFS CID identifies the package and nothing
/// else.
pub fn pack(root: &Path) -> Result<Vec<u8>> {
    let manifest = Manifest::load(root)?;
    let lock = write_lock(root)?;
    if lock.id != manifest.spore.id || lock.version != manifest.spore.version {
        return Err(Error::Lock("lock does not match the manifest".into()));
    }

    let mut files = collect_package(root, &manifest)?;
    files.push((LOCK_NAME.to_owned(), lock.render().into_bytes()));

    let total: u64 = files.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    if total > MAX_PACKAGE_BYTES {
        return Err(Error::Invalid(format!(
            "package exceeds {MAX_PACKAGE_BYTES} bytes"
        )));
    }

    let mut builder = tar::Builder::new(Vec::new());
    for (path, bytes) in &files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_username("").map_err(io_error)?;
        header.set_groupname("").map_err(io_error)?;
        header.set_cksum();
        builder
            .append_data(&mut header, path, Cursor::new(bytes))
            .map_err(io_error)?;
    }
    builder.into_inner().map_err(io_error)
}

/// Verify a packed tar: entry safety, embedded lock, and every digest.
pub fn verify(bytes: &[u8]) -> Result<(Manifest, Lock)> {
    let (manifest, lock, _) = read_package(bytes)?;
    Ok((manifest, lock))
}

/// Unpack a verified package into `dest`.
///
/// The archive is the distribution unit: a machine installs from a `.spore`
/// file and never needs the Spore's source, its composer, or the SDK it was
/// built against. Everything is verified before a single byte is written, so a
/// tampered or partial package cannot leave half a Spore on disk.
///
/// # Errors
/// Fails when the package is unsafe, tampered, or the destination cannot be
/// written.
pub fn unpack(bytes: &[u8], dest: &Path) -> Result<Lock> {
    let (_, lock, files) = read_package(bytes)?;
    for (path, body) in files {
        let target = dest.join(&path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        fs::write(&target, body).map_err(io_error)?;
    }
    Ok(lock)
}

/// The CIDv1 of a package's bytes: raw codec over sha2-256.
///
/// A `.spore` tar is deterministic, so the same tree yields the same bytes and
/// therefore the same CID. That is the identity a registry versions on top of;
/// `spore.lock` stays the human-readable digest.
#[must_use]
pub fn cid_v1_raw(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut block = vec![0x01, 0x55, 0x12, 0x20];
    block.extend_from_slice(&digest);
    format!("b{}", base32_lower(&block))
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

/// Verify a packed tar and hand back its files.
///
/// Installation needs the bytes it just verified; a caller that only wants the
/// identity calls `verify`.
pub fn read_package(bytes: &[u8]) -> Result<(Manifest, Lock, BTreeMap<String, Vec<u8>>)> {
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(Error::Invalid("package is too large".into()));
    }
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive.entries().map_err(io_error)?;

    for entry in entries {
        let mut entry = entry.map_err(io_error)?;
        let kind = entry.header().entry_type();
        if !kind.is_file() {
            return Err(Error::UnsafePath(format!(
                "{} is not a regular file",
                entry
                    .path()
                    .map_or_else(|_| "entry".into(), |p| p.display().to_string())
            )));
        }
        let path = entry
            .path()
            .map_err(io_error)?
            .to_str()
            .ok_or_else(|| Error::UnsafePath("entry name is not UTF-8".into()))?
            .replace('\\', "/");
        safe_relative(&path)?;
        if files.contains_key(&path) {
            return Err(Error::Invalid(format!("duplicate entry {path}")));
        }
        let mut buffer = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buffer).map_err(io_error)?;
        files.insert(path, buffer);
    }

    let lock_bytes = files
        .remove(LOCK_NAME)
        .ok_or_else(|| Error::Lock(format!("package has no {LOCK_NAME}")))?;
    let lock = Lock::parse(
        std::str::from_utf8(&lock_bytes)
            .map_err(|_| Error::Lock("spore.lock is not UTF-8".into()))?,
    )?;

    let computed = lock_of(
        &lock.id,
        &lock.version,
        &files
            .iter()
            .map(|(path, bytes)| (path.clone(), bytes.clone()))
            .collect::<Vec<_>>(),
    );
    if computed.digest != lock.digest {
        return Err(Error::Lock(format!(
            "contents do not match the lock (expected {}, got {})",
            lock.digest, computed.digest
        )));
    }

    let manifest = Manifest::parse(
        std::str::from_utf8(
            files
                .get("spore.toml")
                .ok_or_else(|| Error::Invalid("package has no spore.toml".into()))?,
        )
        .map_err(|_| Error::Invalid("spore.toml is not UTF-8".into()))?,
    )?;
    if manifest.spore.id != lock.id || manifest.spore.version != lock.version {
        return Err(Error::Lock(
            "manifest identity does not match the lock".into(),
        ));
    }
    Ok((manifest, lock, files))
}

// ---- signatures -----------------------------------------------------------

/// Sign a packed tar with a detached, armored PGP signature.
///
/// Soil never generates, stores, or interprets a key. Mycelium binds a key to a
/// user and supplies the identity; an external keyring is equally acceptable.
pub fn sign(package: &Path, signature: &Path, key: Option<&str>) -> Result<()> {
    let mut command = std::process::Command::new("gpg");
    command
        .arg("--batch")
        .arg("--yes")
        .arg("--armor")
        .arg("--detach-sign")
        .arg("--output")
        .arg(signature);
    if let Some(key) = key {
        command.arg("--local-user").arg(key);
    }
    command.arg(package);
    run(command, "gpg --detach-sign")
}

/// Sign a package through a running Geode broker.
///
/// `principal` is the Geode account label, not the raw OIDC subject: the
/// reference is `geode://principal/<label>/pgp`. The `geode-pgp` client leases
/// the key from `geoded` and returns only the detached signature; Soil never
/// sees key material.
///
/// `socket` defaults to `GEODE_SOCKET` or `$HOME/.geode/geode.sock` in the
/// client. `fingerprint` is optional because the client can read it from the
/// leased key.
pub fn sign_principal(
    package: &Path,
    signature: &Path,
    principal: &str,
    socket: Option<&Path>,
    fingerprint: Option<&str>,
) -> Result<()> {
    validate_geode_label(principal)?;
    let reference = format!("geode://principal/{principal}/pgp");
    let mut command = std::process::Command::new("geode-pgp");
    command
        .arg("sign")
        .arg("--reference")
        .arg(&reference)
        .arg("--purpose")
        .arg("spore.package.sign")
        .arg("--message")
        .arg(package);
    if let Some(socket) = socket {
        command.arg("--socket").arg(socket);
    }
    if let Some(fingerprint) = fingerprint {
        command.arg("--fingerprint").arg(fingerprint);
    }
    let output = command
        .output()
        .map_err(|error| Error::Tool(format!("geode-pgp: {error}")))?;
    if !output.status.success() {
        return Err(Error::Signature(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    std::fs::write(signature, output.stdout)
        .map_err(|error| Error::Io(format!("{}: {error}", signature.display())))
}

fn validate_geode_label(label: &str) -> Result<()> {
    let mut bytes = label.bytes();
    let starts_well = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric());
    let rest_well =
        bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if label.is_empty() || label.len() > 64 || !starts_well || !rest_well {
        return Err(Error::Invalid(
            "--principal must be a Geode label: 1..=64 letters, digits, `.`, `_` or `-`".into(),
        ));
    }
    Ok(())
}

/// Verify a detached armored signature over an arbitrary payload.
///
/// This is the catalog/delegation counterpart to [`verify_signature`], which is
/// tied to a package file. The public key is imported into a temporary GnuPG
/// home for one verification and then removed. A missing `gpg` fails closed.
pub fn verify_detached(public_key: &[u8], payload: &[u8], signature: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let base = if Path::new("/tmp").is_dir() {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let home = tempfile::Builder::new()
        .prefix("soil-pgp-")
        .tempdir_in(&base)
        .map_err(|error| Error::Tool(format!("temporary GnuPG home: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700))
            .map_err(|error| Error::Tool(format!("protect GnuPG home: {error}")))?;
    }

    let import = {
        let mut command = Command::new("gpg");
        command
            .arg("--homedir")
            .arg(home.path())
            .args(["--batch", "--no-tty", "--import"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| Error::Tool(format!("gpg: {error}")))?;
        child
            .stdin
            .take()
            .ok_or_else(|| Error::Tool("gpg import has no stdin".into()))?
            .write_all(public_key)
            .map_err(|error| Error::Tool(format!("gpg import: {error}")))?;
        child
            .wait_with_output()
            .map_err(|error| Error::Tool(format!("gpg import: {error}")))?
    };
    if !import.status.success() {
        return Err(Error::Signature(format!(
            "could not import public key: {}",
            String::from_utf8_lossy(&import.stderr).trim()
        )));
    }

    let signature_path = home.path().join("signature.asc");
    let payload_path = home.path().join("payload");
    fs::write(&signature_path, signature)
        .map_err(|error| Error::Io(format!("{}: {error}", signature_path.display())))?;
    fs::write(&payload_path, payload)
        .map_err(|error| Error::Io(format!("{}: {error}", payload_path.display())))?;

    let verified = {
        let mut command = Command::new("gpg");
        command
            .arg("--homedir")
            .arg(home.path())
            .args(["--batch", "--no-tty", "--verify"])
            .arg(&signature_path)
            .arg(&payload_path);
        let output = command
            .output()
            .map_err(|error| Error::Tool(format!("gpg verify: {error}")))?;
        let _ = Command::new("gpgconf")
            .arg("--homedir")
            .arg(home.path())
            .args(["--kill", "all"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        output
    };
    if verified.status.success() {
        Ok(())
    } else {
        Err(Error::Signature(
            String::from_utf8_lossy(&verified.stderr).trim().to_owned(),
        ))
    }
}

/// Verify a detached signature against a trusted keyring.
pub fn verify_signature(
    package: &Path,
    signature: &Path,
    keyring: Option<&Path>,
    required_key: Option<&str>,
) -> Result<()> {
    let mut command = std::process::Command::new("gpg");
    command
        .arg("--batch")
        .arg("--no-tty")
        .arg("--status-fd")
        .arg("1");
    if let Some(keyring) = keyring {
        command
            .arg("--no-default-keyring")
            .arg("--keyring")
            .arg(keyring);
    }
    command.arg("--verify").arg(signature).arg(package);

    let output = command
        .output()
        .map_err(|error| Error::Tool(format!("gpg: {error}")))?;
    if !output.status.success() {
        return Err(Error::Signature(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    if let Some(key) = required_key {
        let status = String::from_utf8_lossy(&output.stdout);
        let valid = status.lines().any(|line| {
            line.contains("VALIDSIG") && line.split_whitespace().any(|field| field.ends_with(key))
        });
        if !valid {
            return Err(Error::Signature(format!("not signed by {key}")));
        }
    }
    Ok(())
}

fn run(mut command: std::process::Command, label: &str) -> Result<()> {
    let output = command
        .output()
        .map_err(|error| Error::Tool(format!("{label}: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn io_error(error: impl fmt::Display) -> Error {
    Error::Io(error.to_string())
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A valid Spore manifest with `extra` appended, so a test writes only the
/// table it is about.
#[cfg(test)]
mod fixtures {
    pub(crate) fn manifest(extra: &str) -> String {
        with_ui("[ui]\nentry = \"ui/main.lua\"\n", extra)
    }

    /// Same manifest with the `[ui]` block chosen by the caller, so a test can
    /// ask what a package looks like with no Lua entrypoint at all.
    pub(crate) fn with_ui(ui: &str, extra: &str) -> String {
        format!(
            "schema = 1\n[spore]\nid = \"dev.fungos.chrome\"\nname = \"chrome\"\nversion = \"0.1.0\"\nlicense = \"MIT\"\nsummary = \"shell chrome\"\n[engine]\nsoil = \">=0.1\"\ntarget = \"wasm32-wasip2\"\n[module]\npath = \"module.wasm\"\n{ui}[launch]\ndefault = \"canvas\"\nsurfaces = [\"canvas\"]\n{extra}"
        )
    }
}

#[cfg(test)]
mod surface_tests {
    use super::fixtures::manifest;
    use super::*;

    /// A Spore that ships one web page and one Canvas view, launched on the
    /// page by default.
    const DECLARED: &str = "[launch]\ndefault = \"workbook\"\nsurfaces = [\"canvas\", \"web\"]\n\
[[surfaces]]\nname = \"workbook\"\nkind = \"web\"\nentry = \"web/index.html\"\n\
[[surfaces]]\nname = \"preview\"\nkind = \"canvas\"\nentry = \"ui/sheet.lua\"\n";

    /// The fixture's own `[launch]`, so a test writes only the tables it is
    /// about. `with_ui` already emits one, and TOML cannot repeat a table.
    fn declared() -> String {
        manifest("").replace(
            "[launch]\ndefault = \"canvas\"\nsurfaces = [\"canvas\"]\n",
            DECLARED,
        )
    }

    #[test]
    fn an_absent_surface_block_parses_exactly_as_before() {
        let parsed = Manifest::parse(&manifest("")).expect("a Spore with no surfaces is a Spore");
        assert!(parsed.surfaces.is_empty());
        assert_eq!(parsed.launch.surfaces, vec!["canvas".to_owned()]);
        assert_eq!(parsed.surface_names(), vec!["canvas"]);
        // The kind list is still the authority, so the old launch path is what
        // a host that never heard of surfaces keeps getting.
        let target = parsed
            .resolve_launch("canvas", &SurfaceKind::ALL)
            .expect("canvas is what it declares");
        assert_eq!(target.kind, Some(SurfaceKind::Canvas));
        assert_eq!(
            target.entry, None,
            "no declaration means the host renders the package's own UI"
        );
        assert_eq!(
            parsed.declared_paths(),
            vec![PathBuf::from("ui/main.lua"), PathBuf::from("module.wasm")],
            "declaring nothing adds nothing to the package"
        );
    }

    #[test]
    fn a_declared_web_surface_and_a_canvas_one_both_resolve() {
        let parsed = Manifest::parse(&declared()).expect("canvas and web together are a Spore");
        assert_eq!(parsed.surface_names(), vec!["workbook", "preview"]);

        let web = parsed
            .resolve_launch("workbook", &SurfaceKind::ALL)
            .expect("a host with a browser opens the page");
        assert_eq!(web.kind, Some(SurfaceKind::Web));
        assert_eq!(web.entry, Some("web/index.html"));
        assert_eq!(web.surface, "workbook");

        let canvas = parsed
            .resolve_launch("preview", &SurfaceKind::ALL)
            .expect("a host with Canvas opens the view");
        assert_eq!(canvas.kind, Some(SurfaceKind::Canvas));
        assert_eq!(canvas.entry, Some("ui/sheet.lua"));

        // The default is a surface name once surfaces exist, and it is one of
        // them rather than a fourth thing.
        assert_eq!(parsed.launch.default, "workbook");
        assert!(parsed.surface(&parsed.launch.default).is_some());
    }

    #[test]
    fn a_web_surface_may_leave_the_conventional_entry_out() {
        let parsed = Manifest::parse(&manifest("").replace(
            "[launch]\ndefault = \"canvas\"\nsurfaces = [\"canvas\"]\n",
            "[launch]\ndefault = \"workbook\"\nsurfaces = [\"web\"]\n\
[[surfaces]]\nname = \"workbook\"\nkind = \"web\"\n",
        ))
        .expect("a Spore that ships the conventional page says so once");
        assert_eq!(
            parsed
                .resolve_launch("workbook", &SurfaceKind::ALL)
                .expect("resolves")
                .entry,
            Some(DEFAULT_WEB_ENTRY),
            "the page is named by the kind, not repeated in every manifest"
        );
    }

    #[test]
    fn a_default_that_names_no_surface_lists_both_sets() {
        let bad = declared().replace("default = \"workbook\"", "default = \"spreadsheet\"");
        let error = Manifest::parse(&bad).expect_err("a default has to name something");
        let message = error.to_string();
        assert!(
            message.contains("spreadsheet"),
            "the error names what was asked for: {message}"
        );
        assert!(
            message.contains("workbook") && message.contains("preview"),
            "and everything it could have named: {message}"
        );
    }

    #[test]
    fn an_unknown_surface_kind_lists_the_valid_ones() {
        let bad = declared().replace("kind = \"canvas\"", "kind = \"desktop\"");
        let error = Manifest::parse(&bad).expect_err("the host only has the kinds it has");
        let message = error.to_string();
        assert!(
            message.contains("desktop"),
            "the error names the bad kind: {message}"
        );
        for valid in SurfaceKind::ALL {
            assert!(
                message.contains(valid.as_str()),
                "and every kind that would have parsed: {message}"
            );
        }
    }

    #[test]
    fn a_web_surface_on_a_host_without_a_browser_is_unsupported() {
        let parsed = Manifest::parse(&declared()).expect("manifest");
        let refused = parsed
            .resolve_launch("workbook", &[SurfaceKind::Canvas])
            .expect_err("a host with no browser view cannot render a page");
        assert_eq!(refused.code(), "unsupported");
        assert!(matches!(
            refused,
            LaunchError::Unsupported {
                kind: SurfaceKind::Web,
                ..
            }
        ));

        // The same host still opens the Canvas surface, so the refusal is about
        // the kind and not about the package.
        assert!(
            parsed
                .resolve_launch("preview", &[SurfaceKind::Canvas])
                .is_ok()
        );
    }

    #[test]
    fn an_unknown_surface_name_is_not_found() {
        let parsed = Manifest::parse(&declared()).expect("manifest");
        let refused = parsed
            .resolve_launch("spreadsheet", &SurfaceKind::ALL)
            .expect_err("the package declares no such view");
        assert_eq!(refused.code(), "not-found");
    }

    #[test]
    fn a_declared_surface_kind_must_be_launchable() {
        let bad = declared().replace(
            "surfaces = [\"canvas\", \"web\"]",
            "surfaces = [\"canvas\"]",
        );
        assert!(
            Manifest::parse(&bad).is_err(),
            "a web surface the kind list forbids is a contradiction"
        );
    }
}

#[cfg(test)]
mod chrome_tests {
    use super::fixtures::{manifest, with_ui};
    use super::*;

    #[test]
    fn a_chrome_spore_must_source_every_region_it_claims() {
        // One region, one UI file: the entry already is the region's Lua, so no
        // `[chrome.sources]` is needed and none can go stale.
        let implicit = Manifest::parse(&manifest("[chrome]\nregions = [\"dock\"]\n"))
            .expect("a single-region Spore defaults to its UI entry");
        assert_eq!(implicit.region_source("dock"), Some("ui/main.lua"));
        assert_eq!(
            implicit.region_source("top-bar"),
            None,
            "an unclaimed region has no source"
        );

        let explicit = Manifest::parse(&manifest(
            "[chrome]\nregions = [\"dock\"]\n[chrome.sources]\ndock = \"dist/dock.lua\"\n[presentation]\nbadge = \"D\"\ncolor = \"#8bd5ca\"\n",
        ))
        .expect("an explicit region artifact wins over the UI entry");
        assert_eq!(explicit.region_source("dock"), Some("dist/dock.lua"));
        assert_eq!(explicit.presentation.as_ref().unwrap().badge, "D");

        let missing = Manifest::parse(&with_ui("", "[chrome]\nregions = [\"dock\"]\n"));
        assert!(
            missing.is_err(),
            "a region with no source and no UI entry is a hole in the shell"
        );

        let unknown = Manifest::parse(&manifest(
            "[chrome]\nregions = [\"statusbar\"]\n[chrome.sources]\nstatusbar = \"ui/x.lua\"\n",
        ));
        assert!(unknown.is_err(), "the host only has the regions it knows");

        let bad_color = Manifest::parse(&manifest(
            "[presentation]\nbadge = \"D\"\ncolor = \"teal\"\n",
        ));
        assert!(bad_color.is_err(), "a colour is #rrggbb or it is nothing");
    }

    #[test]
    fn declared_paths_cover_chrome_sources() {
        let explicit = Manifest::parse(&manifest(
            "[chrome]\nregions = [\"dock\"]\n[chrome.sources]\ndock = \"dist/dock.lua\"\n",
        ))
        .expect("manifest");
        let paths = explicit.declared_paths();
        assert!(paths.contains(&PathBuf::from("dist/dock.lua")));

        let implicit =
            Manifest::parse(&manifest("[chrome]\nregions = [\"dock\"]\n")).expect("manifest");
        assert_eq!(
            implicit.declared_paths(),
            vec![PathBuf::from("ui/main.lua"), PathBuf::from("module.wasm")],
            "the UI entry is declared once even when a region also resolves to it"
        );
    }

    #[test]
    fn a_dependency_is_one_exact_version() {
        let ok = Manifest::parse(&manifest("[deps]\n\"fungos.ui\" = \"0.1.0\"\n"));
        let ok = ok.expect("an exact version parses");
        assert_eq!(ok.deps.get("fungos.ui").map(String::as_str), Some("0.1.0"));

        for range in ["^0.1.0", "~0.1.0", ">=0.1.0", "0.1.*", "<0.2.0"] {
            let bad = Manifest::parse(&manifest(&format!("[deps]\n\"fungos.ui\" = \"{range}\"\n")));
            assert!(
                bad.is_err(),
                "{range} is a range, and a composed artifact cannot float"
            );
        }

        let bad_name = Manifest::parse(&manifest("[deps]\n\"Fungos.UI\" = \"0.1.0\"\n"));
        assert!(
            bad_name.is_err(),
            "a dependency name is a lowercase identifier"
        );
    }

    #[test]
    fn lua_is_an_entrypoint_of_its_own() {
        let lua_only = with_ui("[ui]\nentry = \"ui/app.lua\"\n", "")
            .replace("[module]\npath = \"module.wasm\"\n", "");
        let parsed = Manifest::parse(&lua_only).expect("a Lua-only Spore is a Spore");
        assert!(parsed.module.is_none());
        assert_eq!(
            parsed.declared_paths(),
            vec![PathBuf::from("ui/app.lua")],
            "no wasm means nothing claims a module file"
        );

        // Chrome with no UI entry still installs, because the host draws it and
        // the region names its own artifact.
        let chrome_only = Manifest::parse(
            &with_ui("", "[chrome]\nregions = [\"dock\"]\n")
                .replace("[module]\npath = \"module.wasm\"\n", "")
                .replace(
                    "[chrome]\nregions = [\"dock\"]\n",
                    "[chrome]\nregions = [\"dock\"]\n[chrome.sources]\ndock = \"ui/dock.lua\"\n",
                ),
        )
        .expect("a chrome Spore needs no UI entry when it names its artifact");
        assert_eq!(chrome_only.region_source("dock"), Some("ui/dock.lua"));
    }

    #[test]
    fn a_spore_with_no_entrypoint_is_refused() {
        let empty = with_ui("", "").replace("[module]\npath = \"module.wasm\"\n", "");
        let error = Manifest::parse(&empty).expect_err("nothing to run, nothing to draw");
        assert!(
            error.to_string().contains("entrypoint"),
            "the error says what is missing: {error}"
        );
    }
}

#[cfg(test)]
mod anycast_tests {
    use super::fixtures::manifest;
    use super::identity::Identity;
    use super::publisher::TrustedPublisher;
    use super::{Error, Lock, Manifest, lease};

    const CLAIM: &str =
        "[anycast]\nname = \"confluence\"\nkind = \"confluence\"\nalpns = [\"soil/http/1\"]\n";
    const PEM: &str =
        "-----BEGIN CERTIFICATE-----\nMIIBszCCAVmgAwIBAgIUQ==\n-----END CERTIFICATE-----\n";

    fn lock() -> Lock {
        Lock {
            id: "dev.fungos.confluence".into(),
            version: "0.1.0".into(),
            files: Vec::new(),
            digest: "sha256:abc".into(),
        }
    }

    fn publisher() -> TrustedPublisher {
        TrustedPublisher {
            principal: "fleet/fpl".into(),
            grant_id: "grant-7".into(),
            key_id: "key-1".into(),
        }
    }

    fn identity() -> Identity {
        Identity::derived(PEM, "").expect("the fixture certificate denotes a node")
    }

    #[test]
    fn a_claim_renders_a_lease_the_daemon_would_accept() {
        let parsed = Manifest::parse(&manifest(CLAIM)).expect("claim parses");
        let identity = identity();
        let document = lease::document(
            &parsed,
            &lock(),
            &publisher(),
            &identity,
            &["http://10.253.0.6:8788".into()],
            1_000,
        )
        .expect("lease renders");
        let observation = &document["observations"][0];
        assert_eq!(
            observation["service_id"],
            format!("service/{}/application/confluence", identity.node_id)
        );
        assert_eq!(observation["value"]["kind"], "confluence");
        assert_eq!(
            observation["value"]["endpoints"][0],
            "http://10.253.0.6:8788"
        );
        assert_eq!(observation["value"]["attributes"]["grant_id"], "grant-7");
        assert_eq!(
            observation["value"]["attributes"]["package_digest"],
            "sha256:abc"
        );
        assert_eq!(
            observation["value"]["attributes"]["node_id"],
            identity.node_id.as_str()
        );
        // The lease the daemon accepts expires; a lease it must renew cannot
        // carry a longer life than it is allowed to honour.
        assert_eq!(observation["expires_at"], 1_240);
    }

    #[test]
    fn a_named_node_is_findable_from_its_lease() {
        let parsed = Manifest::parse(&manifest(CLAIM)).expect("claim parses");
        let named = Identity::derived(
            PEM,
            "MYCELIUM_SITE=wagar-house\nMYCELIUM_IROH_SERVER_NAME=runner-3.wagar-house.myc\n",
        )
        .expect("identity");
        let document = lease::document(
            &parsed,
            &lock(),
            &publisher(),
            &named,
            &["http://10.253.0.6:8788".into()],
            1_000,
        )
        .expect("lease renders");
        let attributes = &document["observations"][0]["value"]["attributes"];
        // A digest alone tells an operator nothing about which host answered.
        assert_eq!(attributes["node_name"], "runner-3.wagar-house.myc");
        assert_eq!(attributes["site"], "wagar-house");
    }

    #[test]
    fn a_lease_never_outlives_what_mycelium_honours() {
        let too_long = Manifest::parse(&manifest(
            "[anycast]\nname = \"confluence\"\nkind = \"confluence\"\nttl_secs = 301\n",
        ));
        assert!(too_long.is_err(), "301 s is a renewer that will never run");

        let zero = Manifest::parse(&manifest(
            "[anycast]\nname = \"confluence\"\nkind = \"confluence\"\nttl_secs = 0\n",
        ));
        assert!(zero.is_err(), "a lease with no life is not a lease");
    }

    #[test]
    fn a_claim_must_be_addressable() {
        let upper = Manifest::parse(&manifest(
            "[anycast]\nname = \"Confluence\"\nkind = \"confluence\"\n",
        ));
        assert!(upper.is_err(), "a service label is lowercase alphanumeric");

        let empty = Manifest::parse(&manifest("[anycast]\nname = \"confluence\"\nkind = \"\"\n"));
        assert!(empty.is_err(), "a claim with no kind joins nothing");
    }

    #[test]
    fn a_spore_without_a_claim_never_leases() {
        let parsed = Manifest::parse(&manifest(""));
        let plain = parsed.expect("a local Spore still parses");
        assert!(plain.anycast.is_none());
        let refused = lease::document(
            &plain,
            &lock(),
            &publisher(),
            &identity(),
            &["http://10.253.0.6:8788".into()],
            1_000,
        );
        assert!(matches!(refused, Err(Error::Invalid(_))));
    }

    #[test]
    fn endpoints_are_hints_without_credentials() {
        let parsed = Manifest::parse(&manifest(CLAIM)).expect("claim parses");
        for bad in [
            "http://user:pw@10.0.0.1:8788",
            "http://10.0.0.1:8788/secret",
            "ftp://10.0.0.1:21",
            "10.0.0.1:8788",
        ] {
            let refused = lease::document(
                &parsed,
                &lock(),
                &publisher(),
                &identity(),
                &[bad.into()],
                1_000,
            );
            assert!(refused.is_err(), "{bad} is not a connection hint");
        }
        let too_many: Vec<String> = (0..33).map(|i| format!("tcp://10.0.0.{i}:1")).collect();
        assert!(
            lease::document(
                &parsed,
                &lock(),
                &publisher(),
                &identity(),
                &too_many,
                1_000
            )
            .is_err(),
            "the daemon drops an observation with more than 32 endpoints"
        );
    }
}

#[cfg(test)]
mod http_tests {
    use super::fixtures::{manifest, with_ui};
    use super::{HTTP_METHODS, Manifest, OriginAuth};

    fn claim(extra: &str) -> String {
        format!(
            "[http]\nhosts = [\"contacts.myc\"]\nroutes = [{{ methods = [\"GET\", \"POST\"], path = \"/\" }}]\n{extra}"
        )
    }

    #[test]
    fn a_spore_declares_where_it_answers() {
        let parsed = Manifest::parse(&manifest(&claim(""))).expect("claim parses");
        let http = parsed.http.expect("an [http] table");
        assert_eq!(http.hosts, vec!["contacts.myc"]);
        assert_eq!(http.max_body, 64 * 1024, "a body limit exists by default");
        assert_eq!(http.routes[0].path, "/");
        assert_eq!(http.routes[0].methods, vec!["GET", "POST"]);
        assert_eq!(http.origin_auth, OriginAuth::ProviderSignature);
        assert!(HTTP_METHODS.contains(&"PATCH"), "the method set is closed");
    }

    #[test]
    fn a_host_a_vhost_could_not_match_is_refused() {
        for bad in [
            "Contacts.myc",
            "contacts.myc:443",
            "https://contacts.myc",
            ".myc",
            "contacts.myc/",
        ] {
            let text = format!(
                "[http]\nhosts = [{bad:?}]\nroutes = [{{ methods = [\"GET\"], path = \"/\" }}]\n"
            );
            assert!(
                Manifest::parse(&manifest(&text)).is_err(),
                "{bad} is not a hostname a vhost can match"
            );
        }
    }

    #[test]
    fn a_route_needs_a_prefix_and_a_known_method() {
        for bad in ["contacts", "/a b", ""] {
            let text = format!(
                "[http]\nhosts = [\"contacts.myc\"]\nroutes = [{{ methods = [\"GET\"], path = {bad:?} }}]\n"
            );
            assert!(
                Manifest::parse(&manifest(&text)).is_err(),
                "{bad:?} is not a prefix"
            );
        }
        let unknown = "[http]\nhosts = [\"contacts.myc\"]\nroutes = [{ methods = [\"FETCH\"], path = \"/\" }]\n";
        assert!(
            Manifest::parse(&manifest(unknown)).is_err(),
            "an unknown method would silently never match"
        );
    }

    #[test]
    fn a_spore_without_a_module_cannot_claim_a_name() {
        let text = claim("");
        let bare = with_ui("[ui]\nentry = \"ui/main.lua\"\n", &text)
            .replace("[module]\npath = \"module.wasm\"\n", "");
        let refused = Manifest::parse(&bare);
        assert!(
            refused.is_err(),
            "a Spore must be able to answer on the name it claims"
        );
    }

    #[test]
    fn a_body_limit_past_a_megabyte_is_refused() {
        let text = format!("{}\nmax_body = {}\n", claim(""), super::MAX_HTTP_BODY + 1);
        assert!(Manifest::parse(&manifest(&text)).is_err());
    }
}
#[cfg(test)]
mod command_tests {
    use super::Manifest;
    use super::fixtures::manifest;

    const OK: &str = "[[commands]]\nname = \"unibus\"\nsummary = \"read the inbox\"\nmodule = \"bin/unibus.wasm\"\n";

    #[test]
    fn a_spore_installs_its_own_commands() {
        let parsed = Manifest::parse(&manifest(OK)).expect("command parses");
        assert_eq!(parsed.commands.len(), 1);
        assert_eq!(parsed.commands[0].name, "unibus");
        // The module travels with the package like any other declared path.
        assert!(
            parsed
                .declared_paths()
                .iter()
                .any(|path| path == std::path::Path::new("bin/unibus.wasm")),
            "a command module that is not locked could vanish from the package"
        );
    }

    #[test]
    fn a_command_a_shell_could_misread_is_refused() {
        for bad in [
            // A path, a flag, or an empty name: a shell resolves names, not paths.
            "[[commands]]\nname = \"bin/x\"\nsummary = \"s\"\nmodule = \"bin/x.wasm\"\n",
            "[[commands]]\nname = \"--force\"\nsummary = \"s\"\nmodule = \"bin/x.wasm\"\n",
            "[[commands]]\nname = \"\"\nsummary = \"s\"\nmodule = \"bin/x.wasm\"\n",
            // No summary means a shell cannot list what it would run.
            "[[commands]]\nname = \"x\"\nsummary = \"  \"\nmodule = \"bin/x.wasm\"\n",
            // A module must be a wasm component, not a script.
            "[[commands]]\nname = \"x\"\nsummary = \"s\"\nmodule = \"bin/x.lua\"\n",
        ] {
            assert!(Manifest::parse(&manifest(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn two_commands_cannot_share_a_name() {
        let twice = format!("{OK}{OK}");
        assert!(Manifest::parse(&manifest(&twice)).is_err());
    }
}
