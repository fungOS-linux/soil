//! Run a Lua Spore in Soil.
//!
//! `[engine].target = "soil"` names a Spore with no wasm: the host runs its Lua
//! directly. This crate is that host's first half — read the package with the same
//! reader everything else uses, hold the VM to the same limits Canvas holds its
//! own modules to, and turn what the view drew into a [`soil_scene::Scene`] that
//! the existing renderers already understand.
//!
//! The seam is the `canvas` ABI, because that is what a view already targets:
//!
//! ```lua
//! canvas.rect { id = "bg", translation = { -4, 0 }, scale = { 8, 4 }, ink = "background" }
//! module.viewport = { width = 1200, height = 44 }
//! ```
//!
//! Coordinates are normalized (`-4..4` across the surface) because that is
//! Canvas's convention and a view should not have to reflow when the host grants
//! a different extent. That is also the space the document carries, so the host
//! keeps the coordinates exactly as drawn and the extent only reaches the view
//! as `module.viewport`.
//!
//! Four rules a caller will trip over, all of them deliberate:
//!
//! - **Ink is resolved by the host.** A view names `"accent"`; a name the theme
//!   does not define is a refusal that names the primitive, never a default
//!   colour. Silent substitution is how two renders of the same view disagree.
//! - **A duplicate node id is a refusal.** The document requires unique ids and
//!   the SDK has `surface:unique` for exactly this. Renaming one here would hide
//!   a real view bug behind output that looks fine.
//! - **A view that draws nothing is a refusal.** Painting at load or returning a
//!   module with `render` are both supported; neither is a placeholder success.
//! - **`require`, `io`, `os`, `load`, `dofile` and `debug` do not exist.** A
//!   composed artifact is self-contained. One that needs `require` was not
//!   composed, and the error says so instead of failing inside the SDK.
//!
//! What this deliberately does not do: verify the package lock (that is
//! `soil-spore verify`, once at install — hashing every file per render would buy
//! nothing a verified install has not already bought), or route input (the
//! document has no hit field yet; `widget.hit` is accepted and recorded nowhere).

mod abi;

use mlua::{Lua, LuaOptions, LuaSerdeExt, StdLib, Value, VmState};
use soil_scene::{Present, Scene};
use soil_spore::Manifest;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// The `[engine].target` value this host runs.
///
/// Named once, here, because it is the whole discriminator: a package either
/// names a wasm triple and belongs to a wasm host, or names this and belongs to
/// [`Spore`].
pub const TARGET: &str = "soil";

/// The document's own ceiling. A view that outgrows the scene has outgrown the
/// renderer, and finding that out at load is cheaper than at paint.
pub const MAX_PRIMITIVES: usize = soil_scene::MAX_NODES;

const DEFAULT_MEMORY_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_INTERRUPTS: u64 = 100_000;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;

/// The only failure this crate produces: something a host should not draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<soil_spore::Error> for Error {
    fn from(error: soil_spore::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<soil_scene::Error> for Error {
    fn from(error: soil_scene::Error) -> Self {
        Self(error.to_string())
    }
}

pub(crate) fn lua_error(error: mlua::Error) -> Error {
    Error::invalid(error.to_string())
}

/// Host-controlled limits for one render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_memory_bytes: usize,
    /// Maximum VM interrupt callbacks. The budget that stops a runaway loop.
    pub max_interrupts: u64,
    pub max_primitives: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_memory_bytes: DEFAULT_MEMORY_BYTES,
            max_interrupts: DEFAULT_INTERRUPTS,
            max_primitives: MAX_PRIMITIVES,
        }
    }
}

/// A resolved palette: the host's answer to a view's ink name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    inks: BTreeMap<String, [u8; 3]>,
}

