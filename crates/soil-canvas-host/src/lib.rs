//! Host adapter for `soil:canvas` and `soil:scene`.
//!
//! This is the bridge between the Soil SDK and a real owner surface. A Spore
//! presents a versioned `soil-scene` document; this adapter validates it against
//! the same budget the compositor will enforce and hands it to a [`Presenter`].
//! The shipped [`socket::CanvasSocket`] presenter forwards the document to
//! Canvas's producer socket, so the compositor draws it with the same renderer
//! it uses for every other Spore surface.
//!
//! The adapter deliberately does not own a rasterizer. A host that wants to
//! render without Canvas implements [`Presenter`] with a native renderer; the
//! capability, validation, generation, and budget rules stay identical.

use std::fmt;

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "wit",
    world: "canvas-guest",
});

use soil::canvas::surface as api;
use soil::scene::payload as scene_api;

#[cfg(unix)]
pub mod socket;
#[cfg(unix)]
pub use socket::CanvasSocket;

/// A host-side presenter. The presenter owns transport and rendering; the
/// adapter owns the capability contract.
pub trait Presenter: Send {
    /// Present one validated document and return the owner generation.
    fn present(&mut self, present: &soil_scene::Present) -> Result<u64, PresenterError>;

    /// Deliver one normalized event to the owner, when the owner accepts input
    /// through this adapter. Canvas delivers producer input out of band, so the
    /// default is intentionally a no-op.
    fn input(&mut self, _event: &api::Event) -> Result<(), PresenterError> {
        Ok(())
    }

    /// Release the owner surface.
    fn close(&mut self) -> Result<(), PresenterError> {
        Ok(())
    }
}

/// A presenter failure that maps onto the `soil:canvas` failure taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenterError {
    OverBudget,
    Invalid,
    Backend,
}

impl fmt::Display for PresenterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OverBudget => "presenter budget exceeded",
            Self::Invalid => "presenter refused an invalid document",
            Self::Backend => "presenter backend failed",
        })
    }
}

impl std::error::Error for PresenterError {}

/// One granted Canvas surface.
///
/// A host creates exactly one of these per Spore instance. `reset` starts a
/// fresh invocation budget; a guest that presents twice in one invocation is
/// refused rather than allowed to build a frame queue.
pub struct CanvasHost {
    presenter: Box<dyn Presenter>,
    state: api::State,
    limits: scene_api::Limits,
    max_presents: usize,
    max_inputs: usize,
    presents: usize,
    inputs: usize,
    closed: bool,
    table: ResourceTable,
    wasi: WasiCtx,
}

impl CanvasHost {
    /// Create a surface with the host-selected extent and theme.
    pub fn new(
        presenter: impl Presenter + 'static,
        width: u32,
        height: u32,
        scale: f32,
        theme: api::Theme,
    ) -> Self {
        let limits = scene_api::Limits {
            max_nodes: soil_scene::MAX_NODES as u32,
            max_text_bytes: soil_scene::MAX_TEXT_BYTES as u32,
            max_payload_bytes: soil_scene::MAX_PAYLOAD_BYTES as u32,
            width,
            height,
            scale,
        };
        Self {
            presenter: Box::new(presenter),
            state: api::State {
                generation: 0,
                width,
                height,
                scale,
                focused: true,
                visible: true,
                theme,
            },
            limits,
            max_presents: 1,
            max_inputs: 1,
            presents: 0,
            inputs: 0,
            closed: false,
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new()
                .inherit_stdout()
                .inherit_stderr()
                .build(),
        }
    }

    /// Start a new guest invocation. Budgets are per invocation, not per host.
    pub fn reset(&mut self) {
        self.presents = 0;
        self.inputs = 0;
    }

