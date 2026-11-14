//! Narrow capability routing only. Implementations must call the actual Canvas
//! owner model/render library; this module contains no UI schema or rasterizer.
use crate::{HEIGHT, Key, WIDTH};
use anyhow::{Context, Result, ensure};
use fpl_gfx::SoftwareImage;

pub const ANIMATED_BATTERY_GUEST: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/animated-battery-canvas.wasm"));
pub const IMPORT: &str = "soil:canvas/surface@0.1.0";
pub const CANVAS_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/canvas-client.wasm"));
/// A Spore that owns its pixels and hands them over as RGBA bytes.
pub const PIXEL_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pixel-client.wasm"));

/// Host-selected physical extent, never selected by the guest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SurfaceSize {
    #[default]
    Inner,
    Outer,
}
impl SurfaceSize {
    pub fn dimensions(self) -> (u32, u32) {
        match self {
            Self::Inner => (WIDTH, HEIGHT),
            Self::Outer => (128, 128),
        }
    }
}
pub trait CanvasSurface: Send {
    fn size(&self) -> SurfaceSize {
        SurfaceSize::Inner
    }
    fn update_frame_time(&mut self, _elapsed_ms: u64) {}
    fn present(&mut self) -> Result<SoftwareImage>;
    fn key(&mut self, key: Key) -> Result<()>;
    fn update_model_estimate(
        &mut self,
        _estimate: s22_canvas_surface::ModelEstimateView,
    ) -> Result<()> {
        anyhow::bail!("surface does not support model estimate")
    }
    fn update_status(&mut self, _battery: s22_canvas_surface::BatteryView) -> Result<()> {
        anyhow::bail!("surface does not support status")
    }
}

/// Actual Canvas Work Inbox adapter. Owner schema/theme/navigation and existing
/// owner widgets are reused, with an explicitly permitted egui legacy exception.
/// The external adapter uses a third-party CPU mesh/font backend, NOT the
/// primitive-only GFX fixture renderer and NOT claimed as GPU acceleration.
pub struct OwnerSurface {
    owner: s22_canvas_surface::CanvasSurface,
    size: SurfaceSize,
}
impl OwnerSurface {
    pub fn new() -> Result<Self> {
        Self::with_size(SurfaceSize::Inner)
    }
    pub fn simple_jarvis(size: SurfaceSize) -> Result<Self> {
        let (width, height) = size.dimensions();
        s22_canvas_surface::CanvasSurface::simple_jarvis(width, height)
            .map(|owner| Self { owner, size })
            .map_err(anyhow::Error::msg)
    }
    /// Host-only typed status update; no new guest imports or authority.
    pub fn update_status(&mut self, battery: s22_canvas_surface::BatteryView) -> Result<()> {
        self.owner
            .update_status(battery)
            .map_err(anyhow::Error::msg)
    }
    pub fn update_model_estimate(
        &mut self,
        estimate: s22_canvas_surface::ModelEstimateView,
    ) -> Result<()> {
        self.owner
            .update_model_estimate(estimate)
            .map_err(anyhow::Error::msg)
    }
    pub fn update_frame_time(&mut self, elapsed_ms: u64) {
        self.owner.update_frame_time(elapsed_ms);
    }
    /// Opt-in decorative aurora curtain; no guest authority or device state.
    pub fn set_aurora(&mut self, enabled: bool) {
        self.owner.set_aurora(enabled);
    }
    pub fn with_size(size: SurfaceSize) -> Result<Self> {
        let (width, height) = size.dimensions();
        s22_canvas_surface::CanvasSurface::new(width, height)
            .map(|owner| Self { owner, size })
            .map_err(anyhow::Error::msg)
    }
}
impl CanvasSurface for OwnerSurface {
    fn update_frame_time(&mut self, elapsed_ms: u64) {
        OwnerSurface::update_frame_time(self, elapsed_ms);
    }
    fn update_model_estimate(
        &mut self,
        estimate: s22_canvas_surface::ModelEstimateView,
    ) -> Result<()> {
        OwnerSurface::update_model_estimate(self, estimate)
    }
    fn update_status(&mut self, battery: s22_canvas_surface::BatteryView) -> Result<()> {
        OwnerSurface::update_status(self, battery)
    }
    fn size(&self) -> SurfaceSize {
        self.size
    }
    fn present(&mut self) -> Result<SoftwareImage> {
        let frame = self.owner.present().map_err(anyhow::Error::msg)?;
        Ok(SoftwareImage {
            width: frame.width,
            height: frame.height,
            pixels: frame.rgba,
        })
    }
    fn key(&mut self, key: Key) -> Result<()> {
        use s22_canvas_surface::Key as K;
        self.owner
            .key(match key {
                Key::Up => K::Up,
                Key::Down => K::Down,
                Key::Left => K::Left,
                Key::Right => K::Right,
                Key::Select => K::Enter,
                Key::Back => K::Back,
                Key::Home => K::Home,
            })
            .map_err(anyhow::Error::msg)
    }
}