impl Theme {
    /// A complete, neutral palette, so a host can render before it has an
    /// identity of its own. Paper, because a bar that ships with a dark default
    /// looks broken on a light desktop and vice versa.
    #[must_use]
    pub fn new() -> Self {
        let inks = [
            ("background", [0xf5, 0xf1, 0xe8]),
            ("surface", [0xff, 0xff, 0xff]),
            ("surface-raised", [0xfb, 0xf7, 0xef]),
            ("surface-recessed", [0xe8, 0xe2, 0xd5]),
            ("text", [0x1c, 0x1a, 0x17]),
            ("muted", [0x6b, 0x64, 0x59]),
            ("accent", [0xb5, 0x65, 0x1d]),
            ("primary", [0xb5, 0x65, 0x1d]),
            ("secondary", [0x5b, 0x7d, 0x8c]),
            ("success", [0x2f, 0x7d, 0x4f]),
            ("warning", [0xb0, 0x7d, 0x1f]),
            ("danger", [0xb3, 0x40, 0x2f]),
            ("info", [0x2f, 0x6b, 0x9d]),
            ("outline", [0xd8, 0xd2, 0xc4]),
            // The SDK's word for the same hairline `outline` names in the scene
            // vocabulary. Two names, one colour: a view should not have to learn
            // which of the two a given host happens to carry.
            ("border", [0xd8, 0xd2, 0xc4]),
            ("shadow", [0x00, 0x00, 0x00]),
            ("highlight", [0xff, 0xff, 0xff]),
            ("meter-low", [0x4b, 0x8f, 0x6a]),
            ("meter-high", [0xc4, 0x6a, 0x2f]),
        ];
        Self {
            inks: inks
                .into_iter()
                .map(|(name, rgb)| (name.to_string(), rgb))
                .collect(),
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            inks: BTreeMap::new(),
        }
    }

    /// Replaces or adds one ink.
    pub fn set(&mut self, name: impl Into<String>, rgb: [u8; 3]) -> &mut Self {
        self.inks.insert(name.into(), rgb);
        self
    }

    #[must_use]
    pub fn color(&self, ink: &str) -> Option<[u8; 3]> {
        self.inks.get(ink).copied()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inks.is_empty()
    }

    /// Reads a theme file: `return { name = "Paper", inks = { accent = "#b5651d" } }`.
    ///
    /// The same shape the preview tooling takes, so one theme file previews a
    /// view and runs it. Hex only: a palette a human cannot read is a palette
    /// nobody edits.
    pub fn from_source(source: &str) -> Result<Self, Error> {
        let lua = Lua::new_with(
            StdLib::TABLE | StdLib::STRING,
            LuaOptions::default(),
        )
        .map_err(lua_error)?;
        lua.sandbox(true).map_err(lua_error)?;
        let value: Value = lua
            .load(source)
            .set_name("theme")
            .eval()
            .map_err(lua_error)?;
        let table = match value {
            Value::Table(table) => table,
            _ => return Err(Error::invalid("a theme file must return a table")),
        };
        let Some(inks) = table.get::<Option<mlua::Table>>("inks").map_err(lua_error)? else {
            return Err(Error::invalid("a theme file must define `inks`"));
        };
        let mut theme = Self::empty();
        for pair in inks.pairs::<String, String>() {
            let (name, value) = pair.map_err(lua_error)?;
            let rgb = hex(&value).ok_or_else(|| {
                Error::invalid(format!("ink {name:?} is {value:?}, not #rrggbb"))
            })?;
            theme.set(name, rgb);
        }
        if theme.is_empty() {
            return Err(Error::invalid("a theme file must define at least one ink"));
        }
        Ok(theme)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new()
    }
}

/// A Lua Spore, read off disk and checked against its manifest.
///
/// The source is held, not the tree: a package that changes under a running host
/// is a reload decision the host makes, not something that takes effect mid-frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spore {
    id: String,
    version: String,
    entry: String,
    source: String,
    limits: Limits,
}

