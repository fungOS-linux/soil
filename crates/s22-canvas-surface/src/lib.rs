//! Headless host adapter for the existing Canvas Work Inbox owner.
//! Uses explicit demo snapshot; never claims live project or device data.
pub use fpl_gfx::status_widgets::{Availability, BatteryView, ModelEstimateView, VolumeView};
mod aurora;
mod demo_views;
mod simple_jarvis;
use canvas_workspace_ui::{Inbox, Options, Snapshot};
use coherence_native::{Density, Mode, Tokens};
pub use demo_views::{DemoView, Navigation};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};

#[derive(Debug, Clone, Copy)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Back,
    Tab,
    Home,
    Recents,
    Ptt,
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub generation: u64,
}
impl Frame {
    pub fn bgr24(&self) -> Vec<u8> {
        self.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect()
    }
}

/// Host-owned imported surface layer.
///
/// A native or remote adapter uploads its current frame here. The layer is
/// host-side only; it is not a Spore capability and no guest bytes pass through
/// this path. Native GPU adapters should replace this software upload with a
/// shared texture.
#[derive(Clone)]
pub struct SurfaceLayer {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
}

pub struct CanvasSurface {
    model_estimate: ModelEstimateView,
    model_estimate_enabled: bool,
    battery: BatteryView,
    frame_time_ms: u64,
    animation_enabled: bool,
    volume: VolumeView,
    volume_overlay: bool,
    navigation: Navigation,
    simple_jarvis: bool,
    aurora: bool,
    context: egui::Context,
    inbox: Inbox,
    snapshot: Snapshot,
    tokens: Tokens,
    raster: EguiSoftwareRender,
    width: u32,
    height: u32,
    generation: u64,
    events: Vec<egui::Event>,
    surface_layer: Option<SurfaceLayer>,
    surface_texture: Option<egui::TextureHandle>,
    surface_layer_dirty: bool,
}
impl CanvasSurface {
    /// Explicit opt-in demonstration; default owner Canvas remains unchanged.
    /// Replace read-only status atomically; the host owns freshness/expiry.
    /// Update an independently authorized model channel, never measured SOC.
    pub fn update_model_estimate(&mut self, view: ModelEstimateView) -> Result<(), String> {
        view.validate()?;
        self.model_estimate = view;
        self.model_estimate_enabled = true;
        Ok(())
    }
    /// Explicit host/guest-authorized animation time; no wall clock or scheduler.
    /// Millisecond resolution; phase wraps after one hour. Do not quantize here:
    /// the presenter/producer cadence is the frame-rate policy, not the adapter.
    pub fn update_frame_time(&mut self, elapsed_ms: u64) {
        self.frame_time_ms = elapsed_ms % 3_600_000;
        self.animation_enabled = true;
    }
    /// Measured state only; never accepts or infers a requested set target.
    pub fn update_volume(&mut self, view: VolumeView) -> Result<(), String> {
        view.validate()?;
        self.volume = view;
        Ok(())
    }
    /// Host chooses visibility on each presentation; no renderer clock/guest authority.
    pub fn update_volume_overlay(&mut self, visible: bool) {
        self.volume_overlay = visible;
    }
    pub fn update_status(&mut self, battery: BatteryView) -> Result<(), String> {
        battery.validate()?;
        self.battery = battery;
        Ok(())
    }
    pub fn simple_jarvis(width: u32, height: u32) -> Result<Self, String> {
        let mut surface = Self::new(width, height)?;
        surface.simple_jarvis = true;
        Ok(surface)
    }
    /// Opt-in decorative aurora curtain. The default surface is unchanged.
    pub fn set_aurora(&mut self, enabled: bool) {
        self.aurora = enabled;
    }
    /// Host-only imported surface. Replaces the previous layer, or clears it
    /// with `None`. Validation is bounded; the adapter owns the source pixels.
    pub fn set_surface_layer(&mut self, layer: Option<SurfaceLayer>) -> Result<(), String> {
        if let Some(layer) = &layer
            && (layer.width == 0
                || layer.height == 0
                || layer.width > 4096
                || layer.height > 4096
                || layer.rgba.len() != (layer.width as usize * layer.height as usize * 4))
        {
            return Err("surface layer is empty, oversized, or not RGBA8".into());
        }
        self.surface_layer = layer;
        self.surface_texture = None;
        self.surface_layer_dirty = true;
        Ok(())
    }

    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        let snapshot = canvas_workspace_ui::demo::work();
        Self::with_snapshot(width, height, snapshot)
    }
    /// Host-owned validated schema, never guest-authored primitive records.
    pub fn with_snapshot(width: u32, height: u32, snapshot: Snapshot) -> Result<Self, String> {
        if !(120..=1024).contains(&width) || !(128..=1024).contains(&height) {
            return Err("Canvas extent outside bounded 120..1024 x 128..1024".into());
        }
        snapshot.validate()?;
        let context = egui::Context::default();
        let tokens = Tokens::new(Mode::Dark, Density::Compact);
        context.set_style(
            coherence_native::egui::style(&tokens)
                .map_err(|e| format!("Coherence metrics: {e:?}"))?,
        );
        Ok(Self {
            model_estimate: ModelEstimateView::default(),
            model_estimate_enabled: false,
            battery: BatteryView::default(),
            frame_time_ms: 0,
            animation_enabled: false,
            volume: VolumeView::default(),
            volume_overlay: false,
            navigation: Navigation::default(),
            simple_jarvis: false,
            aurora: false,
            context,
            inbox: Inbox::default(),
            snapshot,
            tokens,
            raster: EguiSoftwareRender::new(ColorFieldOrder::Rgba).with_caching(false),
            width,
            height,
            generation: 0,
            events: Vec::new(),
            surface_layer: None,
            surface_texture: None,
            surface_layer_dirty: false,
        })
    }
    pub fn selected(&self) -> Option<&str> {
        self.inbox.selected.as_deref()
    }
    pub fn details_open(&self) -> bool {
        self.inbox.details_open
    }
    /// Status-view navigation; only used by the explicit simple_jarvis surface.
    pub fn navigation(&self) -> Navigation {
        self.navigation
    }
    pub fn key(&mut self, key: Key) -> Result<(), String> {
        if self.simple_jarvis {
            self.navigation.key(key);
            return Ok(());
        }
        if self.events.len() >= 64 {
            return Err("pending Canvas input bound exceeded".into());
        }
        let key = match key {
            Key::Up => egui::Key::ArrowUp,
            Key::Down => egui::Key::ArrowDown,
            Key::Left => egui::Key::ArrowLeft,
            Key::Right => egui::Key::ArrowRight,
            Key::Enter => egui::Key::Enter,
            Key::Back => egui::Key::Escape,
            Key::Tab => egui::Key::Tab,
            Key::Home => {
                self.inbox.details_open = false;
                return Ok(());
            }
            Key::Recents | Key::Ptt => return Ok(()),
        };
        for pressed in [true, false] {
            self.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        Ok(())
    }
    fn pixel_scale(&self) -> f32 {
        if self.width <= 160 {
            1.0
        } else {
            self.width as f32 / 480.0
        }
    }
    pub fn present(&mut self) -> Result<Frame, String> {
        let t0 = std::time::Instant::now();
        // Compact outer surfaces are native logical pixels, never a reduced
        // desktop framebuffer. Inner retains its existing 480px owner layout.
        let native_compact = self.width <= 160;
        let scale = self.pixel_scale();
        let logical_width = self.width as f32 / scale;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(logical_width, self.height as f32 / scale),
            )),
            focused: true,
            events: std::mem::take(&mut self.events),
            time: Some(self.frame_time_ms as f64 / 1000.0),
            ..Default::default()
        };
        let surface = if let Some(layer) = &self.surface_layer {
            if self.surface_texture.is_none() || self.surface_layer_dirty {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [layer.width as usize, layer.height as usize],
                    &layer.rgba,
                );
                match &mut self.surface_texture {
                    Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        self.surface_texture = Some(self.context.load_texture(
                            "soil-surface-layer",
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
                self.surface_layer_dirty = false;
            }
            self.surface_texture
                .as_ref()
                .map(|texture| (texture.clone(), layer.x, layer.y, layer.width, layer.height))
        } else {
            None
        };
        let output = self.context.run(input, |ctx| {
            if self.simple_jarvis {
                // The simple-jarvis surface owns its own background/FX and must
                // run edge-to-edge; the default panel margin leaves a dark
                // border around the aurora curtain.
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        demo_views::show(
                            ui,
                            self.navigation,
                            self.battery,
                            self.model_estimate_enabled.then_some(self.model_estimate),
                            self.animation_enabled
                                .then_some(self.frame_time_ms as f32 / 1000.0),
                            self.aurora,
                        );
                        if self.volume_overlay {
                            demo_views::volume_overlay(ui, self.volume);
                        }
                    });
                return;
            }
            egui::TopBottomPanel::top("canvas-host-provenance").show(ctx, |ui| {
                if native_compact {
                    ui.label(egui::RichText::new("Canvas DEMO").size(12.0));
                } else {
                    ui.label("Canvas / Work inbox — DEMO DATA");
                }
            });
            egui::CentralPanel::default().show(ctx, |ui| {
                self.inbox.show(
                    ui,
                    &self.tokens,
                    &self.snapshot,
                    Options {
                        keyboard_active: true,
                        observation_expired: false,
                    },
                );
            });
            if let Some((texture, x, y, width, height)) = &surface {
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("soil-surface-layer"),
                ));
                painter.image(
                    texture.id(),
                    egui::Rect::from_min_size(
                        egui::pos2(*x as f32, *y as f32),
                        egui::vec2(*width as f32, *height as f32),
                    ),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        });
        let t1 = std::time::Instant::now();
        let jobs = self.context.tessellate(output.shapes, scale);
        let t2 = std::time::Instant::now();
        if jobs.len() > 32768 {
            return Err("Canvas mesh job bound exceeded".into());
        }
        if jobs
            .iter()
            .any(|j| matches!(j.primitive, egui::epaint::Primitive::Callback(_)))
        {
            return Err("Canvas software surface cannot execute paint callbacks".into());
        }
        let mut pixels = vec![[0, 0, 0, 255]; (self.width * self.height) as usize];
        self.raster.render(
            &mut BufferMutRef::new(&mut pixels, self.width as usize, self.height as usize),
            &jobs,
            &output.textures_delta,
            scale,
        );
        let t3 = std::time::Instant::now();
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("generation exhausted")?;
        let rgba = pixels.into_iter().flatten().collect();
        let t4 = std::time::Instant::now();
        if std::env::var_os("SOIL_CANVAS_TIMING").is_some() {
            eprintln!(
                "present egui={:?} tess={:?} raster={:?} flatten={:?} total={:?} jobs={}",
                t1 - t0,
                t2 - t1,
                t3 - t2,
                t4 - t3,
                t4 - t0,
                jobs.len()
            );
        }
        Ok(Frame {
            width: self.width,
            height: self.height,
            rgba,
            generation: self.generation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_owner_pixels_and_navigation() {
        assert_owner_surface(480, 640);
    }
    #[test]
    fn outer_actual_owner_pixels_and_navigation() {
        assert_owner_surface(128, 128);
    }
    #[test]
    fn native_compact_geometry_and_fonts() {
        let surface = CanvasSurface::new(128, 128).unwrap();
        assert_eq!(surface.pixel_scale(), 1.0);
        assert_eq!(128.0 / surface.pixel_scale(), 128.0);
        const { assert!(canvas_workspace_ui::COMPACT_FONT_POINTS >= 12.0) };
        assert_eq!(CanvasSurface::new(480, 640).unwrap().pixel_scale(), 1.0);
        let ctx = egui::Context::default();
        ctx.set_style(coherence_native::egui::style(&surface.tokens).unwrap());
        let mut inbox = Inbox::default();
        for details in [false, true] {
            inbox.details_open = details;
            let mut rects = Vec::new();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(128.0, 128.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    assert_eq!(ctx.pixels_per_point(), 1.0);
                    egui::TopBottomPanel::top("provenance").show(ctx, |ui| {
                        ui.label(egui::RichText::new("Canvas DEMO").size(12.0));
                    });
                    egui::CentralPanel::default().show(ctx, |ui| {
                        rects = inbox
                            .show(ui, &surface.tokens, &surface.snapshot, Options::default())
                            .row_rects;
                    });
                },
            );
            assert!(rects.len() >= 4);
            for rect in rects {
                assert!(
                    rect.min.x >= 0.0
                        && rect.min.y >= 0.0
                        && rect.max.x <= 128.0
                        && rect.max.y <= 128.0,
                    "{rect:?}"
                );
            }
        }
    }
    #[test]
    fn simple_jarvis_opt_in_both_extents() {
        for (width, height) in [(128, 128), (480, 640)] {
            let mut surface = CanvasSurface::simple_jarvis(width, height).unwrap();
            let image = surface.present().unwrap();
            assert_eq!((image.width, image.height), (width, height));
            assert!(image.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            assert!(
                image
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|p| p != &image.rgba[..4])
            );
            assert!(surface.selected().is_none());
            assert!(fpl_gfx::Theme::jarvis_hud().validate().is_ok());
        }
    }
    #[test]
    fn battery_updates_are_atomic_and_nonfresh_numbers_hidden() {
        for (width, height) in [(128, 128), (480, 640)] {
            let mut surface = CanvasSurface::simple_jarvis(width, height).unwrap();
            surface.key(Key::Right).unwrap();
            assert_eq!(surface.battery, BatteryView::default());
            let fresh = BatteryView {
                percent: None,
                voltage_uv: Some(3_879_000),
                availability: Availability::Fresh,
            };
            surface.update_status(fresh).unwrap();
            assert!(
                surface
                    .update_status(BatteryView {
                        percent: Some(101),
                        ..fresh
                    })
                    .is_err()
            );
            assert_eq!(surface.battery, fresh);
            for availability in [
                Availability::Unknown,
                Availability::Stale,
                Availability::Denied,
            ] {
                surface
                    .update_status(BatteryView {
                        percent: Some(79),
                        availability,
                        ..fresh
                    })
                    .unwrap();
                surface.present().unwrap();
                let cached = surface.present().unwrap();
                surface
                    .update_status(BatteryView {
                        percent: None,
                        voltage_uv: None,
                        availability,
                    })
                    .unwrap();
                let empty = surface.present().unwrap();
                assert_eq!(
                    cached.rgba, empty.rgba,
                    "nonfresh cached numbers must not affect pixels"
                );
            }
        }
    }
    #[test]
    fn model_estimate_labels_atomic_updates_and_native_pixels() {
        for (width, height) in [(128, 128), (480, 640)] {
            let mut surface = CanvasSurface::simple_jarvis(width, height).unwrap();
            surface.key(Key::Right).unwrap();
            assert_eq!(surface.model_estimate, ModelEstimateView::default());
            let battery = surface.battery;
            let fresh = ModelEstimateView {
                percent: Some(57),
                availability: Availability::Fresh,
            };
            surface.update_model_estimate(fresh).unwrap();
            assert!(
                surface
                    .update_model_estimate(ModelEstimateView {
                        percent: Some(101),
                        ..fresh
                    })
                    .is_err()
            );
            assert_eq!(surface.model_estimate, fresh);
            assert_eq!(surface.battery, battery);
            let mut marks = Vec::new();
            fpl_gfx::status_widgets::model_estimate::visit(fresh, |m| marks.push(m)).unwrap();
            assert!(marks.iter().any(|m| matches!(m,fpl_gfx::status_widgets::Mark::Text{text,..} if text=="Estimated 57%")));
            surface.present().unwrap();
            let numeric = surface.present().unwrap();
            surface
                .update_model_estimate(ModelEstimateView {
                    percent: None,
                    ..fresh
                })
                .unwrap();
            surface.present().unwrap();
            assert_ne!(numeric.rgba, surface.present().unwrap().rgba);
            for availability in [
                Availability::Unknown,
                Availability::Stale,
                Availability::Denied,
            ] {
                surface
                    .update_model_estimate(ModelEstimateView {
                        availability,
                        ..fresh
                    })
                    .unwrap();
                surface.present().unwrap();
                let cached = surface.present().unwrap();
                surface
                    .update_model_estimate(ModelEstimateView {
                        percent: None,
                        availability,
                    })
                    .unwrap();
                surface.present().unwrap();
                assert_eq!(cached.rgba, surface.present().unwrap().rgba);
                assert_eq!(surface.battery, battery);
            }
        }
    }
    #[test]
    fn demo_navigation_boundaries_repeat_back_and_pixels() {
        for (width, height) in [(128, 128), (480, 640)] {
            let mut surface = CanvasSurface::simple_jarvis(width, height).unwrap();
            surface.present().unwrap();
            let home = surface.present().unwrap();
            for _ in 0..100 {
                surface.key(Key::Left).unwrap();
                surface.key(Key::Up).unwrap();
            }
            assert_eq!(surface.navigation(), Navigation::default());
            surface.key(Key::Down).unwrap();
            surface.present().unwrap();
            assert_ne!(home.rgba, surface.present().unwrap().rgba);
            for _ in 0..100 {
                surface.key(Key::Down).unwrap();
            }
            assert_eq!(surface.navigation().focus, 1);
            surface.key(Key::Enter).unwrap();
            assert_eq!(surface.navigation().view, DemoView::DeviceStatus);
            surface.present().unwrap();
            let device = surface.present().unwrap();
            for _ in 0..100 {
                surface.key(Key::Right).unwrap();
            }
            assert_eq!(surface.navigation().view, DemoView::DeviceStatus);
            surface.key(Key::Enter).unwrap();
            assert_eq!(surface.navigation().view, DemoView::DeviceStatus);
            surface.key(Key::Left).unwrap();
            assert_eq!(surface.navigation().view, DemoView::Battery);
            surface.present().unwrap();
            let battery = surface.present().unwrap();
            assert_ne!(device.rgba, battery.rgba);
            assert_ne!(home.rgba, battery.rgba);
            surface.key(Key::Back).unwrap();
            surface.key(Key::Back).unwrap();
            assert_eq!(surface.navigation(), Navigation::default());
            surface.key(Key::Enter).unwrap();
            assert_eq!(surface.navigation().view, DemoView::Battery);
            surface.key(Key::Back).unwrap();
            surface.present().unwrap();
            assert_eq!(home.rgba, surface.present().unwrap().rgba);
            assert!(!surface.model_estimate_enabled);
        }
    }
    #[test]
    fn supplied_reactor_time_is_repeatable_bounded_and_home_only() {
        for (width, height) in [(128, 128), (480, 640)] {
            let mut surface = CanvasSurface::simple_jarvis(width, height).unwrap();
            assert!(!surface.animation_enabled);
            surface.update_frame_time(0);
            surface.present().unwrap();
            let zero = surface.present().unwrap();
            assert_eq!(zero.rgba, surface.present().unwrap().rgba);
            surface.update_frame_time(0);
            assert_eq!(zero.rgba, surface.present().unwrap().rgba);
            surface.update_frame_time(800);
            let later = surface.present().unwrap();
            assert_ne!(zero.rgba, later.rgba);
            assert_eq!(later.rgba, surface.present().unwrap().rgba);
            surface.update_frame_time(3_600_800);
            assert_eq!(later.rgba, surface.present().unwrap().rgba);
            surface.update_frame_time(u64::MAX);
            assert!(surface.frame_time_ms < 3_600_000);
            surface.key(Key::Down).unwrap();
            assert_eq!(surface.navigation().focus, 1);
            surface.key(Key::Enter).unwrap();
            surface.present().unwrap();
            let device = surface.present().unwrap();
            surface.update_frame_time(1600);
            assert_eq!(device.rgba, surface.present().unwrap().rgba);
            surface.key(Key::Back).unwrap();
            assert_eq!(surface.navigation(), Navigation::default());
            let started = std::time::Instant::now();
            for tick in 0..13 {
                surface.update_frame_time(tick * 80);
                surface.present().unwrap();
            }
            eprintln!(
                "reactor {width}x{height}: 13 debug frames {:?}; physical CPU budget unqualified",
                started.elapsed()
            );
        }
    }
    #[test]
    fn aurora_is_opt_in_and_animated() {
        let mut plain = CanvasSurface::simple_jarvis(128, 128).unwrap();
        plain.update_frame_time(0);
        let plain0 = plain.present().unwrap();

        let mut aurora = CanvasSurface::simple_jarvis(128, 128).unwrap();
        aurora.set_aurora(true);
        aurora.update_frame_time(0);
        let a0 = aurora.present().unwrap();
        assert_ne!(plain0.rgba, a0.rgba);
        aurora.update_frame_time(500);
        let a1 = aurora.present().unwrap();
        assert_ne!(a0.rgba, a1.rgba);
        // The default surface remains untouched unless the FX flag is set.
        let plain_before = plain.present().unwrap();
        plain.set_aurora(false);
        assert_eq!(plain_before.rgba, plain.present().unwrap().rgba);
    }
    #[test]
    fn host_surface_layer_is_composited_and_bounded() {
        let width = 128u32;
        let height = 128u32;
        let mut surface = CanvasSurface::new(width, height).unwrap();
        // A solid red layer, built in one allocation: `chunks_exact_mut` here was
        // a loop whose only job was to write the same four bytes.
        let rgba = [255u8, 0, 0, 255].repeat((width * height) as usize);
        surface
            .set_surface_layer(Some(SurfaceLayer {
                rgba,
                width,
                height,
                x: 0,
                y: 0,
            }))
            .unwrap();
        let frame = surface.present().unwrap();
        assert_eq!(&frame.rgba[..4], &[255, 0, 0, 255]);
        assert!(
            surface
                .set_surface_layer(Some(SurfaceLayer {
                    rgba: vec![0; 3],
                    width: 2,
                    height: 2,
                    x: 0,
                    y: 0,
                }))
                .is_err()
        );
        surface.set_surface_layer(None).unwrap();
    }

    #[test]
    fn dimensions_remain_bounded() {
        assert!(CanvasSurface::new(128, 127).is_err());
        assert!(CanvasSurface::new(119, 128).is_err());
        assert!(CanvasSurface::new(1025, 128).is_err());
        assert!(CanvasSurface::new(128, 1025).is_err());
    }
    fn assert_owner_surface(width: u32, height: u32) {
        fn assert_send<T: Send>() {}
        assert_send::<CanvasSurface>();
        let mut surface = CanvasSurface::new(width, height).unwrap();
        let first = surface.present().unwrap();
        assert_eq!((first.width, first.height), (width, height));
        assert_eq!(first.rgba.len(), (width * height * 4) as usize);
        assert!(
            first
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p != &first.rgba[..4])
        );
        assert!(first.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        let selected = surface.selected().map(str::to_owned);
        surface.key(Key::Down).unwrap();
        let second = surface.present().unwrap();
        assert_ne!(selected.as_deref(), surface.selected());
        assert_ne!(first.rgba, second.rgba);
        surface.key(Key::Enter).unwrap();
        surface.present().unwrap();
        assert!(surface.details_open());
        surface.key(Key::Back).unwrap();
        surface.present().unwrap();
        assert!(!surface.details_open());
        assert_eq!(second.bgr24().len(), (width * height * 3) as usize);
    }
}
