//! The `canvas` ABI, as a view sees it.
//!
//! A view draws with normalized coordinates: the surface spans `-4..4` in both
//! axes, so a view never has to know the extent it will be granted, and the host
//! never has to reflow it.
//!
//! That is also the space the scene document uses, so this module passes the
//! coordinates through unchanged: a document rendered at 1200x44 and one
//! rendered at 3840x216 produce the same nodes, and the extent only reaches the
//! view as `module.viewport`. Converting to pixels here was the original bug —
//! a bar that filled a 1200-wide surface filled a quarter of it once the
//! renderer, which had its own idea of the extent, did the scaling.
//!
//! `module.viewport` and `module.theme` carry the same facts a Canvas module
//! gets. `widget.hit` exists and records nothing: a view that registers a hit
//! must not fail for it, and this host has no input path yet. When it does, the
//! hit table is where the mapping belongs — the document has no field for it.

use mlua::{Lua, Table};
use soil_scene::{Content, Node, Rect, Text, Transform};
use std::{cell::RefCell, rc::Rc};

use crate::{Error, Theme};

/// One draw call, before it is turned into a scene node.
///
/// Kept separate from [`Node`] so the ABI only has to read Lua, and the scene
/// only has to be built once: a view that names a colour badly fails in one
/// place with the primitive's id in the message.
pub(crate) enum Prim {
    Rect {
        id: String,
        ink: Option<String>,
        rgb: Option<[u8; 3]>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        opacity: f32,
        radius: f32,
    },
    Text {
        id: String,
        ink: Option<String>,
        rgb: Option<[u8; 3]>,
        text: String,
        x: f32,
        y: f32,
        /// Size relative to the host's body size. The ABI hands a view's chosen
        /// pixel size over as a ratio, and the renderer multiplies it back by
        /// `theme.body` — so passing it through is what makes a 13px label 13px.
        scale: f32,
    },
}

impl Prim {
    /// Reads a `canvas.rect` spec.
    ///
    /// Every field is optional except the id: a view that omits a coordinate
    /// meant zero, and a view that omits an ink meant "the host's default", which
    /// is a refusal here rather than a colour this host invents.
    pub(crate) fn rect(spec: &Table) -> mlua::Result<Self> {
        Ok(Self::Rect {
            id: identifier(spec)?,
            ink: spec.get("ink")?,
            rgb: colour(spec)?,
            x: coordinate(spec, "translation", 1)?,
            y: coordinate(spec, "translation", 2)?,
            w: coordinate(spec, "scale", 1)?,
            h: coordinate(spec, "scale", 2)?,
            opacity: spec.get::<Option<f32>>("opacity")?.unwrap_or(1.0),
            radius: spec.get::<Option<f32>>("radius")?.unwrap_or(0.0),
        })
    }

    /// Reads a `canvas.text` spec.
    pub(crate) fn text(spec: &Table) -> mlua::Result<Self> {
        Ok(Self::Text {
            id: identifier(spec)?,
            ink: spec.get("ink")?,
            rgb: colour(spec)?,
            text: spec.get::<Option<String>>("text")?.unwrap_or_default(),
            x: coordinate(spec, "translation", 1)?,
            y: coordinate(spec, "translation", 2)?,
            scale: coordinate_or(spec, "scale", 1, 1.0)?,
        })
    }

    /// The id this primitive was drawn under.
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Rect { id, .. } | Self::Text { id, .. } => id,
        }
    }

    /// The ink this primitive names, if it named one instead of a colour.
    pub(crate) fn ink(&self) -> Option<&str> {
        match self {
            Self::Rect { ink, .. } | Self::Text { ink, .. } => ink.as_deref(),
        }
    }

    /// Turns one primitive into a node in the normalized space the view drew in.
    pub(crate) fn into_node(self, theme: &Theme) -> Result<Node, Error> {
        match self {
            Self::Rect {
                id,
                ink,
                rgb,
                x,
                y,
                w,
                h,
                opacity,
                radius,
            } => {
                let rgb = resolve(&id, ink.as_deref(), rgb, theme)?;
                Ok(Node {
                    id,
                    parent: None,
                    transform: Transform {
                        translation: [x, y, 0.0],
                        rotation: [0.0; 3],
                        scale: [w, h, 1.0],
                    },
                    content: Content::Rect(Rect {
                        ink: None,
                        rgb: Some(rgb),
                        opacity,
                        radius,
                    }),
                    transition_ms: None,
                })
            }
            Self::Text {
                id,
                ink,
                rgb,
                text,
                x,
                y,
                scale,
            } => {
                let rgb = resolve(&id, ink.as_deref(), rgb, theme)?;
                Ok(Node {
                    id,
                    parent: None,
                    transform: Transform {
                        translation: [x, y, 0.0],
                        rotation: [0.0; 3],
                        // The size is a ratio, not the box size: text has no box
                        // in the document, and the renderer owns the font.
                        scale: [scale, scale, 1.0],
                    },
                    content: Content::Text(Text {
                        text,
                        ink: None,
                        rgb: Some(rgb),
                        opacity: 1.0,
                    }),
                    transition_ms: None,
                })
            }
        }
    }
}