impl Spore {
    /// Loads a package's `[ui] entry`.
    pub fn load(root: &Path) -> Result<Self, Error> {
        Self::load_region(root, None)
    }

    /// Loads the Lua a chrome `region` draws.
    ///
    /// A per-region artifact in `[chrome.sources]` wins; otherwise the Spore's
    /// single `[ui] entry` is the region's Lua. That precedence is
    /// [`Manifest::region_source`]'s, not a second rule kept in step by hand.
    pub fn load_region(root: &Path, region: Option<&str>) -> Result<Self, Error> {
        let manifest = Manifest::load(root)?;
        manifest.validate()?;
        if manifest.engine.target != TARGET {
            return Err(Error::invalid(format!(
                "{} names [engine].target = {:?}; this host runs only {TARGET:?}",
                manifest.spore.id, manifest.engine.target
            )));
        }
        let entry = match region {
            Some(region) => {
                let Some(chrome) = manifest.chrome.as_ref() else {
                    return Err(Error::invalid(format!(
                        "{} draws no chrome region, and {region:?} was requested",
                        manifest.spore.id
                    )));
                };
                if !chrome.regions.iter().any(|claimed| claimed == region) {
                    return Err(Error::invalid(format!(
                        "{} claims regions {:?}, not {region:?}",
                        manifest.spore.id, chrome.regions
                    )));
                }
                manifest.region_source(region).map(str::to_owned)
            }
            None => manifest.ui.as_ref().map(|ui| ui.entry.clone()),
        }
        .ok_or_else(|| {
            Error::invalid(format!("{} has no Lua to run", manifest.spore.id))
        })?;

        let relative = soil_spore::safe_relative(&entry)?;
        let source = std::fs::read_to_string(root.join(&relative))
            .map_err(|error| Error::invalid(format!("{entry}: {error}")))?;
        if source.len() > MAX_SOURCE_BYTES {
            return Err(Error::invalid(format!(
                "{entry} is {} bytes, over the {MAX_SOURCE_BYTES} ceiling",
                source.len()
            )));
        }
        Ok(Self {
            id: manifest.spore.id,
            version: manifest.spore.version,
            entry,
            source,
            limits: Limits::default(),
        })
    }

    /// Replaces the render limits.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The package-relative path that will run.
    #[must_use]
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Runs the view once and returns the document it drew.
    ///
    /// `extent` is the surface the host granted. It reaches the view as
    /// `module.viewport` and bounds `measure_text`; it deliberately does not
    /// rescale the document, so the same view renders at any size.
    pub fn render(
        &self,
        snapshot: &serde_json::Value,
        theme: &Theme,
        extent: [f32; 2],
    ) -> Result<Scene, Error> {
        if extent.iter().any(|value| !value.is_finite()) || extent[0] <= 0.0 || extent[1] <= 0.0
        {
            return Err(Error::invalid("extent must be two positive, finite pixels"));
        }

        let lua = Lua::new_with(
            StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8,
            LuaOptions::default(),
        )
        .map_err(lua_error)?;
        lua.set_memory_limit(self.limits.max_memory_bytes)
            .map_err(lua_error)?;
        remove_ambient_globals(&lua)?;

        let interrupts = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&interrupts);
        let budget = self.limits.max_interrupts;
        lua.set_interrupt(move |_| {
            let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
            if count > budget {
                Err(mlua::Error::RuntimeError(
                    "the view exceeded its interrupt budget".to_string(),
                ))
            } else {
                Ok(VmState::Continue)
            }
        });

        let primitives = Rc::new(RefCell::new(Vec::new()));
        abi::install(
            &lua,
            extent,
            self.limits.max_primitives,
            Rc::clone(&primitives),
        )?;
        lua.sandbox(true).map_err(lua_error)?;

