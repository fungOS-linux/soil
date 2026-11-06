//! A wgpu surface whose scene target is the same device as the host.
//!
//! Native windows must not render to an offscreen texture, read it back, and
//! upload it again. `WindowRenderer` builds the scene pipelines for the surface
//! format and draws directly into the swapchain image.

use std::sync::Arc;

use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::{Renderer, Theme};

pub struct WindowRenderer {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    samples: u32,
    msaa: Option<wgpu::Texture>,
    msaa_size: (u32, u32),
}

impl WindowRenderer {
    pub fn new(window: Arc<Window>, size: PhysicalSize<u32>) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window)?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("soil-window"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: Default::default(),
            }))?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: Vec::new(),
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let samples = if adapter
            .get_texture_format_features(format)
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4)
        {
            4
        } else {
            1
        };
        let renderer = Renderer::from_device_sampled(device, queue, format, samples)?;
        Ok(Self {
            surface,
            config,
            renderer,
            samples,
            msaa: None,
            msaa_size: (0, 0),
        })
    }

    pub fn set_font(&mut self, font: Option<fontdue::Font>) {
        self.renderer.set_font(font);
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.msaa = None;
        self.msaa_size = (0, 0);
        self.surface.configure(self.renderer.device(), &self.config);
    }

    fn ensure_msaa(&mut self) -> Option<wgpu::TextureView> {
        if self.samples <= 1 {
            return None;
        }
        let size = (self.config.width, self.config.height);
        if self.msaa.is_none() || self.msaa_size != size {
            let texture = self
                .renderer
                .device()
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("soil-window msaa"),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: self.samples,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.config.format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
            self.msaa = Some(texture);
            self.msaa_size = size;
        }
        self.msaa
            .as_ref()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()))
    }

    /// Draw directly into the current swapchain image.
    pub fn present(&mut self, scene: &soil_scene::Scene, theme: Theme) -> anyhow::Result<()> {
        let output = self.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        if let Some(msaa) = self.ensure_msaa() {
            self.renderer.draw_to_resolve(
                scene,
                self.config.width,
                self.config.height,
                theme,
                &msaa,
                &view,
            )?;
        } else {
            self.renderer
                .draw_to(scene, self.config.width, self.config.height, theme, &view)?;
        }
        output.present();
        Ok(())
    }
}