#[derive(Default)]
pub(crate) struct CanvasHost {
    pub surface: Option<Box<dyn CanvasSurface>>,
    /// Host-selected extent when no owner surface is granted. A guest that
    /// renders its own pixels still never chooses the display.
    pub size: SurfaceSize,
    pub frame: Option<SoftwareImage>,
    pub presents: usize,
    pub actions: usize,
    pub expected_time: Option<u64>,
}
impl CanvasHost {
    pub fn reset(&mut self) {
        self.frame = None;
        self.presents = 0;
        self.actions = 0;
        self.expected_time = None;
    }
}
pub(crate) fn link(linker: &mut wasmtime::Linker<crate::Limits>) -> Result<()> {
    linker.func_wrap(
        IMPORT,
        "present",
        |mut caller: wasmtime::Caller<'_, crate::Limits>| -> wasmtime::Result<i32> {
            crate::host((|| -> Result<i32> {
                if caller.data().3.enabled {
                    let view = caller.data().3.presentation();
                    if let Some(surface) = &mut caller.data_mut().2.surface {
                        surface.update_status(view)?;
                    }
                }
                if caller.data().4.enabled {
                    let view = caller.data().4.presentation();
                    if let Some(surface) = &mut caller.data_mut().2.surface {
                        surface.update_model_estimate(view)?;
                    }
                }
                let host = &mut caller.data_mut().2;
                host.presents += 1;
                ensure!(host.presents <= 1, "canvas present budget");
                let Some(surface) = &mut host.surface else {
                    return Ok(1);
                };
                let (width, height) = surface.size().dimensions();
                let frame = surface.present()?;
                ensure!(
                    frame.width == width
                        && frame.height == height
                        && frame.pixels.len() == width as usize * height as usize * 4,
                    "invalid Canvas owner frame"
                );
                ensure!(
                    frame.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255),
                    "nonopaque Canvas frame"
                );
                host.frame = Some(frame);
                Ok(0)
            })())
        },
    )?;
    // A Spore that renders its own pixels hands them over here: bytes, not a
    // document. Extent is the host's, so the guest fills `size.dimensions()` and
    // the host validates length and opacity before retaining the frame.
    linker.func_wrap(
        IMPORT,
        "present_rgba",
        |mut caller: wasmtime::Caller<'_, crate::Limits>,
         ptr: i32,
         len: i32|
         -> wasmtime::Result<i32> {
            crate::host((|| -> Result<i32> {
                let (width, height) = caller
                    .data()
                    .2
                    .surface
                    .as_ref()
                    .map(|surface| surface.size())
                    .unwrap_or(caller.data().2.size)
                    .dimensions();
                let expected = width as usize * height as usize * 4;
                ensure!(
                    len >= 0 && len as usize == expected,
                    "Canvas frame is {len} bytes, {width}x{height} RGBA8 is {expected}"
                );
                let memory = caller
                    .get_export("memory")
                    .and_then(|export| export.into_memory())
                    .context("Canvas guest has no memory export")?;
                let mut pixels = vec![0u8; expected];
                memory.read(&caller, ptr as usize, &mut pixels)?;
                ensure!(
                    pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255),
                    "nonopaque Canvas frame"
                );
                let host = &mut caller.data_mut().2;
                host.presents += 1;
                ensure!(host.presents <= 1, "canvas present budget");
                host.frame = Some(SoftwareImage {
                    width,
                    height,
                    pixels,
                });
                Ok(0)
            })())
        },
    )?;
    linker.func_wrap(
        IMPORT,
        "navigate",
        |mut caller: wasmtime::Caller<'_, crate::Limits>, key: i32| -> wasmtime::Result<i32> {
            crate::host((|| -> Result<i32> {
                let host = &mut caller.data_mut().2;
                host.actions += 1;
                ensure!(host.actions <= 1, "canvas action budget");
                let key = match key {
                    1 => Key::Up,
                    2 => Key::Down,
                    3 => Key::Left,
                    4 => Key::Right,
                    5 => Key::Select,
                    6 => Key::Back,
                    7 => Key::Home,
                    _ => return Ok(2),
                };
                let Some(surface) = &mut host.surface else {
                    return Ok(1);
                };
                surface.key(key)?;
                Ok(0)
            })())
        },
    )?;
    Ok(())
}

/// Separate clock import, absent from all legacy constructors.
pub(crate) fn link_animation(linker: &mut wasmtime::Linker<crate::Limits>) -> Result<()> {
    linker.func_wrap(
        "soil:canvas/animation@0.1.0",
        "tick",
        |mut caller: wasmtime::Caller<'_, crate::Limits>,
         elapsed_ms: i64|
         -> wasmtime::Result<i32> {
            crate::host((|| -> Result<i32> {
                let time = u64::try_from(elapsed_ms)?;
                let host = &mut caller.data_mut().2;
                let expected = host
                    .expected_time
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("unexpected or repeated animation tick"))?;
                ensure!(time == expected, "guest changed host frame time");
                if let Some(surface) = &mut host.surface {
                    surface.update_frame_time(time);
                }
                Ok(0)
            })())
        },
    )?;
    Ok(())
}