    pub fn state(&self) -> &api::State {
        &self.state
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn check_open(&self) -> Result<(), api::Failure> {
        if self.closed {
            Err(api::Failure::Closed)
        } else {
            Ok(())
        }
    }
}

impl WasiView for CanvasHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// A component runner for Spores that use `soil:canvas`.
pub struct CanvasRuntime {
    engine: Engine,
    linker: Linker<CanvasHost>,
}

impl CanvasRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        add_to_linker(&mut linker)?;
        Ok(Self { engine, linker })
    }

    /// Run a component's exported `run() -> result<u32, string>` with one
    /// granted Canvas surface. The host resets per-invocation budgets before
    /// each call; the presenter retains the owner connection across calls.
    pub fn run(&self, bytes: &[u8], host: CanvasHost) -> anyhow::Result<u32> {
        let component = Component::new(&self.engine, bytes)?;
        let mut store = Store::new(&self.engine, host);
        let bindings = CanvasGuest::instantiate(&mut store, &component, &self.linker)?;
        bindings.call_run(&mut store)?.map_err(anyhow::Error::msg)
    }
}

impl api::Host for CanvasHost {
    fn present(&mut self, document: Vec<u8>) -> Result<api::State, api::Failure> {
        self.check_open()?;
        if self.presents >= self.max_presents {
            return Err(api::Failure::OverBudget);
        }
        if document.len() > self.limits.max_payload_bytes as usize {
            return Err(api::Failure::OverBudget);
        }
        let present =
            soil_scene::Present::from_bytes(&document).map_err(|_| api::Failure::Invalid)?;
        self.presents += 1;
        let generation = self
            .presenter
            .present(&present)
            .map_err(presenter_failure)?;
        self.state.generation = self.state.generation.max(generation);
        Ok(self.state)
    }

    fn observe(&mut self) -> Result<api::State, api::Failure> {
        self.check_open()?;
        Ok(self.state)
    }

    fn input(&mut self, generation: u64, event: api::Event) -> Result<api::State, api::Failure> {
        self.check_open()?;
        if generation != self.state.generation {
            return Err(api::Failure::Stale);
        }
        if self.inputs >= self.max_inputs {
            return Err(api::Failure::OverBudget);
        }
        self.inputs += 1;
        self.presenter.input(&event).map_err(presenter_failure)?;
        Ok(self.state)
    }

    fn close(&mut self) -> Result<(), api::Failure> {
        self.check_open()?;
        self.presenter.close().map_err(presenter_failure)?;
        self.closed = true;
        Ok(())
    }
}

impl scene_api::Host for CanvasHost {
    fn supports(&mut self, schema: u32) -> bool {
        schema == soil_scene::SCENE_VERSION
    }

    fn budget(&mut self) -> scene_api::Limits {
        self.limits
    }
}

fn presenter_failure(error: PresenterError) -> api::Failure {
    match error {
        PresenterError::OverBudget => api::Failure::OverBudget,
        PresenterError::Invalid => api::Failure::Invalid,
        PresenterError::Backend => api::Failure::Backend,
    }
}

