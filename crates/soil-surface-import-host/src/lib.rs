//! Host implementation of `soil:surface-import`.
//!
//! A source adapter owns the native view, remote transport, decoder, or Spore
//! render target. The Spore sees an opaque handle, damage/frame events, and
//! normalized input. The host owns compositing and presentation.

use std::time::Duration;

use wasmtime::component::{HasSelf, Linker, Resource, ResourceTable};

wasmtime::component::bindgen!({
    path: "wit",
    world: "host",
    with: {
        "soil:surface-import/surface.surface": SurfaceResource,
    },
});

use soil::input::types::Key;
use soil::surface_import::surface as api;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceError {
    Denied,
    UnknownSource,
    NotReady,
    Unsupported,
    OverBudget,
    Invalid,
    Closed,
    Backend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Native,
    Remote,
    Media,
    Spore,
    Retained,
    /// A web page in a host-owned browser view. The adapter owns the engine;
    /// the Spore still sees only a handle.
    Browser,
}

#[derive(Debug, Clone)]
pub struct Info {
    pub kind: Kind,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub color_space: String,
    pub alpha: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub id: u64,
    pub pts_ms: u64,
    pub damage: Vec<Rect>,
    pub keyframe: bool,
}

#[derive(Debug, Clone)]
pub enum Event {
    Resized(Info),
    Frame(Frame),
    Damaged(Vec<Rect>),
    Idle,
    Closed,
}

#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Primary,
    Secondary,
    Middle,
}

#[derive(Debug, Clone)]
pub enum Input {
    PointerMove(Point),
    PointerButton(Button, bool, Point),
    Scroll(f32, f32, Point),
    Key(String, Modifiers, bool),
    Text(String),
    Focus(bool),
}

/// A host-side source adapter. Platform view import, VST3 hosting, remote
/// surfaces, media decode, and Spore render targets implement this outside the
/// guest ABI.
pub trait SurfaceSource: Send + Sync {
    fn open(&self, source_id: &str) -> Result<Box<dyn SurfaceSession>, SurfaceError>;
}

pub trait SurfaceSession: Send {
    fn info(&mut self) -> Result<Info, SurfaceError>;
    fn next(&mut self, timeout: Duration) -> Result<Option<Event>, SurfaceError>;
    fn input(&mut self, event: Input) -> Result<(), SurfaceError>;
    fn close(&mut self) -> Result<(), SurfaceError>;
}

pub struct SurfaceResource {
    session: Box<dyn SurfaceSession>,
    closed: bool,
}

pub struct SurfaceHost {
    table: ResourceTable,
    source: Box<dyn SurfaceSource>,
}

impl SurfaceHost {
    pub fn new(source: impl SurfaceSource + 'static) -> Self {
        Self {
            table: ResourceTable::new(),
            source: Box::new(source),
        }
    }
}

impl api::Host for SurfaceHost {
    fn open(&mut self, source_id: String) -> Result<Resource<SurfaceResource>, api::Error> {
        let session = self.source.open(&source_id).map_err(error)?;
        self.table
            .push(SurfaceResource {
                session,
                closed: false,
            })
            .map_err(|_| api::Error::OverBudget)
    }
}

impl api::HostSurface for SurfaceHost {
    fn info(&mut self, surface: Resource<SurfaceResource>) -> Result<api::Info, api::Error> {
        let surface = self
            .table
            .get_mut(&surface)
            .map_err(|_| api::Error::Closed)?;
        if surface.closed {
            return Err(api::Error::Closed);
        }
        surface.session.info().map(api_info).map_err(error)
    }

    fn next(
        &mut self,
        surface: Resource<SurfaceResource>,
        timeout_ms: u32,
    ) -> Result<Option<api::Event>, api::Error> {
        let surface = self
            .table
            .get_mut(&surface)
            .map_err(|_| api::Error::Closed)?;
        if surface.closed {
            return Err(api::Error::Closed);
        }
        surface
            .session
            .next(Duration::from_millis(u64::from(timeout_ms)))
            .map(|event| event.map(api_event))
            .map_err(error)
    }

