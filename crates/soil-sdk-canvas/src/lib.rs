//! Guest SDK for `soil:canvas` and `soil:scene`.
//!
//! This is the rendering half of the Soil SDK. A Spore describes what it wants
//! drawn as a versioned [`soil_scene::Present`] document and hands it to the
//! host-owned Canvas surface. The host validates the document, applies its
//! budget, and draws it; the guest never gets pixels, a window, a texture, or a
//! clock.
//!
//! ```ignore
//! use soil_sdk_canvas::Surface;
//!
//! let mut scene = soil_scene::Scene::new(Vec::new());
//! // build scene nodes ...
//! Surface::present_scene(&scene)?;
//! ```

wit_bindgen::generate!({
    path: "wit",
    world: "guest",
    generate_all,
});

use soil::canvas::surface as canvas_surface;
use soil::scene::payload as scene_payload;

pub use canvas_surface::{Event, Failure, Rgb, State, Theme, ThemeMode};
pub use scene_payload::Limits;

/// A failure from the Canvas capability.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the Spore has no Canvas grant")]
    Denied,
    #[error("the Canvas surface is not ready")]
    NotReady,
    #[error("the supplied generation is stale")]
    Stale,
    #[error("a Canvas present/input/frame budget was exceeded")]
    OverBudget,
    #[error("the scene document or event is invalid")]
    Invalid,
    #[error("the Canvas surface is closed")]
    Closed,
    #[error("the Canvas owner or platform presenter failed")]
    Backend,
    #[error(transparent)]
    Scene(#[from] soil_scene::Error),
}

impl From<Failure> for Error {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Denied => Self::Denied,
            Failure::NotReady => Self::NotReady,
            Failure::Stale => Self::Stale,
            Failure::OverBudget => Self::OverBudget,
            Failure::Invalid => Self::Invalid,
            Failure::Closed => Self::Closed,
            Failure::Backend => Self::Backend,
        }
    }
}

/// The one Canvas surface granted to this Spore.
///
/// These are associated functions rather than a resource because the WIT
/// contract grants exactly one surface per Spore. The host chooses its extent
/// and display; the Spore only presents and observes.
pub struct Surface;

impl Surface {
    /// Present a typed scene document.
    pub fn present(present: &soil_scene::Present) -> Result<State, Error> {
        let document = present.to_bytes()?;
        Self::present_bytes(&document)
    }

    /// Present an already-encoded `soil-scene` document.
    ///
    /// This is the escape hatch for a producer that streams bytes from a
    /// native renderer. The host still validates the document and refuses
    /// anything it cannot draw.
    pub fn present_bytes(document: &[u8]) -> Result<State, Error> {
        canvas_surface::present(document).map_err(Error::from)
    }

    /// Present a scene the host rasterizes or forwards to its compositor.
    pub fn present_scene(scene: &soil_scene::Scene) -> Result<State, Error> {
        Self::present(&soil_scene::Present::Scene(scene.clone()))
    }

    /// Present pixels the Spore rendered itself.
    pub fn present_rgba(frame: &soil_scene::RgbaFrame) -> Result<State, Error> {
        Self::present(&soil_scene::Present::Rgba(frame.clone()))
    }

    /// Present a lease on a texture the Spore rendered on the GPU.
    pub fn present_lease(lease: &soil_scene::GpuLease) -> Result<State, Error> {
        Self::present(&soil_scene::Present::GpuLease(lease.clone()))
    }

    /// Read the current owner state without presenting.
    pub fn observe() -> Result<State, Error> {
        canvas_surface::observe().map_err(Error::from)
    }

    /// Deliver one normalized event bound to the generation the Spore last saw.
    pub fn input(generation: u64, event: Event) -> Result<State, Error> {
        canvas_surface::input(generation, &event).map_err(Error::from)
    }

    /// Release the grant. Further calls fail with [`Error::Closed`].
    pub fn close() -> Result<(), Error> {
        canvas_surface::close().map_err(Error::from)
    }

    /// Whether the host can draw this scene schema.
    pub fn supports(schema: u32) -> bool {
        scene_payload::supports(schema)
    }

    /// The host's current node, text, payload, and extent budget.
    pub fn budget() -> Limits {
        scene_payload::budget()
    }

    /// The schema version this SDK emits.
    pub const SCENE_VERSION: u32 = soil_scene::SCENE_VERSION;
}

#[cfg(test)]
mod tests {
    const CANVAS_WIT: &str = include_str!("../../../wit/soil-canvas.wit");
    const SCENE_WIT: &str = include_str!("../../../wit/soil-scene.wit");

    use super::*;
    use soil_scene::{Content, Node, Rect, Scene, Transform};

    #[test]
    fn pins_the_canvas_and_scene_contracts() {
        assert!(CANVAS_WIT.contains("package soil:canvas@1.0.0;"));
        assert!(CANVAS_WIT.contains("present: func(document: list<u8>)"));
        assert!(CANVAS_WIT.contains("observe: func()"));
        assert!(SCENE_WIT.contains("package soil:scene@1.0.0;"));
        assert!(SCENE_WIT.contains("supports: func(schema: u32) -> bool;"));
        assert!(SCENE_WIT.contains("budget: func() -> limits;"));
    }

    #[test]
    fn present_encodes_a_scene_document() {
        let scene = Scene::new(vec![Node {
            id: "panel".into(),
            parent: None,
            transform: Transform {
                scale: [100.0, 40.0, 1.0],
                ..Transform::default()
            },
            content: Content::Rect(Rect {
                ink: Some(soil_scene::Ink::Surface),
                ..Rect::default()
            }),
            transition_ms: None,
        }]);
        let bytes = soil_scene::Present::Scene(scene)
            .to_bytes()
            .expect("scene document");
        let decoded = soil_scene::Present::from_bytes(&bytes).expect("valid scene");
        assert!(matches!(decoded, soil_scene::Present::Scene(_)));
    }

    #[test]
    fn parses_the_lua_ui_document_shape() {
        // This is the shape `fungos.ui` / `sdk/ui/ui.lua` writes. The two SDKs
        // share the wire document even though one is Rust and one is Lua.
        let document = br#"{
          "kind": "scene",
          "schema": 1,
          "nodes": [
            {
              "id": "e1",
              "transform": {
                "translation": [-4, 4, -1],
                "rotation": [0, 0, 0],
                "scale": [8, 8, 1]
              },
              "type": "rect",
              "ink": "background",
              "opacity": 1,
              "radius": 0
            }
          ]
        }"#;
        let present = soil_scene::Present::from_bytes(document).expect("Lua document");
        let soil_scene::Present::Scene(scene) = present else {
            panic!("Lua document is a scene");
        };
        assert_eq!(scene.nodes.len(), 1);
        assert!(matches!(
            scene.nodes[0].content,
            soil_scene::Content::Rect(soil_scene::Rect {
                ink: Some(soil_scene::Ink::Background),
                ..
            })
        ));
    }

    #[test]
    fn failure_taxonomy_is_not_collapsed() {
        for (failure, message) in [
            (Failure::Denied, "Canvas grant"),
            (Failure::NotReady, "not ready"),
            (Failure::Stale, "stale"),
            (Failure::OverBudget, "budget"),
            (Failure::Invalid, "invalid"),
            (Failure::Closed, "closed"),
            (Failure::Backend, "presenter failed"),
        ] {
            let error = Error::from(failure);
            assert!(error.to_string().contains(message), "{error}");
        }
    }
}