/// Add the Canvas and scene-payload imports to a component linker.
pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: api::Host + scene_api::Host + 'static,
{
    api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    scene_api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use soil_scene::{Content, Node, Rect, Scene, Transform};

    #[derive(Clone, Default)]
    struct RecordingPresenter {
        seen: Arc<Mutex<Vec<soil_scene::Present>>>,
        generation: u64,
    }

    impl Presenter for RecordingPresenter {
        fn present(&mut self, present: &soil_scene::Present) -> Result<u64, PresenterError> {
            self.seen.lock().unwrap().push(present.clone());
            self.generation += 1;
            Ok(self.generation)
        }
    }

    fn theme() -> api::Theme {
        api::Theme {
            mode: api::ThemeMode::Dark,
            background: api::Rgb {
                r: 18,
                g: 20,
                b: 24,
            },
            surface: api::Rgb {
                r: 26,
                g: 29,
                b: 35,
            },
            text: api::Rgb {
                r: 232,
                g: 235,
                b: 240,
            },
            muted: api::Rgb {
                r: 148,
                g: 156,
                b: 168,
            },
            primary: api::Rgb {
                r: 86,
                g: 156,
                b: 255,
            },
            success: api::Rgb {
                r: 80,
                g: 200,
                b: 120,
            },
            warning: api::Rgb {
                r: 240,
                g: 180,
                b: 60,
            },
            danger: api::Rgb {
                r: 240,
                g: 90,
                b: 90,
            },
            info: api::Rgb {
                r: 90,
                g: 190,
                b: 240,
            },
            micro: 12.0,
            small: 13.0,
            body: 15.0,
            large: 18.0,
            title: 22.0,
            display: 28.0,
            hero: 36.0,
            space_two: 8.0,
            radius: 8.0,
        }
    }

    fn document() -> Vec<u8> {
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
        soil_scene::Present::Scene(scene).to_bytes().unwrap()
    }

    fn host() -> CanvasHost {
        CanvasHost::new(RecordingPresenter::default(), 1200, 840, 1.0, theme())
    }

    #[test]
    fn presents_a_valid_document_and_returns_owner_generation() {
        let mut host = host();
        let state = api::Host::present(&mut host, document()).unwrap();
        assert_eq!(state.generation, 1);
        assert_eq!(state.width, 1200);
        assert_eq!(state.height, 840);
    }

    #[test]
    fn invalid_documents_do_not_reach_the_presenter() {
        let mut host = host();
        let error = api::Host::present(&mut host, b"not json".to_vec()).unwrap_err();
        assert_eq!(error, api::Failure::Invalid);
        assert!(host.presents == 0);
    }

    #[test]
    fn a_second_present_in_one_invocation_is_over_budget() {
        let mut host = host();
        api::Host::present(&mut host, document()).unwrap();
        let error = api::Host::present(&mut host, document()).unwrap_err();
        assert_eq!(error, api::Failure::OverBudget);
        host.reset();
        api::Host::present(&mut host, document()).unwrap();
    }

    #[test]
    fn stale_generations_are_refused() {
        let mut host = host();
        let event = api::Event {
            key: soil::input::types::Key::Down,
            modifiers: soil::input::types::Modifiers {
                shift: false,
                control: false,
                alt: false,
                meta: false,
            },
        };
        assert_eq!(
            api::Host::input(&mut host, 99, event.clone()).unwrap_err(),
            api::Failure::Stale
        );
        api::Host::present(&mut host, document()).unwrap();
        let state = api::Host::input(&mut host, 1, event).unwrap();
        assert_eq!(state.generation, 1);
    }

    #[test]
    fn close_revokes_the_grant() {
        let mut host = host();
        api::Host::close(&mut host).unwrap();
        assert!(host.is_closed());
        assert_eq!(
            api::Host::present(&mut host, document()).unwrap_err(),
            api::Failure::Closed
        );
        assert_eq!(
            api::Host::close(&mut host).unwrap_err(),
            api::Failure::Closed
        );
    }

    #[test]
    fn scene_budget_matches_the_document_ceiling() {
        let mut host = host();
        assert!(scene_api::Host::supports(
            &mut host,
            soil_scene::SCENE_VERSION
        ));
        assert!(!scene_api::Host::supports(
            &mut host,
            soil_scene::SCENE_VERSION + 1
        ));
        let budget = scene_api::Host::budget(&mut host);
        assert_eq!(budget.max_nodes as usize, soil_scene::MAX_NODES);
        assert_eq!(
            budget.max_payload_bytes as usize,
            soil_scene::MAX_PAYLOAD_BYTES
        );
        assert_eq!((budget.width, budget.height), (1200, 840));
    }

    #[test]
    fn links_the_canvas_world() {
        let engine = wasmtime::Engine::new(&wasmtime::Config::new()).unwrap();
        let mut linker: Linker<CanvasHost> = Linker::new(&engine);
        add_to_linker(&mut linker).unwrap();
        CanvasRuntime::new().unwrap();
    }

    #[test]
    fn pins_the_canvas_and_scene_contracts() {
        let canvas = include_str!("../../../wit/soil-canvas.wit");
        let scene = include_str!("../../../wit/soil-scene.wit");
        assert!(canvas.contains("package soil:canvas@1.0.0;"));
        assert!(canvas.contains("present: func(document: list<u8>)"));
        assert!(scene.contains("package soil:scene@1.0.0;"));
        assert!(scene.contains("budget: func() -> limits;"));
    }
}