    fn input(
        &mut self,
        surface: Resource<SurfaceResource>,
        event: api::Input,
    ) -> Result<(), api::Error> {
        let surface = self
            .table
            .get_mut(&surface)
            .map_err(|_| api::Error::Closed)?;
        if surface.closed {
            return Err(api::Error::Closed);
        }
        surface.session.input(input(event)).map_err(error)
    }

    fn close(&mut self, surface: Resource<SurfaceResource>) -> Result<(), api::Error> {
        let mut surface = self.table.delete(surface).map_err(|_| api::Error::Closed)?;
        if !surface.closed {
            surface.closed = true;
            surface.session.close().map_err(error)?;
        }
        Ok(())
    }

    fn drop(&mut self, surface: Resource<SurfaceResource>) -> Result<(), wasmtime::Error> {
        let Ok(mut surface) = self.table.delete(surface) else {
            return Ok(());
        };
        if !surface.closed {
            surface.closed = true;
            let _ = surface.session.close();
        }
        Ok(())
    }
}

fn api_info(info: Info) -> api::Info {
    api::Info {
        kind: match info.kind {
            Kind::Native => api::Kind::Native,
            Kind::Remote => api::Kind::Remote,
            Kind::Media => api::Kind::Media,
            Kind::Spore => api::Kind::Spore,
            Kind::Retained => api::Kind::Retained,
            Kind::Browser => api::Kind::Browser,
        },
        width: info.width,
        height: info.height,
        scale: info.scale,
        color_space: info.color_space,
        alpha: info.alpha,
    }
}

fn api_frame(frame: Frame) -> api::Frame {
    api::Frame {
        id: frame.id,
        pts_ms: frame.pts_ms,
        damage: frame.damage.into_iter().map(api_rect).collect(),
        keyframe: frame.keyframe,
    }
}