/// Installs `canvas`, `module` and `widget` for one render.
pub(crate) fn install(
    lua: &Lua,
    extent: [f32; 2],
    max_primitives: usize,
    sink: Rc<RefCell<Vec<Prim>>>,
) -> Result<(), Error> {
    let canvas = lua.create_table().map_err(super::lua_error)?;
    let rects = Rc::clone(&sink);
    canvas
        .set(
            "rect",
            lua.create_function(move |_, spec: Table| {
                push(&rects, Prim::rect(&spec), max_primitives)
            })
            .map_err(super::lua_error)?,
        )
        .map_err(super::lua_error)?;
    let texts = Rc::clone(&sink);
    canvas
        .set(
            "text",
            lua.create_function(move |_, spec: Table| {
                push(&texts, Prim::text(&spec), max_primitives)
            })
            .map_err(super::lua_error)?,
        )
        .map_err(super::lua_error)?;
    canvas
        .set(
            "measure_text",
            lua.create_function(|lua, (text, style): (String, Option<Table>)| {
                let size = style
                    .and_then(|table| table.get::<Option<f32>>("size").ok().flatten())
                    .unwrap_or(10.0);
                // An estimate on purpose. A host with a font measures; this one
                // would have to load one to answer, and a view that lays out
                // against an estimate it can predict is better than a view that
                // refuses to draw. The shape is the ABI's: a table with `width`,
                // the way Canvas answers it, so a view takes the same branch in
                // both hosts.
                let measured = lua.create_table()?;
                measured.set("width", text.chars().count() as f32 * size * 0.55)?;
                measured.set("height", size * 1.2)?;
                Ok(measured)
            })
            .map_err(super::lua_error)?,
        )
        .map_err(super::lua_error)?;
    lua.globals().set("canvas", canvas).map_err(super::lua_error)?;

    let module = lua.create_table().map_err(super::lua_error)?;
    let viewport = lua.create_table().map_err(super::lua_error)?;
    viewport.set("width", extent[0]).map_err(super::lua_error)?;
    viewport.set("height", extent[1]).map_err(super::lua_error)?;
    module.set("viewport", viewport).map_err(super::lua_error)?;
    let theme = lua.create_table().map_err(super::lua_error)?;
    theme.set("body", 16.0).map_err(super::lua_error)?;
    theme.set("small", 13.0).map_err(super::lua_error)?;
    theme.set("micro", 10.0).map_err(super::lua_error)?;
    theme.set("radius", 10.0).map_err(super::lua_error)?;
    module.set("theme", theme).map_err(super::lua_error)?;
    lua.globals().set("module", module).map_err(super::lua_error)?;

    let widget = lua.create_table().map_err(super::lua_error)?;
    widget
        .set(
            "hit",
            lua.create_function(|_, _: mlua::MultiValue| Ok(()))
                .map_err(super::lua_error)?,
        )
        .map_err(super::lua_error)?;
    lua.globals().set("widget", widget).map_err(super::lua_error)?;
    Ok(())
}

fn push(
    sink: &Rc<RefCell<Vec<Prim>>>,
    prim: mlua::Result<Prim>,
    max: usize,
) -> mlua::Result<()> {
    let prim = prim?;
    let mut sink = sink.borrow_mut();
    if sink.len() >= max {
        return Err(mlua::Error::RuntimeError(format!(
            "a view may draw at most {max} primitives"
        )));
    }
    sink.push(prim);
    Ok(())
}

fn resolve(
    id: &str,
    ink: Option<&str>,
    rgb: Option<[u8; 3]>,
    theme: &Theme,
) -> Result<[u8; 3], Error> {
    if let Some(rgb) = rgb {
        return Ok(rgb);
    }
    match ink {
        Some(name) => theme.color(name).ok_or_else(|| {
            Error::invalid(format!(
                "{id:?} names ink {name:?}, which this theme does not define"
            ))
        }),
        None => Err(Error::invalid(format!(
            "{id:?} names neither an ink nor a colour"
        ))),
    }
}

fn identifier(spec: &Table) -> mlua::Result<String> {
    let id = spec.get::<Option<String>>("id")?;
    id.filter(|id| !id.is_empty()).ok_or_else(|| {
        mlua::Error::RuntimeError("every primitive needs an id".to_string())
    })
}

fn coordinate(spec: &Table, key: &str, index: usize) -> mlua::Result<f32> {
    coordinate_or(spec, key, index, 0.0)
}

fn coordinate_or(spec: &Table, key: &str, index: usize, fallback: f32) -> mlua::Result<f32> {
    match spec.get::<Option<Table>>(key)? {
        Some(pair) => pair.get::<f32>(index),
        None => Ok(fallback),
    }
}

fn colour(spec: &Table) -> mlua::Result<Option<[u8; 3]>> {
    let Some(table) = spec.get::<Option<Table>>("rgb")? else {
        return Ok(None);
    };
    Ok(Some([
        table.get::<u8>(1)?,
        table.get::<u8>(2)?,
        table.get::<u8>(3)?,
    ]))
}