        let loaded: Value = lua
            .load(&self.source)
            .set_name(self.entry.clone())
            .eval()
            .map_err(|error| self.describe(error))?;
        if let Value::Table(module) = loaded {
            let render: Option<mlua::Function> =
                module.get("render").map_err(|error| self.describe(error))?;
            if let Some(render) = render {
                let snapshot = lua
                    .to_value(snapshot)
                    .map_err(|error| self.describe(error))?;
                render
                    .call::<()>(snapshot)
                    .map_err(|error| self.describe(error))?;
            }
        }
        drop(lua);

        let primitives = Rc::try_unwrap(primitives)
            .map_err(|_| Error::invalid("the view retained its primitive buffer"))?
            .into_inner();
        if primitives.is_empty() {
            return Err(Error::invalid(format!(
                "{} drew nothing: paint at load, or return a module with render(snapshot)",
                self.entry
            )));
        }

        let mut seen = BTreeSet::new();
        let mut nodes = Vec::with_capacity(primitives.len());
        // Refuse the whole vocabulary at once. Reporting one missing ink per run
        // turns a theme gap into a guessing game, which is the opposite of what
        // a refusal is for — and each name carries the primitives that used it,
        // so the fix is one edit rather than a search.
        let mut missing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for primitive in &primitives {
            if let Some(ink) = primitive.ink()
                && theme.color(ink).is_none()
            {
                missing.entry(ink).or_default().push(primitive.id());
            }
        }
        if !missing.is_empty() {
            let detail = missing
                .iter()
                .map(|(ink, ids)| format!("{ink} ({})", ids.join(", ")))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::invalid(format!(
                "the view names inks this theme does not define: {detail}"
            )));
        }
        for primitive in primitives {
            let node = primitive.into_node(theme)?;
            if !seen.insert(node.id.clone()) {
                return Err(Error::invalid(format!(
                    "duplicate node id {:?}: views namespace with surface:unique",
                    node.id
                )));
            }
            nodes.push(node);
        }
        let scene = Scene::new(nodes);
        scene.validate()?;
        Ok(scene)
    }

    fn describe(&self, error: mlua::Error) -> Error {
        Error::invalid(format!("{}: {error}", self.entry))
    }

    /// Runs the view and returns the payload a host presents.
    ///
    /// [`Present`] is what `soil:canvas.present` takes, so a host can hand this
    /// to a renderer or `Present::to_bytes` without rewrapping it. [`Self::render`]
    /// stays for callers that want the scene itself.
    pub fn present(
        &self,
        snapshot: &serde_json::Value,
        theme: &Theme,
        extent: [f32; 2],
    ) -> Result<Present, Error> {
        Ok(Present::Scene(self.render(snapshot, theme, extent)?))
    }
}

/// Removes the globals a view has no business reaching for.
///
/// The same list, for the same reason, as Canvas's own Lua host: one dialect and
/// one set of restrictions means a view cannot pass in one host and fail in the
/// other for a reason the view's author cannot see.
fn remove_ambient_globals(lua: &Lua) -> Result<(), Error> {
    let globals = lua.globals();
    for name in [
        "collectgarbage",
        "debug",
        "dofile",
        "getfenv",
        "io",
        "load",
        "loadfile",
        "loadstring",
        "os",
        "package",
        "require",
        "setfenv",
    ] {
        globals.set(name, Value::Nil).map_err(lua_error)?;
    }
    if let Ok(math) = globals.get::<mlua::Table>("math") {
        math.set("random", Value::Nil).map_err(lua_error)?;
        math.set("randomseed", Value::Nil).map_err(lua_error)?;
    }
    Ok(())
}

fn hex(value: &str) -> Option<[u8; 3]> {
    let trimmed = value.trim();
    let bytes = trimmed.strip_prefix('#')?;
    if bytes.len() != 6 || !bytes.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&bytes[0..2], 16).ok()?,
        u8::from_str_radix(&bytes[2..4], 16).ok()?,
        u8::from_str_radix(&bytes[4..6], 16).ok()?,
    ])
}