fn api_rect(rect: Rect) -> api::Rect {
    api::Rect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

fn api_event(event: Event) -> api::Event {
    match event {
        Event::Resized(info) => api::Event::Resized(api_info(info)),
        Event::Frame(frame) => api::Event::Frame(api_frame(frame)),
        Event::Damaged(damage) => api::Event::Damaged(damage.into_iter().map(api_rect).collect()),
        Event::Idle => api::Event::Idle,
        Event::Closed => api::Event::Closed,
    }
}

fn input(event: api::Input) -> Input {
    match event {
        api::Input::PointerMove(point) => Input::PointerMove(internal_point(point)),
        api::Input::PointerButton(event) => Input::PointerButton(
            match event.button {
                api::Button::Primary => Button::Primary,
                api::Button::Secondary => Button::Secondary,
                api::Button::Middle => Button::Middle,
            },
            event.pressed,
            internal_point(event.point),
        ),
        api::Input::Scroll(event) => {
            Input::Scroll(event.delta_x, event.delta_y, internal_point(event.point))
        }
        api::Input::Key(event) => Input::Key(
            key_name(event.key),
            Modifiers {
                shift: event.modifiers.shift,
                control: event.modifiers.control,
                alt: event.modifiers.alt,
                meta: event.modifiers.meta,
            },
            event.pressed,
        ),
        api::Input::Text(text) => Input::Text(text),
        api::Input::Focus(focused) => Input::Focus(focused),
    }
}

fn key_name(key: Key) -> String {
    match key {
        Key::Up => "up".into(),
        Key::Down => "down".into(),
        Key::Left => "left".into(),
        Key::Right => "right".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::PageUp => "page-up".into(),
        Key::PageDown => "page-down".into(),
        Key::Insert => "insert".into(),
        Key::Select => "select".into(),
        Key::Back => "back".into(),
        Key::Menu => "menu".into(),
        Key::Info => "info".into(),
        Key::Number(value) => value.to_string(),
        Key::Star => "star".into(),
        Key::Hash => "hash".into(),
        Key::Call => "call".into(),
        Key::EndCall => "end-call".into(),
        Key::PushToTalk => "push-to-talk".into(),
        Key::SoftLeft => "soft-left".into(),
        Key::SoftRight => "soft-right".into(),
        Key::PlayPause => "play-pause".into(),
        Key::Rewind => "rewind".into(),
        Key::FastForward => "fast-forward".into(),
        Key::Next => "next".into(),
        Key::Previous => "previous".into(),
        Key::VolumeUp => "volume-up".into(),
        Key::VolumeDown => "volume-down".into(),
        Key::Mute => "mute".into(),
        Key::FKey(value) => format!("f{value}"),
        Key::Character(value) => value.to_string(),
        Key::Backspace => "backspace".into(),
        Key::Delete => "delete".into(),
        Key::Tab => "tab".into(),
        Key::Escape => "escape".into(),
        Key::Enter => "enter".into(),
        Key::Named(value) => value,
    }
}

fn internal_point(point: api::Point) -> Point {
    Point {
        x: point.x,
        y: point.y,
    }
}

fn error(error: SurfaceError) -> api::Error {
    match error {
        SurfaceError::Denied => api::Error::Denied,
        SurfaceError::UnknownSource => api::Error::UnknownSource,
        SurfaceError::NotReady => api::Error::NotReady,
        SurfaceError::Unsupported => api::Error::Unsupported,
        SurfaceError::OverBudget => api::Error::OverBudget,
        SurfaceError::Invalid => api::Error::Invalid,
        SurfaceError::Closed => api::Error::Closed,
        SurfaceError::Backend => api::Error::Backend,
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: api::Host + 'static,
{
    api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureSource;

    impl SurfaceSource for FixtureSource {
        fn open(&self, source_id: &str) -> Result<Box<dyn SurfaceSession>, SurfaceError> {
            if source_id != "fixture" {
                return Err(SurfaceError::UnknownSource);
            }
            Ok(Box::new(FixtureSession {
                step: 0,
                input: None,
            }))
        }
    }

    struct FixtureSession {
        step: u8,
        input: Option<Input>,
    }

    impl SurfaceSession for FixtureSession {
        fn info(&mut self) -> Result<Info, SurfaceError> {
            Ok(Info {
                kind: Kind::Native,
                width: 640,
                height: 480,
                scale: 1.0,
                color_space: "srgb".into(),
                alpha: "opaque".into(),
            })
        }

        fn next(&mut self, _timeout: Duration) -> Result<Option<Event>, SurfaceError> {
            self.step += 1;
            Ok(match self.step {
                1 => Some(Event::Resized(Info {
                    kind: Kind::Native,
                    width: 640,
                    height: 480,
                    scale: 1.0,
                    color_space: "srgb".into(),
                    alpha: "opaque".into(),
                })),
                2 => Some(Event::Frame(Frame {
                    id: 7,
                    pts_ms: 42,
                    damage: vec![Rect {
                        x: 0,
                        y: 0,
                        width: 10,
                        height: 10,
                    }],
                    keyframe: true,
                })),
                _ => None,
            })
        }

        fn input(&mut self, event: Input) -> Result<(), SurfaceError> {
            self.input = Some(event);
            Ok(())
        }

        fn close(&mut self) -> Result<(), SurfaceError> {
            Ok(())
        }
    }

    #[test]
    fn opens_surface_and_reports_info_and_events() {
        let mut host = SurfaceHost::new(FixtureSource);
        let surface = api::Host::open(&mut host, "fixture".into()).unwrap();
        let info = api::HostSurface::info(&mut host, surface).unwrap();
        assert_eq!(info.width, 640);
        assert_eq!(info.height, 480);
        assert_eq!(info.kind, api::Kind::Native);
        let surface = api::Host::open(&mut host, "fixture".into()).unwrap();
        let event = api::HostSurface::next(&mut host, surface, 100).unwrap();
        assert!(matches!(event, Some(api::Event::Resized(_))));
    }

    #[test]
    fn forwards_normalized_input() {
        let mut host = SurfaceHost::new(FixtureSource);
        let surface = api::Host::open(&mut host, "fixture".into()).unwrap();
        api::HostSurface::input(
            &mut host,
            surface,
            api::Input::PointerButton(api::PointerButton {
                button: api::Button::Primary,
                pressed: true,
                point: api::Point { x: 0.25, y: 0.75 },
            }),
        )
        .unwrap();
    }
}
