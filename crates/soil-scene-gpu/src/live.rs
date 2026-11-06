//! Resident live visualization for the native Soil host.
//!
//! Lua owns the UI: it writes a small `LiveSpec` describing layers, rects,
//! titles, palettes and styles. Rust owns the data streams and the frame loop:
//! it samples resident data, advances by real delta time, and builds a
//! `soil_scene` every frame. There is no per-frame Lua call and no FFI stream
//! bridge.

use std::collections::VecDeque;
use std::time::Instant;

use serde::Deserialize;
use soil_scene::{
    Area, Band, Candle, Candles, Content, Heatmap, Node, Points, Polyline, Rect, Scene, Text,
    Transform,
};

use crate::{Frame, Theme};

const TAU: f32 = std::f32::consts::PI * 2.0;
const CHART_POINTS: usize = 160;
const SAMPLE_SECONDS: f32 = 0.2;
const ATLAS_POINTS: usize = 240;

#[derive(Debug, Clone, Deserialize)]
pub struct LiveSpec {
    #[serde(default = "one")]
    pub schema: u32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub gap: f32,
    #[serde(default)]
    pub fps: Option<u32>,
    /// Open the native window fullscreen.
    #[serde(default)]
    pub fullscreen: Option<bool>,
    /// Lua-owned zoom for UI geometry and text. `1.0` fills the window at the
    /// app's natural density; larger values show fewer, bigger elements.
    #[serde(default = "unit_float")]
    pub ui_scale: f32,
    #[serde(default)]
    pub layers: Vec<LayerSpec>,
}

fn unit_float() -> f32 {
    1.0
}

fn one() -> u32 {
    1
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LayerSpec {
    Chart(ChartSpec),
    Graph(GraphSpec),
    Map(MapSpec),
    Atlas(AtlasSpec),
}

#[derive(Debug, Clone, Deserialize)]
pub struct RectSpec {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Default for RectSpec {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChartSpec {
    pub id: String,
    #[serde(default)]
    pub rect: RectSpec,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub series: Vec<SeriesSpec>,
    /// Draw the interactive legend. Lua can hide it per chart.
    #[serde(default = "yes")]
    pub legend: bool,
    #[serde(default)]
    pub style: ChartStyle,
    #[serde(default)]
    pub y_min: Option<f32>,
    #[serde(default)]
    pub y_max: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeriesSpec {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// Initial visibility. The legend still shows hidden series so a click can
    /// bring them back.
    #[serde(default)]
    pub visible: Option<bool>,
    #[serde(default)]
    pub kind: SeriesKind,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub color2: Option<[u8; 3]>,
    #[serde(default)]
    pub width: Option<f32>,
    #[serde(default)]
    pub fill_opacity: Option<f32>,
    #[serde(default)]
    pub smooth: bool,
    #[serde(default)]
    pub source: Option<String>,
    /// Optional lower boundary for a `band` series.
    #[serde(default)]
    pub band_source: Option<String>,
    /// OHLC source names for a `candle` series. `source` is the close when
    /// `close_source` is omitted.
    #[serde(default)]
    pub open_source: Option<String>,
    #[serde(default)]
    pub high_source: Option<String>,
    #[serde(default)]
    pub low_source: Option<String>,
    #[serde(default)]
    pub close_source: Option<String>,
    /// Bar baseline, defaults to zero.
    #[serde(default)]
    pub baseline: Option<f32>,
    /// Fraction of a category slot used by a grouped bar series.
    #[serde(default)]
    pub bar_width: Option<f32>,
    #[serde(default)]
    pub bar_radius: Option<f32>,
    /// Sample every Nth point. Useful for bars and candles, where one mark per
    /// 5 Hz sample is denser than the eye can resolve.
    #[serde(default)]
    pub stride: Option<usize>,
    #[serde(default)]
    pub up_color: Option<[u8; 3]>,
    #[serde(default)]
    pub down_color: Option<[u8; 3]>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SeriesKind {
    #[default]
    Line,
    Area,
    Scatter,
    Bar,
    Candle,
    Band,
    #[serde(rename = "stacked_area")]
    StackedArea,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChartStyle {
    #[serde(default = "title_size")]
    pub title_size: f32,
    #[serde(default = "label_size")]
    pub label_size: f32,
    #[serde(default = "plot_opacity")]
    pub plot_opacity: f32,
    #[serde(default = "grid_opacity")]
    pub grid_opacity: f32,
    #[serde(default = "axis_opacity")]
    pub axis_opacity: f32,
    #[serde(default = "line_width")]
    pub line_width: f32,
    #[serde(default = "glow")]
    pub glow: f32,
    #[serde(default = "glow_width")]
    pub glow_width: f32,
    #[serde(default = "area_opacity")]
    pub area_opacity: f32,
    #[serde(default = "smooth_steps")]
    pub smooth_steps: usize,
}

fn title_size() -> f32 {
    18.0
}
fn label_size() -> f32 {
    9.0
}
fn plot_opacity() -> f32 {
    0.14
}
fn grid_opacity() -> f32 {
    0.09
}
fn axis_opacity() -> f32 {
    0.18
}
fn line_width() -> f32 {
    2.0
}
fn glow() -> f32 {
    0.16
}
fn glow_width() -> f32 {
    6.0
}
fn area_opacity() -> f32 {
    0.22
}
fn smooth_steps() -> usize {
    6
}

impl Default for ChartStyle {
    fn default() -> Self {
        Self {
            title_size: title_size(),
            label_size: label_size(),
            plot_opacity: plot_opacity(),
            grid_opacity: grid_opacity(),
            axis_opacity: axis_opacity(),
            line_width: line_width(),
            glow: glow(),
            glow_width: glow_width(),
            area_opacity: area_opacity(),
            smooth_steps: smooth_steps(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphSpec {
    pub id: String,
    #[serde(default)]
    pub rect: RectSpec,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub node_count: usize,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub colors: Vec<[u8; 3]>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MapSpec {
    pub id: String,
    #[serde(default)]
    pub rect: RectSpec,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub routes: Vec<RouteSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteSpec {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AtlasSpec {
    pub id: String,
    #[serde(default)]
    pub rect: RectSpec,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub colors: Vec<[u8; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitAction {
    ToggleSeries { layer: usize, series: usize },
}

struct LayerRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    time: f32,
    metrics: Metrics,
}

pub struct LiveViz {
    spec: LiveSpec,
    theme: Theme,
    time: f32,
    frame: u64,
    last_update: Instant,
    history: VecDeque<Sample>,
    last_index: i64,
    atlas_points: Vec<(f32, f32, usize)>,
    hidden: Vec<Vec<bool>>,
    scale: f32,
    viewport: (f32, f32),
    ui_scale: f32,
    pointer: Option<(f32, f32)>,
}

impl LiveViz {
    pub fn new(spec: LiveSpec) -> Self {
        let atlas_points = (0..ATLAS_POINTS)
            .map(|index| {
                let cluster = index % 3;
                let angle = index as f32 * 2.399963 + cluster as f32 * 1.7;
                let radius = 0.12 + ((index * 37) % 100) as f32 / 100.0 * 0.20;
                (
                    0.5 + angle.cos() * radius,
                    0.5 + angle.sin() * radius * 0.78,
                    cluster,
                )
            })
            .collect();
        let hidden = spec
            .layers
            .iter()
            .map(|layer| match layer {
                LayerSpec::Chart(chart) => chart
                    .series
                    .iter()
                    .map(|series| series.visible == Some(false))
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        let viewport = (spec.width.max(1.0), spec.height.max(1.0));
        let ui_scale = spec.ui_scale.clamp(0.5, 4.0);
        let mut live = Self {
            spec,
            theme: Theme::default(),
            time: 0.0,
            frame: 0,
            last_update: Instant::now(),
            history: VecDeque::with_capacity(CHART_POINTS),
            last_index: 0,
            atlas_points,
            hidden,
            scale: 1.0,
            viewport,
            ui_scale,
            pointer: None,
        };
        live.rebuild(0.0);
        live
    }

    fn sample(index: i64) -> Sample {
        let raw = index as f32 * SAMPLE_SECONDS;
        Sample {
            time: raw,
            metrics: metrics(raw),
        }
    }

    fn sample_index(time: f32) -> i64 {
        // Do not lose a sample when the host clock lands a hair below a
        // 0.2-second boundary after accumulating real deltas.
        ((time.max(0.0) + 0.0001) / SAMPLE_SECONDS).floor() as i64
    }

    /// Fill the visible window for an absolute time. This is the only path that
    /// changes historical samples; normal playback only appends at the sample
    /// cadence.
    fn rebuild(&mut self, time: f32) {
        self.time = time.max(0.0);
        self.last_index = Self::sample_index(self.time);
        self.history.clear();
        let first = self.last_index - CHART_POINTS as i64 + 1;
        for index in first..=self.last_index {
            self.history.push_back(Self::sample(index));
        }
    }

    fn append_until(&mut self, time: f32) {
        let target = Self::sample_index(time);
        while self.last_index < target {
            self.last_index += 1;
            self.history.push_back(Self::sample(self.last_index));
            if self.history.len() > CHART_POINTS {
                self.history.pop_front();
            }
        }
    }

    pub fn spec(&self) -> &LiveSpec {
        &self.spec
    }

    /// Physical pixels per logical spec pixel. The host sets this from the
    /// window scale factor so text stays crisp instead of shrinking on Retina.
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.clamp(0.5, 4.0);
    }

    /// Layout against the actual window instead of stretching a fixed design
    /// canvas. Width and height are logical pixels.
    pub fn set_viewport(&mut self, width: f32, height: f32) {
        if width > 1.0 && height > 1.0 {
            self.viewport = (width, height);
        }
    }

    /// Logical window size before the Lua-owned UI zoom.
    pub fn viewport(&self) -> (f32, f32) {
        self.viewport
    }

    /// Design units the painter lays out in. `ui_scale > 1` shows fewer, bigger
    /// elements; the panel fractions still span the window.
    pub fn design_size(&self) -> (f32, f32) {
        (
            (self.viewport.0 / self.ui_scale).max(1.0),
            (self.viewport.1 / self.ui_scale).max(1.0),
        )
    }

    /// Pointer position in design pixels.
    pub fn set_pointer(&mut self, pointer: Option<(f32, f32)>) {
        self.pointer = pointer;
    }

    /// Advance by an explicit delta, for a host that already owns the clock.
    pub fn advance(&mut self, dt: f32) -> Scene {
        let next = self.time + dt.clamp(0.0, 0.25);
        self.append_until(next);
        self.time = next;
        self.frame = self.frame.wrapping_add(1);
        self.scene()
    }

    /// Seek to an absolute scene time. Used by deterministic frame capture;
    /// the live loop uses `tick`, which clamps a suspended process jump.
    pub fn seek(&mut self, time: f32) -> Scene {
        self.rebuild(time);
        self.frame = self.frame.wrapping_add(1);
        self.scene()
    }

    /// Advance by real elapsed time, not a fixed assumed tick.
    pub fn tick(&mut self, now: Instant) -> Option<Scene> {
        let dt = now.duration_since(self.last_update).as_secs_f32();
        if dt < 0.001 {
            return None;
        }
        self.last_update = now;
        Some(self.advance(dt))
    }

    pub fn scene(&self) -> Scene {
        let mut nodes = Vec::with_capacity(512);
        let gap = self.spec.gap.max(0.0);
        let (width, height) = self.design_size();
        let mut painter = Painter::new(
            &mut nodes,
            width,
            height,
            self.theme,
            self.scale * self.ui_scale,
        );
        for (layer_index, layer) in self.spec.layers.iter().enumerate() {
            match layer {
                LayerSpec::Chart(spec) => {
                    let hidden = self
                        .hidden
                        .get(layer_index)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    draw_chart(
                        &mut painter,
                        spec,
                        self.time,
                        gap,
                        hidden,
                        self.pointer,
                        &self.history,
                    );
                }
                LayerSpec::Graph(spec) => draw_graph(&mut painter, spec, self.time, gap),
                LayerSpec::Map(spec) => draw_map(&mut painter, spec, self.time, gap),
                LayerSpec::Atlas(spec) => {
                    draw_atlas(&mut painter, spec, self.time, gap, &self.atlas_points)
                }
            }
        }
        Scene::new(nodes)
    }

    pub fn reset(&mut self) {
        self.frame = 0;
        for layer in &mut self.hidden {
            layer.fill(false);
        }
        self.rebuild(0.0);
    }

    pub fn toggle_series(&mut self, layer: usize, series: usize) {
        if let Some(row) = self.hidden.get_mut(layer)
            && let Some(value) = row.get_mut(series)
        {
            *value = !*value;
        }
    }

    /// Hit-test the chart legend band. Coordinates are window pixels.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<HitAction> {
        let gap = self.spec.gap.max(0.0);
        let (width, height) = self.design_size();
        for (layer_index, layer) in self.spec.layers.iter().enumerate() {
            let LayerSpec::Chart(spec) = layer else {
                continue;
            };
            let r = layer_rect(&spec.rect, width, height, gap);
            let layout = chart_layout(spec, &r);
            let Some(first) = layout.legend.first() else {
                continue;
            };
            let top = first.y - 2.0;
            let bottom = top + layout.legend_height.max(layout.legend_row_height);
            if y < top || y > bottom {
                continue;
            }
            for item in &layout.legend {
                if x >= item.x && x <= item.x + item.width {
                    return Some(HitAction::ToggleSeries {
                        layer: layer_index,
                        series: item.series,
                    });
                }
            }
        }
        None
    }

    pub fn render(&self, width: u32, height: u32) -> anyhow::Result<Frame> {
        let scene = self.scene();
        crate::render(&scene, width, height, self.theme)
    }
}

fn layer_rect(rect: &RectSpec, width: f32, height: f32, gap: f32) -> LayerRect {
    LayerRect {
        x: rect.x * width,
        y: rect.y * height,
        w: (rect.width * width - gap).max(1.0),
        h: (rect.height * height - gap).max(1.0),
    }
}

struct Painter<'a> {
    nodes: &'a mut Vec<Node>,
    width: f32,
    height: f32,
    theme: Theme,
    scale: f32,
    z: f32,
}

impl<'a> Painter<'a> {
    fn new(nodes: &'a mut Vec<Node>, width: f32, height: f32, theme: Theme, scale: f32) -> Self {
        Self {
            nodes,
            width: width.max(1.0),
            height: height.max(1.0),
            theme,
            scale,
            z: 0.0,
        }
    }

    fn nx(&self, x: f32) -> f32 {
        -4.0 + 8.0 * x / self.width
    }
    fn ny(&self, y: f32) -> f32 {
        4.0 - 8.0 * y / self.height
    }
    fn sw(&self, w: f32) -> f32 {
        8.0 * w / self.width
    }
    fn sh(&self, h: f32) -> f32 {
        8.0 * h / self.height
    }
    fn ts(&self, size: f32) -> f32 {
        size * self.scale / (self.theme.body * 1.25).max(1.0)
    }

    fn rect(
        &mut self,
        id: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: [u8; 3],
        opacity: f32,
        radius: f32,
    ) {
        self.z += 0.01;
        self.nodes.push(Node {
            id: id.into(),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [self.nx(x), self.ny(y), self.z],
                rotation: [0.0; 3],
                scale: [self.sw(w.max(1.0)), self.sh(h.max(1.0)), 1.0],
            },
            content: Content::Rect(Rect {
                ink: None,
                rgb: Some(color),
                opacity,
                radius: self.sw(radius).min(self.sh(radius)),
            }),
        });
    }

    fn text(&mut self, id: &str, value: &str, x: f32, y: f32, size: f32, color: [u8; 3]) {
        self.z += 0.01;
        self.nodes.push(Node {
            id: id.into(),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [self.nx(x), self.ny(y), self.z],
                rotation: [0.0; 3],
                scale: [self.ts(size), self.ts(size), 1.0],
            },
            content: Content::Text(Text {
                text: value.into(),
                ink: None,
                rgb: Some(color),
                opacity: 1.0,
            }),
        });
    }

    fn polyline(
        &mut self,
        id: &str,
        points: &[(f32, f32)],
        width: f32,
        color: [u8; 3],
        opacity: f32,
    ) {
        if points.len() < 2 {
            return;
        }
        self.z += 0.02;
        let points = points
            .iter()
            .map(|(x, y)| [self.nx(*x), self.ny(*y)])
            .collect();
        self.nodes.push(Node {
            id: id.into(),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [0.0, 0.0, self.z],
                rotation: [0.0; 3],
                scale: [1.0, 1.0, 1.0],
            },
            content: Content::Polyline(Polyline {
                points,
                ink: None,
                rgb: Some(color),
                rgb2: None,
                width: self.sh(width),
                opacity,
                join: soil_scene::LineJoin::Round,
                miter_limit: 4.0,
            }),
        });
    }

    fn polyline_gradient(
        &mut self,
        id: &str,
        points: &[(f32, f32)],
        width: f32,
        color: [u8; 3],
        color2: [u8; 3],
        opacity: f32,
    ) {
        if points.len() < 2 {
            return;
        }
        self.z += 0.02;
        let points = points
            .iter()
            .map(|(x, y)| [self.nx(*x), self.ny(*y)])
            .collect();
        self.nodes.push(Node {
            id: id.into(),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [0.0, 0.0, self.z],
                rotation: [0.0; 3],
                scale: [1.0, 1.0, 1.0],
            },
            content: Content::Polyline(Polyline {
                points,
                ink: None,
                rgb: Some(color),
                rgb2: Some(color2),
                width: self.sh(width),
                opacity,
                join: soil_scene::LineJoin::Round,
                miter_limit: 4.0,
            }),
        });
    }

    fn area(
        &mut self,
        id: &str,
        points: &[(f32, f32)],
        baseline: f32,
        color: [u8; 3],
        opacity: f32,
    ) {
        if points.len() < 2 {
            return;
        }
        self.z += 0.01;
        let points = points
            .iter()
            .map(|(x, y)| [self.nx(*x), self.ny(*y)])
            .collect();
        self.nodes.push(Node {
            id: id.into(),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [0.0, 0.0, self.z],
                rotation: [0.0; 3],
                scale: [1.0, 1.0, 1.0],
            },
            content: Content::Area(Area {
                points,
                baseline: self.ny(baseline),
                ink: None,
                rgb: Some(color),
                opacity,
                base_opacity: Some(0.0),
            }),
        });
    }

    fn band(
        &mut self,
        id: &str,
        upper: &[(f32, f32)],
        lower: &[(f32, f32)],
        color: [u8; 3],
        opacity: f32,
    ) {
        if upper.len() < 2 || upper.len() != lower.len() {
            return;
        }
        self.z += 0.01;
        let upper = upper
            .iter()
            .map(|(x, y)| [self.nx(*x), self.ny(*y)])
            .collect();
        let lower = lower
            .iter()
            .map(|(x, y)| [self.nx(*x), self.ny(*y)])
            .collect();
        self.nodes.push(Node {
            id: id.into(),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [0.0, 0.0, self.z],
                rotation: [0.0; 3],
                scale: [1.0, 1.0, 1.0],
            },
            content: Content::Band(Band {
                upper,
                lower,
                ink: None,
                rgb: Some(color),
                opacity,
            }),
        });
    }

    fn candles(
        &mut self,
        id: &str,
        candles: &[(f32, Candle)],
        width: f32,
        up: [u8; 3],
        down: [u8; 3],
        opacity: f32,
    ) {
        if candles.is_empty() {
            return;
        }
        self.z += 0.02;
        let candles = candles
            .iter()
            .map(|(x, candle)| Candle {
                x: self.nx(*x),
                open: self.ny(candle.open),
                high: self.ny(candle.high),
                low: self.ny(candle.low),
                close: self.ny(candle.close),
            })
            .collect();
        self.nodes.push(Node {
            id: id.into(),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [0.0, 0.0, self.z],
                rotation: [0.0; 3],
                scale: [1.0, 1.0, 1.0],
            },
            content: Content::Candles(Candles {
                candles,
                width: self.sw(width),
                up_ink: None,
                down_ink: None,
                up_rgb: Some(up),
                down_rgb: Some(down),
                opacity,
            }),
        });
    }

    fn points(&mut self, id: &str, points: &[(f32, f32)], size: f32, color: [u8; 3], opacity: f32) {
        if points.is_empty() {
            return;
        }
        self.z += 0.02;
        let points = points
            .iter()
            .map(|(x, y)| [self.nx(*x), self.ny(*y)])
            .collect();
        self.nodes.push(Node {
            id: id.into(),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [0.0, 0.0, self.z],
                rotation: [0.0; 3],
                scale: [1.0, 1.0, 1.0],
            },
            content: Content::Points(Points {
                points,
                size: [self.sw(size), self.sh(size)],
                shape: soil_scene::PointShape::Circle,
                ink: None,
                rgb: Some(color),
                opacity,
            }),
        });
    }

    fn heatmap(
        &mut self,
        id: &str,
        values: Vec<f32>,
        rows: u32,
        cols: u32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        max: f32,
        opacity: f32,
    ) {
        self.z += 0.01;
        self.nodes.push(Node {
            id: id.into(),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [self.nx(x), self.ny(y), self.z],
                rotation: [0.0; 3],
                scale: [self.sw(w), self.sh(h), 1.0],
            },
            content: Content::Heatmap(Heatmap {
                rows,
                cols,
                values,
                min: Some(0.0),
                max: Some(max.max(1.0)),
                opacity,
                zero_transparent: true,
            }),
        });
    }
}

const CHART_COLORS: [[u8; 3]; 3] = [[0, 229, 255], [255, 45, 149], [176, 38, 255]];
const GRAPH_COLORS: [[u8; 3]; 3] = [[0, 229, 255], [0, 245, 160], [255, 107, 0]];
const MAP_COLORS: [[u8; 3]; 3] = [[0, 229, 255], [0, 245, 160], [255, 211, 0]];
const ATLAS_COLORS: [[u8; 3]; 3] = [[0, 229, 255], [255, 45, 149], [176, 38, 255]];

fn panel(painter: &mut Painter, id: &str, r: &LayerRect, title: Option<&str>) {
    painter.rect(
        &format!("{id}-panel"),
        r.x,
        r.y,
        r.w,
        r.h,
        [26, 26, 46],
        0.72,
        10.0,
    );
    if let Some(title) = title {
        painter.text(
            &format!("{id}-title"),
            title,
            r.x + 16.0,
            r.y + 12.0,
            18.0,
            [224, 224, 224],
        );
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Metrics {
    memory: f32,
    cpu: f32,
    p50: f32,
    p95: f32,
    p99: f32,
    throughput: f32,
    errors5xx: f32,
    errors4xx: f32,
    active: f32,
    waiting: f32,
    cost_open: f32,
    cost_high: f32,
    cost_low: f32,
    cost_close: f32,
}

fn clamp(value: f32, min: f32, max: f32) -> f32 {
    value.max(min).min(max)
}

fn noise(range: f32, elapsed: f32, seed: f32) -> f32 {
    let value = ((elapsed * 12.9898 + seed * 78.233).sin() * 43758.5453).fract();
    (value - 0.5) * 2.0 * range
}

fn sine_wave(elapsed: f32, period: f32, amplitude: f32) -> f32 {
    amplitude * (std::f32::consts::TAU * elapsed / period).sin()
}

/// A deterministic request-cost path. The live host owns this stream; the
/// candle chart only names the source.
fn cost_at(elapsed: f32) -> f32 {
    42.0 + elapsed * 0.018
        + sine_wave(elapsed, 47.0, 2.1)
        + sine_wave(elapsed, 11.0, 0.55)
        + noise(0.32, elapsed, 21.0)
}

fn cost_ohlc(elapsed: f32) -> (f32, f32, f32, f32) {
    let span = 0.2;
    let open = cost_at(elapsed - span);
    let close = cost_at(elapsed);
    let mut high = open.max(close);
    let mut low = open.min(close);
    for step in 1..=4 {
        let sample = cost_at(elapsed - span + span * step as f32 / 5.0);
        high = high.max(sample);
        low = low.min(sample);
    }
    let wick = 0.06 + noise(0.03, elapsed, 22.0).abs();
    (open, high + wick, (low - wick).max(0.0), close)
}

fn metrics(elapsed: f32) -> Metrics {
    let memory = if elapsed < 60.0 {
        45.0 + noise(3.0, elapsed, 1.0) + sine_wave(elapsed, 30.0, 0.9)
    } else if elapsed < 120.0 {
        45.0 + (elapsed - 60.0) * 0.5 + noise(3.0, elapsed, 2.0) + sine_wave(elapsed, 30.0, 0.9)
    } else if elapsed < 155.0 {
        75.0 + (elapsed - 120.0) * 0.8 + noise(3.0, elapsed, 3.0) + sine_wave(elapsed, 30.0, 0.9)
    } else if elapsed < 175.0 {
        95.0 + noise(1.0, elapsed, 4.0)
    } else {
        50.0 + (95.0 - 50.0) * (-0.05 * (elapsed - 175.0)).exp() + noise(2.0, elapsed, 5.0)
    };
    let memory = clamp(memory, 0.0, 100.0);
    let gc_pressure = (memory - 60.0).max(0.0) * 1.2;
    let cpu = clamp(25.0 + gc_pressure + noise(3.0, elapsed, 6.0), 0.0, 100.0);
    let gc_jitter = (memory - 55.0).max(0.0) * 2.0;
    let p50 = (45.0 + gc_jitter + noise(5.0, elapsed, 7.0)).max(0.0);
    let p95_multiplier = 1.8 + (memory - 70.0).max(0.0) * 0.06;
    let p95 = p50 * p95_multiplier + noise(10.0, elapsed, 8.0);
    let p99_multiplier = 1.5 + (memory - 75.0).max(0.0) * 0.08;
    let p99 = p95 * p99_multiplier + noise(20.0, elapsed, 9.0);
    let hold_factor = (p95 - 100.0).max(0.0) * 0.5;
    let active = clamp(80.0 + hold_factor + noise(3.0, elapsed, 10.0), 0.0, 200.0);
    let waiting = ((active - 180.0) * 3.0 + noise(2.0, elapsed, 11.0)).max(0.0);
    let base_throughput = 1200.0;
    let throughput_factor = (1.0 - ((active - 100.0).max(0.0) / 100.0) * 0.8).max(0.05);
    let throughput = (base_throughput * throughput_factor
        + noise(30.0, elapsed, 12.0)
        + sine_wave(elapsed, 30.0, 24.0))
    .max(0.0);
    let error_base = (p99 - 400.0).max(0.0) * 0.02;
    let errors5xx = (error_base.powf(1.5) + noise(0.5, elapsed, 13.0)).max(0.0);
    let errors4xx = (3.5 + noise(1.0, elapsed, 14.0)).max(0.0);
    let (cost_open, cost_high, cost_low, cost_close) = cost_ohlc(elapsed);
    Metrics {
        memory,
        cpu,
        p50,
        p95: p95.max(0.0),
        p99: p99.max(0.0),
        throughput,
        errors5xx,
        errors4xx,
        active,
        waiting,
        cost_open,
        cost_high,
        cost_low,
        cost_close,
    }
}

fn source_value(source: &str, metrics: Metrics) -> Option<f32> {
    Some(match source {
        "latency.p50" => metrics.p50,
        "latency.p95" => metrics.p95,
        "latency.p99" => metrics.p99,
        "throughput" => metrics.throughput,
        "memory" => metrics.memory,
        "cpu" => metrics.cpu,
        "errors.5xx" => metrics.errors5xx,
        "errors.4xx" => metrics.errors4xx,
        "connections.active" => metrics.active,
        "connections.waiting" => metrics.waiting,
        "cost" | "cost.close" => metrics.cost_close,
        "cost.open" => metrics.cost_open,
        "cost.high" => metrics.cost_high,
        "cost.low" => metrics.cost_low,
        _ => return None,
    })
}

/// Each chart reads the same append-only resident history, oldest at x=0.
fn chart_value(samples: &VecDeque<Sample>, source: Option<&str>, point: usize) -> f32 {
    let source = source.unwrap_or("latency.p50");
    samples
        .get(point)
        .and_then(|sample| source_value(source, sample.metrics))
        .unwrap_or(0.0)
}

fn series_stride(series: &SeriesSpec) -> usize {
    series.stride.unwrap_or(1).clamp(1, 64)
}

fn point_indices(series: &SeriesSpec) -> impl Iterator<Item = usize> {
    let stride = series_stride(series);
    (0..CHART_POINTS).step_by(stride)
}

fn candle_values(
    samples: &VecDeque<Sample>,
    series: &SeriesSpec,
    point: usize,
) -> (f32, f32, f32, f32) {
    let close_source = series.close_source.as_deref().or(series.source.as_deref());
    let open_source = series.open_source.as_deref().or(close_source);
    let high_source = series.high_source.as_deref().or(close_source);
    let low_source = series.low_source.as_deref().or(close_source);
    let open = chart_value(samples, open_source, point);
    let high = chart_value(samples, high_source, point);
    let low = chart_value(samples, low_source, point);
    let close = chart_value(samples, close_source, point);
    (
        open,
        high.max(open).max(close),
        low.min(open).min(close),
        close,
    )
}

fn band_values(samples: &VecDeque<Sample>, series: &SeriesSpec, point: usize) -> (f32, f32) {
    let upper = chart_value(samples, series.source.as_deref(), point);
    let lower = series
        .band_source
        .as_deref()
        .map(|source| chart_value(samples, Some(source), point))
        .unwrap_or(upper);
    (upper, lower)
}

fn series_value(samples: &VecDeque<Sample>, series: &SeriesSpec, point: usize) -> f32 {
    match series.kind {
        SeriesKind::Candle => candle_values(samples, series, point).3,
        SeriesKind::Band => {
            let (upper, lower) = band_values(samples, series, point);
            (upper + lower) * 0.5
        }
        _ => chart_value(samples, series.source.as_deref(), point),
    }
}

fn chart_bounds(spec: &ChartSpec, hidden: &[bool], samples: &VecDeque<Sample>) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut bar_lo = f32::INFINITY;
    let mut bar_hi = f32::NEG_INFINITY;
    let mut stack = vec![0.0f32; CHART_POINTS];
    for (index, series) in spec.series.iter().enumerate() {
        if hidden.get(index).copied().unwrap_or(false) {
            continue;
        }
        match series.kind {
            SeriesKind::Candle => {
                for point in point_indices(series) {
                    let (_, high, low, _) = candle_values(samples, series, point);
                    lo = lo.min(low);
                    hi = hi.max(high);
                }
            }
            SeriesKind::Band => {
                for point in point_indices(series) {
                    let (upper, lower) = band_values(samples, series, point);
                    lo = lo.min(lower);
                    hi = hi.max(upper);
                }
            }
            SeriesKind::StackedArea => {
                for point in point_indices(series) {
                    let lower = stack[point];
                    let upper = lower + chart_value(samples, series.source.as_deref(), point);
                    lo = lo.min(lower);
                    hi = hi.max(upper);
                    stack[point] = upper;
                }
            }
            SeriesKind::Bar => {
                let baseline = series.baseline.unwrap_or(0.0);
                bar_lo = bar_lo.min(baseline);
                bar_hi = bar_hi.max(baseline);
                for point in point_indices(series) {
                    let value = chart_value(samples, series.source.as_deref(), point);
                    lo = lo.min(value);
                    hi = hi.max(value);
                }
            }
            _ => {
                let source = series.source.as_deref();
                for point in point_indices(series) {
                    let value = chart_value(samples, source, point);
                    lo = lo.min(value);
                    hi = hi.max(value);
                }
            }
        }
    }
    if bar_lo.is_finite() {
        lo = lo.min(bar_lo);
        hi = hi.max(bar_hi);
    }
    if !lo.is_finite() || !hi.is_finite() {
        lo = 0.0;
        hi = 1.0;
    }
    if (hi - lo).abs() < f32::EPSILON {
        hi = lo + 1.0;
    }
    let raw_lo = lo;
    let raw_hi = hi;
    let span = (raw_hi - raw_lo).abs();
    let padding = if span > f32::EPSILON {
        span * 0.05
    } else {
        (raw_hi.abs() * 0.05).max(0.5)
    };
    let lo = spec
        .y_min
        .map(|min| min.min(raw_lo))
        .unwrap_or(raw_lo - padding);
    let hi = spec
        .y_max
        .map(|max| max.max(raw_hi))
        .unwrap_or(raw_hi + padding);
    nice_bounds(lo, hi)
}

fn nice_step(span: f32, target: usize) -> f32 {
    if !span.is_finite() || span <= 0.0 {
        return 1.0;
    }
    let target = target.max(1) as f32;
    let raw = span / target;
    let magnitude = 10f32.powf(raw.log10().floor());
    for candidate in [1.0, 2.0, 2.5, 5.0, 10.0] {
        let step = candidate * magnitude;
        if raw <= step {
            return step;
        }
    }
    10.0 * magnitude
}

fn nice_bounds(lo: f32, hi: f32) -> (f32, f32) {
    if !lo.is_finite() || !hi.is_finite() || hi < lo {
        return (0.0, 1.0);
    }
    let step = nice_step((hi - lo).max(f32::EPSILON), 6);
    let lo = (lo / step).floor() * step;
    let hi = (hi / step).ceil() * step;
    if hi <= lo { (lo, lo + step) } else { (lo, hi) }
}

fn format_axis_value(value: f32, step: f32) -> String {
    let abs = value.abs();
    if step >= 1000.0 {
        if abs >= 1000.0 {
            format!("{:.1}k", value / 1000.0)
        } else {
            format!("{value:.0}")
        }
    } else if step >= 10.0 {
        format!("{value:.0}")
    } else if step >= 1.0 {
        format!("{value:.0}")
    } else if step >= 0.1 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

fn series_color(series: &SeriesSpec, index: usize) -> [u8; 3] {
    series
        .color
        .unwrap_or(CHART_COLORS[index % CHART_COLORS.len()])
}

fn series_point_color(
    samples: &VecDeque<Sample>,
    series: &SeriesSpec,
    index: usize,
    point: usize,
) -> [u8; 3] {
    if series.kind == SeriesKind::Candle {
        let (open, _, _, close) = candle_values(samples, series, point);
        return if close >= open {
            series.up_color.unwrap_or([34, 197, 94])
        } else {
            series.down_color.unwrap_or([239, 68, 68])
        };
    }
    series_color(series, index)
}

#[derive(Debug, Clone)]
struct LegendItem {
    series: usize,
    x: f32,
    y: f32,
    width: f32,
}

#[derive(Debug, Clone)]
struct ChartLayout {
    left: f32,
    top: f32,
    plot_w: f32,
    plot_h: f32,
    plot_bottom: f32,
    legend: Vec<LegendItem>,
    legend_row_height: f32,
    legend_height: f32,
}

fn approx_text_width(text: &str, size: f32) -> f32 {
    text.chars()
        .map(|ch| {
            if ch.is_whitespace() {
                size * 0.32
            } else {
                size * 0.58
            }
        })
        .sum()
}

fn chart_layout(spec: &ChartSpec, r: &LayerRect) -> ChartLayout {
    let left = r.x + 58.0;
    let top = r.y + 54.0;
    let plot_w = (r.w - 78.0).max(16.0);
    let font = spec.style.label_size.max(9.0);
    let mark = 12.0;
    let gap = 16.0;
    let row_height = (font + 9.0).max(18.0);
    let mut legend = Vec::new();
    let mut rows = 0usize;
    if spec.legend {
        let mut x = 0.0;
        let mut row = 0usize;
        for (index, series) in spec.series.iter().enumerate() {
            let label = series
                .label
                .clone()
                .unwrap_or_else(|| format!("series {}", index + 1));
            let width = mark + 6.0 + approx_text_width(&label, font);
            if x > 0.0 && x + width > plot_w {
                row += 1;
                x = 0.0;
            }
            legend.push(LegendItem {
                series: index,
                x: left + x,
                y: row as f32 * row_height,
                width,
            });
            x += width + gap;
            rows = rows.max(row + 1);
        }
    }
    let legend_height = rows as f32 * row_height;
    let legend_space = if legend.is_empty() {
        14.0
    } else {
        legend_height + 18.0
    };
    let plot_h = (r.h - 62.0 - legend_space).max(16.0);
    let plot_bottom = top + plot_h;
    let legend_y = plot_bottom + 26.0;
    for item in &mut legend {
        item.y += legend_y;
    }
    ChartLayout {
        left,
        top,
        plot_w,
        plot_h,
        plot_bottom,
        legend,
        legend_row_height: row_height,
        legend_height,
    }
}

fn draw_chart(
    painter: &mut Painter,
    spec: &ChartSpec,
    time: f32,
    gap: f32,
    hidden: &[bool],
    pointer: Option<(f32, f32)>,
    samples: &VecDeque<Sample>,
) {
    let r = layer_rect(&spec.rect, painter.width, painter.height, gap);
    let style = &spec.style;
    panel(painter, &spec.id, &r, spec.title.as_deref());
    if let Some(subtitle) = &spec.subtitle {
        painter.text(
            &format!("{}-subtitle", spec.id),
            subtitle,
            r.x + 16.0,
            r.y + 36.0,
            10.0,
            [145, 155, 170],
        );
    }
    let layout = chart_layout(spec, &r);
    let left = layout.left;
    let top = layout.top;
    let plot_w = layout.plot_w;
    let plot_h = layout.plot_h;
    let plot_bottom = layout.plot_bottom;
    painter.rect(
        &format!("{}-plot", spec.id),
        left,
        top,
        plot_w,
        plot_h,
        [17, 19, 29],
        style.plot_opacity,
        10.0,
    );
    painter.rect(
        &format!("{}-axis-y", spec.id),
        left,
        top,
        1.0,
        plot_h,
        [145, 155, 170],
        style.axis_opacity,
        0.0,
    );
    painter.rect(
        &format!("{}-axis-x", spec.id),
        left,
        plot_bottom,
        plot_w,
        1.0,
        [145, 155, 170],
        style.axis_opacity,
        0.0,
    );

    let xlo = 0.0;
    let xhi = (CHART_POINTS - 1) as f32;
    let (ylo, yhi) = chart_bounds(spec, hidden, samples);
    // Data values are immutable. The viewport, not the samples, moves: a real
    // sample clock leaves a fractional phase between appends, so old marks
    // drift left smoothly instead of being rewritten in place.
    let phase = samples
        .back()
        .map(|sample| ((time - sample.time) / SAMPLE_SECONDS).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let sx = |x: f32| left + (x - phase - xlo) / (xhi - xlo) * plot_w;
    let sy = |y: f32| plot_bottom - (y - ylo) / (yhi - ylo) * plot_h;
    let y_step = nice_step(yhi - ylo, 6);
    let y_ticks = (((yhi - ylo) / y_step).round() as usize).clamp(1, 12);
    for tick in 0..=y_ticks {
        let value = ylo + tick as f32 * y_step;
        if value > yhi + y_step * 0.01 {
            break;
        }
        let y = sy(value);
        painter.rect(
            &format!("{}-grid-{tick}", spec.id),
            left,
            y,
            plot_w,
            1.0,
            [145, 155, 170],
            style.grid_opacity,
            0.0,
        );
        let label = format_axis_value(value, y_step);
        let label_w = approx_text_width(&label, style.label_size);
        painter.text(
            &format!("{}-ylabel-{tick}", spec.id),
            &label,
            left - label_w - 10.0,
            y - style.label_size * 0.55,
            style.label_size,
            [145, 155, 170],
        );
    }
    for tick in 0..=4 {
        let value = xhi * tick as f32 / 4.0;
        let x = sx(value);
        let seconds = (value - phase - (CHART_POINTS - 1) as f32) * SAMPLE_SECONDS;
        let label = if tick == 4 {
            "now".to_string()
        } else {
            format!("{seconds:.0}s")
        };
        let label_w = approx_text_width(&label, style.label_size);
        painter.text(
            &format!("{}-xlabel-{tick}", spec.id),
            &label,
            (x - label_w * 0.5).max(left),
            plot_bottom + 8.0,
            style.label_size,
            [145, 155, 170],
        );
    }

    let bar_count = spec
        .series
        .iter()
        .enumerate()
        .filter(|(index, series)| {
            series.kind == SeriesKind::Bar && !hidden.get(*index).copied().unwrap_or(false)
        })
        .count()
        .max(1);
    let bar_stride = spec
        .series
        .iter()
        .find(|series| series.kind == SeriesKind::Bar)
        .map(series_stride)
        .unwrap_or(1);
    let bar_slots = (0..CHART_POINTS).step_by(bar_stride).count().max(1) as f32;
    let mut bar_rank = 0;
    let mut stack = vec![0.0f32; CHART_POINTS];

    for (index, series) in spec.series.iter().enumerate() {
        if hidden.get(index).copied().unwrap_or(false) {
            continue;
        }
        let color = series_color(series, index);
        let points: Vec<usize> = point_indices(series).collect();
        match series.kind {
            SeriesKind::StackedArea => {
                let mut upper = Vec::with_capacity(points.len());
                let mut lower = Vec::with_capacity(points.len());
                for &point in &points {
                    let lower_value = stack[point];
                    let upper_value =
                        lower_value + chart_value(samples, series.source.as_deref(), point);
                    stack[point] = upper_value;
                    upper.push((sx(point as f32), sy(upper_value.clamp(ylo, yhi))));
                    lower.push((sx(point as f32), sy(lower_value.clamp(ylo, yhi))));
                }
                painter.band(
                    &format!("{}-{}-stack", spec.id, index),
                    &upper,
                    &lower,
                    color,
                    series.fill_opacity.unwrap_or(0.28),
                );
                painter.polyline(
                    &format!("{}-{}-stack-top", spec.id, index),
                    &upper,
                    series.width.unwrap_or(1.4),
                    color,
                    0.9,
                );
            }
            SeriesKind::Bar => {
                let group_slot = plot_w / bar_slots;
                let group_w =
                    (group_slot * series.bar_width.unwrap_or(0.72).clamp(0.1, 1.0)).max(2.0);
                let bar_w = (group_w / bar_count as f32).max(1.0);
                let baseline = series.baseline.unwrap_or(0.0).clamp(ylo, yhi);
                let zero = sy(baseline);
                let radius = series.bar_radius.unwrap_or(2.0);
                for point in points {
                    let value =
                        chart_value(samples, series.source.as_deref(), point).clamp(ylo, yhi);
                    let y = sy(value);
                    let center = sx(point as f32);
                    let x = center - group_w * 0.5 + bar_rank as f32 * bar_w + bar_w * 0.09;
                    painter.rect(
                        &format!("{}-{}-bar-{point}", spec.id, index),
                        x,
                        y.min(zero),
                        (bar_w * 0.82).max(1.0),
                        (zero - y).abs().max(1.0),
                        color,
                        series.fill_opacity.unwrap_or(0.9),
                        radius,
                    );
                }
                bar_rank += 1;
            }
            SeriesKind::Candle => {
                let up = series.up_color.unwrap_or([34, 197, 94]);
                let down = series.down_color.unwrap_or([239, 68, 68]);
                let body = series.width.unwrap_or(6.0).max(1.0);
                let candles: Vec<(f32, Candle)> = points
                    .iter()
                    .map(|&point| {
                        let (open, high, low, close) = candle_values(samples, series, point);
                        (
                            sx(point as f32),
                            Candle {
                                x: 0.0,
                                open: sy(open),
                                high: sy(high),
                                low: sy(low),
                                close: sy(close),
                            },
                        )
                    })
                    .collect();
                painter.candles(
                    &format!("{}-{}-candles", spec.id, index),
                    &candles,
                    body,
                    up,
                    down,
                    1.0,
                );
            }
            SeriesKind::Band => {
                let upper: Vec<(f32, f32)> = points
                    .iter()
                    .map(|&point| {
                        let (upper, _) = band_values(samples, series, point);
                        (sx(point as f32), sy(upper.clamp(ylo, yhi)))
                    })
                    .collect();
                let lower: Vec<(f32, f32)> = points
                    .iter()
                    .map(|&point| {
                        let (_, lower) = band_values(samples, series, point);
                        (sx(point as f32), sy(lower.clamp(ylo, yhi)))
                    })
                    .collect();
                painter.band(
                    &format!("{}-{}-band", spec.id, index),
                    &upper,
                    &lower,
                    color,
                    series.fill_opacity.unwrap_or(style.area_opacity * 0.7),
                );
                let paint = series.width.unwrap_or(style.line_width);
                painter.polyline(
                    &format!("{}-{}-band-upper", spec.id, index),
                    &upper,
                    paint,
                    color,
                    0.82,
                );
                painter.polyline(
                    &format!("{}-{}-band-lower", spec.id, index),
                    &lower,
                    paint * 0.65,
                    series.color2.unwrap_or(color),
                    0.48,
                );
            }
            _ => {
                let mut screen: Vec<(f32, f32)> = points
                    .iter()
                    .map(|&point| {
                        let y =
                            chart_value(samples, series.source.as_deref(), point).clamp(ylo, yhi);
                        (sx(point as f32), sy(y))
                    })
                    .collect();
                if series.smooth {
                    screen = smooth(&screen, style.smooth_steps);
                }
                let paint = series.width.unwrap_or(style.line_width);
                if series.kind == SeriesKind::Area {
                    painter.area(
                        &format!("{}-{}-fill", spec.id, index),
                        &screen,
                        sy(ylo),
                        color,
                        series.fill_opacity.unwrap_or(style.area_opacity),
                    );
                }
                if style.glow > 0.0 {
                    painter.polyline(
                        &format!("{}-{}-glow-outer", spec.id, index),
                        &screen,
                        paint + style.glow_width * 1.7,
                        color,
                        style.glow * 0.35,
                    );
                    if let Some(color2) = series.color2 {
                        painter.polyline_gradient(
                            &format!("{}-{}-glow-inner", spec.id, index),
                            &screen,
                            paint + style.glow_width * 0.8,
                            color,
                            color2,
                            style.glow * 0.72,
                        );
                    } else {
                        painter.polyline(
                            &format!("{}-{}-glow-inner", spec.id, index),
                            &screen,
                            paint + style.glow_width * 0.8,
                            color,
                            style.glow * 0.72,
                        );
                    }
                }
                if series.kind == SeriesKind::Scatter {
                    painter.points(
                        &format!("{}-{}-points", spec.id, index),
                        &screen,
                        3.0,
                        color,
                        0.95,
                    );
                } else {
                    painter.polyline(
                        &format!("{}-{}", spec.id, index),
                        &screen,
                        paint,
                        color,
                        1.0,
                    );
                }
            }
        }
    }

    let legend_font = spec.style.label_size.max(9.0);
    for item in &layout.legend {
        let index = item.series;
        let series = &spec.series[index];
        let color = if series.kind == SeriesKind::Candle {
            series
                .up_color
                .unwrap_or_else(|| series_color(series, index))
        } else {
            series_color(series, index)
        };
        let hidden_series = hidden.get(index).copied().unwrap_or(false);
        let opacity = if hidden_series { 0.35 } else { 1.0 };
        let mid = item.y + legend_font * 0.5;
        match series.kind {
            SeriesKind::Scatter => painter.points(
                &format!("{}-legend-point-{index}", spec.id),
                &[(item.x + 6.0, mid)],
                6.0,
                color,
                opacity,
            ),
            SeriesKind::Bar | SeriesKind::Candle => painter.rect(
                &format!("{}-legend-mark-{index}", spec.id),
                item.x,
                mid - 5.0,
                11.0,
                10.0,
                color,
                opacity,
                2.0,
            ),
            SeriesKind::Band | SeriesKind::StackedArea => painter.rect(
                &format!("{}-legend-mark-{index}", spec.id),
                item.x,
                mid - 4.0,
                12.0,
                8.0,
                color,
                if series.kind == SeriesKind::StackedArea {
                    opacity * 0.72
                } else {
                    opacity * 0.45
                },
                2.0,
            ),
            _ => painter.polyline(
                &format!("{}-legend-line-{index}", spec.id),
                &[(item.x, mid), (item.x + 12.0, mid)],
                3.0,
                color,
                opacity,
            ),
        }
        let label = series
            .label
            .clone()
            .unwrap_or_else(|| format!("series {}", index + 1));
        painter.text(
            &format!("{}-legend-{index}", spec.id),
            &label,
            item.x + 18.0,
            item.y,
            legend_font,
            if hidden_series {
                [120, 130, 145]
            } else {
                [180, 190, 205]
            },
        );
    }

    if let Some((px, py)) = pointer
        && px >= left
        && px <= left + plot_w
        && py >= top
        && py <= plot_bottom
    {
        let data_x = xlo + (px - left) / plot_w * (xhi - xlo);
        let nearest = (data_x + phase)
            .round()
            .clamp(0.0, (CHART_POINTS - 1) as f32) as usize;
        let cx = sx(nearest as f32);
        painter.polyline(
            &format!("{}-crosshair", spec.id),
            &[(cx, top), (cx, plot_bottom)],
            1.0,
            [205, 214, 244],
            0.34,
        );
        let mut rows = Vec::new();
        for (index, series) in spec.series.iter().enumerate() {
            if hidden.get(index).copied().unwrap_or(false) {
                continue;
            }
            let value = series_value(samples, series, nearest);
            let color = series_point_color(samples, series, index, nearest);
            let y = sy(value.clamp(ylo, yhi));
            painter.points(
                &format!("{}-{}-hover", spec.id, index),
                &[(cx, y)],
                6.0,
                color,
                1.0,
            );
            let label = series
                .label
                .clone()
                .unwrap_or_else(|| format!("series {}", index + 1));
            rows.push((label, color, value));
        }
        if !rows.is_empty() {
            let width = 156.0;
            let height = 24.0 + rows.len() as f32 * 18.0;
            let tx = (cx + 14.0).min(left + plot_w - width).max(left);
            let ty = (py - height - 10.0)
                .max(top + 4.0)
                .min(plot_bottom - height);
            painter.rect(
                &format!("{}-tooltip", spec.id),
                tx,
                ty,
                width,
                height,
                [26, 26, 46],
                0.94,
                8.0,
            );
            let sample_time = samples
                .get(nearest)
                .map(|sample| sample.time - time)
                .unwrap_or(0.0);
            painter.text(
                &format!("{}-tooltip-title", spec.id),
                &format!("t {sample_time:.1}s"),
                tx + 10.0,
                ty + 7.0,
                10.0,
                [180, 190, 205],
            );
            for (row, (label, color, value)) in rows.iter().enumerate() {
                let y = ty + 25.0 + row as f32 * 18.0;
                painter.points(
                    &format!("{}-tooltip-dot-{row}", spec.id),
                    &[(tx + 13.0, y + 6.0)],
                    7.0,
                    *color,
                    1.0,
                );
                painter.text(
                    &format!("{}-tooltip-label-{row}", spec.id),
                    label,
                    tx + 22.0,
                    y,
                    10.0,
                    [205, 214, 244],
                );
                painter.text(
                    &format!("{}-tooltip-value-{row}", spec.id),
                    &format!("{value:.1}"),
                    tx + width - 48.0,
                    y,
                    10.0,
                    *color,
                );
            }
        }
    }
}

fn draw_graph(painter: &mut Painter, spec: &GraphSpec, time: f32, gap: f32) {
    let r = layer_rect(&spec.rect, painter.width, painter.height, gap);
    panel(painter, &spec.id, &r, spec.title.as_deref());
    let count = spec.node_count.max(3).min(12);
    let cx = r.x + r.w * 0.5;
    let cy = r.y + r.h * 0.54;
    let rx = r.w * 0.30;
    let ry = r.h * 0.28;
    let mut nodes = Vec::with_capacity(count);
    for index in 0..count {
        let angle = index as f32 / count as f32 * TAU + time * 0.34;
        let radius = 0.76 + 0.24 * (time * 0.8 + index as f32).sin();
        nodes.push((
            cx + angle.cos() * rx * radius,
            cy + (angle * 1.23).sin() * ry * radius,
        ));
    }
    for index in 0..count {
        let next = (index + 1) % count;
        painter.polyline(
            &format!("{}-edge-{index}", spec.id),
            &[nodes[index], nodes[next]],
            1.4,
            [145, 155, 170],
            0.30,
        );
        if index % 2 == 0 {
            let chord = (index + count / 2) % count;
            painter.polyline(
                &format!("{}-chord-{index}", spec.id),
                &[nodes[index], nodes[chord]],
                1.0,
                [145, 155, 170],
                0.16,
            );
        }
    }
    for index in 0..count {
        let color = spec
            .colors
            .get(index % spec.colors.len().max(1))
            .copied()
            .unwrap_or(GRAPH_COLORS[index % GRAPH_COLORS.len()]);
        painter.points(
            &format!("{}-node-{index}", spec.id),
            &[nodes[index]],
            7.0,
            color,
            1.0,
        );
        let label = spec
            .labels
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("node {index}"));
        painter.text(
            &format!("{}-label-{index}", spec.id),
            &label,
            nodes[index].0 + 8.0,
            nodes[index].1 - 6.0,
            9.0,
            [224, 224, 224],
        );
    }
}

fn draw_map(painter: &mut Painter, spec: &MapSpec, time: f32, gap: f32) {
    let r = layer_rect(&spec.rect, painter.width, painter.height, gap);
    panel(painter, &spec.id, &r, spec.title.as_deref());
    let pad = 22.0;
    let plot_top = r.y + 42.0;
    let center_lon = -30.0 + (time * 0.17).sin() * 12.0;
    let center_lat = 32.0 + (time * 0.13).cos() * 6.0;
    let zoom = 0.82 + (time * 0.09).sin() * 0.08;
    let span = 360.0 / 2f32.powf(zoom);
    let lon0 = center_lon - span / 2.0;
    let lon1 = center_lon + span / 2.0;
    let my = mercator(center_lat);
    let half = span.to_radians() / 2.0;
    let my0 = my - half;
    let my1 = my + half;
    let plot_w = (r.w - pad * 2.0).max(1.0);
    let plot_h = (r.y + r.h - pad - plot_top).max(1.0);
    let project = |lon: f32, lat: f32| {
        let x = r.x + pad + (lon - lon0) / (lon1 - lon0) * plot_w;
        let y = plot_top + (1.0 - (mercator(lat) - my0) / (my1 - my0)) * plot_h;
        (x, y)
    };
    for lon in (-160..=160).step_by(20) {
        let (x, _) = project(lon as f32, 0.0);
        painter.rect(
            &format!("{}-lon-{lon}", spec.id),
            x,
            r.y + pad,
            1.0,
            plot_h,
            [145, 155, 170],
            0.08,
            0.0,
        );
        painter.text(
            &format!("{}-lon-label-{lon}", spec.id),
            &format!("{lon}"),
            x + 3.0,
            r.y + r.h - 16.0,
            8.0,
            [145, 155, 170],
        );
    }
    for lat in (-80..=80).step_by(20) {
        let (_, y) = project(0.0, lat as f32);
        painter.rect(
            &format!("{}-lat-{lat}", spec.id),
            r.x + pad,
            y,
            plot_w,
            1.0,
            [145, 155, 170],
            0.08,
            0.0,
        );
        painter.text(
            &format!("{}-lat-label-{lat}", spec.id),
            &format!("{lat}"),
            r.x + 3.0,
            y + 2.0,
            8.0,
            [145, 155, 170],
        );
    }
    let routes = [
        vec![(-122.4, 37.8), (-100.0, 41.0), (-74.0, 40.7)],
        vec![(-122.4, 37.8), (-90.0, 30.0), (-80.0, 25.7)],
        vec![(-74.0, 40.7), (-40.0, 51.0), (2.35, 48.9)],
        vec![(-0.1, 51.5), (20.0, 50.0), (37.6, 55.7)],
    ];
    for (index, route) in routes.iter().enumerate() {
        let color = spec
            .routes
            .get(index)
            .and_then(|r| r.color)
            .unwrap_or(MAP_COLORS[index % MAP_COLORS.len()]);
        let points: Vec<(f32, f32)> = route.iter().map(|(lon, lat)| project(*lon, *lat)).collect();
        painter.polyline(
            &format!("{}-route-{index}", spec.id),
            &points,
            2.0,
            color,
            0.92,
        );
        let progress = (time * 0.16 + index as f32 * 0.23).fract();
        let seg = (progress * (route.len() - 1) as f32).floor() as usize;
        let last = (seg + 1).min(route.len() - 1);
        let local = progress * (route.len() - 1) as f32 - seg as f32;
        let lon = route[seg].0 + (route[last].0 - route[seg].0) * local;
        let lat = route[seg].1 + (route[last].1 - route[seg].1) * local;
        let pulse = project(lon, lat);
        painter.points(
            &format!("{}-pulse-{index}", spec.id),
            &[pulse],
            7.0,
            color,
            1.0,
        );
        let label = spec
            .routes
            .get(index)
            .and_then(|r| r.label.clone())
            .unwrap_or_else(|| format!("route {}", index + 1));
        painter.text(
            &format!("{}-route-label-{index}", spec.id),
            &label,
            points[0].0 + 6.0,
            points[0].1 - 12.0,
            9.0,
            color,
        );
    }
}

fn draw_atlas(
    painter: &mut Painter,
    spec: &AtlasSpec,
    time: f32,
    gap: f32,
    base_points: &[(f32, f32, usize)],
) {
    let r = layer_rect(&spec.rect, painter.width, painter.height, gap);
    panel(painter, &spec.id, &r, spec.title.as_deref());
    let pad = 20.0;
    let plot_top = r.y + 42.0;
    let plot_w = (r.w - pad * 2.0).max(1.0);
    let plot_h = (r.y + r.h - pad - plot_top).max(1.0);
    let rows = 32u32;
    let cols = 48u32;
    let mut density = vec![0f32; (rows * cols) as usize];
    let mut groups: Vec<Vec<(f32, f32)>> = vec![Vec::new(); 3];
    for (index, (x, y, cluster)) in base_points.iter().enumerate() {
        let wobble = (time * 0.8 + index as f32 * 0.23).sin() * 0.012;
        let px = r.x + pad + (x + wobble).clamp(0.0, 1.0) * plot_w;
        let py = r.y + pad + (y + wobble * 0.6).clamp(0.0, 1.0) * plot_h;
        let col = ((px - r.x - pad) / plot_w * cols as f32)
            .floor()
            .clamp(0.0, cols as f32 - 1.0) as usize;
        let row = ((py - r.y - pad) / plot_h * rows as f32)
            .floor()
            .clamp(0.0, rows as f32 - 1.0) as usize;
        density[row * cols as usize + col] += 1.0;
        let group_index = *cluster % groups.len();
        groups[group_index].push((px, py));
    }
    painter.heatmap(
        &format!("{}-density", spec.id),
        density,
        rows,
        cols,
        r.x + pad,
        r.y + pad,
        plot_w,
        plot_h,
        ATLAS_POINTS as f32 / 6.0,
        0.62,
    );
    for (index, points) in groups.iter().enumerate() {
        let color = spec
            .colors
            .get(index % spec.colors.len().max(1))
            .copied()
            .unwrap_or(ATLAS_COLORS[index % ATLAS_COLORS.len()]);
        painter.points(
            &format!("{}-points-{index}", spec.id),
            points,
            3.0,
            color,
            0.78,
        );
        let label = spec
            .labels
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("cluster {}", index + 1));
        let anchor = points
            .get(points.len() / 2)
            .copied()
            .unwrap_or((r.x + r.w * 0.5, r.y + r.h * 0.5));
        painter.text(
            &format!("{}-label-{index}", spec.id),
            &label,
            anchor.0 + 5.0,
            anchor.1 - 5.0,
            9.0,
            [224, 224, 224],
        );
    }
}

fn mercator(lat: f32) -> f32 {
    let lat = lat.clamp(-85.05112878, 85.05112878);
    (std::f32::consts::FRAC_PI_4 + (lat.to_radians()) / 2.0)
        .tan()
        .ln()
}

fn smooth(points: &[(f32, f32)], steps: usize) -> Vec<(f32, f32)> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let steps = steps.max(2);
    let mut out = Vec::with_capacity((points.len() - 1) * steps + 1);
    out.push(points[0]);
    for index in 0..points.len() - 1 {
        let p0 = points[index.saturating_sub(1)];
        let p1 = points[index];
        let p2 = points[index + 1];
        let p3 = points[(index + 2).min(points.len() - 1)];
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let t2 = t * t;
            let t3 = t2 * t;
            let catmull = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * (2.0 * b
                    + (-a + c) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                    + (-a + 3.0 * b - 3.0 * c + d) * t3)
            };
            out.push((
                catmull(p0.0, p1.0, p2.0, p3.0),
                catmull(p0.1, p1.1, p2.1, p3.1),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"{
        "schema": 1,
        "width": 800,
        "height": 600,
        "gap": 8,
        "layers": [
            {
                "kind": "chart",
                "id": "latency",
                "rect": { "x": 0, "y": 0, "width": 0.5, "height": 0.5 },
                "title": "Latency",
                "series": [
                    { "label": "p50", "source": "latency.p50" },
                    { "label": "p95", "source": "latency.p95" }
                ]
            },
            {
                "kind": "graph",
                "id": "topology",
                "rect": { "x": 0.5, "y": 0, "width": 0.5, "height": 0.5 },
                "labels": ["a", "b", "c"]
            }
        ]
    }"#;

    const SPEC_TYPES: &str = r#"{
        "schema": 1,
        "width": 800,
        "height": 600,
        "layers": [
            {
                "kind": "chart",
                "id": "types",
                "rect": { "x": 0, "y": 0, "width": 1, "height": 1 },
                "title": "Types",
                "series": [
                    { "label": "5xx", "kind": "bar", "source": "errors.5xx", "stride": 4 },
                    {
                        "label": "cost",
                        "kind": "candle",
                        "source": "cost.close",
                        "open_source": "cost.open",
                        "high_source": "cost.high",
                        "low_source": "cost.low",
                        "close_source": "cost.close",
                        "stride": 2
                    },
                    {
                        "label": "envelope",
                        "kind": "band",
                        "source": "latency.p95",
                        "band_source": "latency.p50",
                        "stride": 2
                    },
                    {
                        "label": "stack",
                        "kind": "stacked_area",
                        "source": "memory",
                        "stride": 2
                    }
                ]
            }
        ]
    }"#;

    #[test]
    fn bar_candle_and_band_series_build_valid_scene_nodes() {
        let spec: LiveSpec = serde_json::from_str(SPEC_TYPES).expect("spec");
        let mut live = LiveViz::new(spec);
        let scene = live.advance(42.0);
        scene.validate().expect("valid scene");
        let bars = scene
            .nodes
            .iter()
            .filter(|node| matches!(&node.content, Content::Rect(rect) if rect.rgb.is_some()))
            .count();
        let candles = scene
            .nodes
            .iter()
            .filter(|node| matches!(&node.content, Content::Candles(_)))
            .count();
        let bands = scene
            .nodes
            .iter()
            .filter(|node| matches!(&node.content, Content::Band(_)))
            .count();
        assert!(bars > 10, "expected many bar rects, got {bars}");
        assert_eq!(candles, 1, "expected one candle primitive");
        assert_eq!(bands, 2, "expected band and stacked-area primitives");
    }

    #[test]
    fn a_live_spec_builds_a_valid_scene() {
        let spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        let mut live = LiveViz::new(spec);
        let scene = live.advance(0.016);
        scene.validate().expect("valid scene");
        assert!(scene.nodes.len() > 20);
    }

    #[test]
    fn y_bounds_expand_to_include_data_and_land_on_nice_steps() {
        let (lo, hi) = nice_bounds(0.0, 760.0);
        assert!(lo <= 0.0);
        assert!(hi >= 760.0);
        let step = nice_step(hi - lo, 6);
        assert!((lo / step).fract().abs() < 0.0001);
        assert!((hi / step).fract().abs() < 0.0001);
        let (lo, hi) = nice_bounds(40.0, 48.0);
        assert_eq!((lo, hi), (40.0, 48.0));
    }

    #[test]
    fn legend_is_lua_visible_and_clickable() {
        let spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        let live = LiveViz::new(spec.clone());
        let chart = match &live.spec.layers[0] {
            LayerSpec::Chart(chart) => chart,
            _ => panic!("chart"),
        };
        let (width, height) = live.design_size();
        let rect = layer_rect(&chart.rect, width, height, live.spec.gap);
        let layout = chart_layout(chart, &rect);
        assert_eq!(layout.legend.len(), 2);
        let item = &layout.legend[1];
        assert_eq!(
            live.hit_test(item.x + 1.0, item.y + 1.0),
            Some(HitAction::ToggleSeries {
                layer: 0,
                series: 1
            })
        );
        let mut hidden = LiveViz::new(spec);
        hidden.toggle_series(0, 1);
        assert!(hidden.hidden[0][1]);
    }

    #[test]
    fn legend_can_be_disabled_from_the_spec() {
        let mut spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        if let LayerSpec::Chart(chart) = &mut spec.layers[0] {
            chart.legend = false;
        }
        let live = LiveViz::new(spec);
        let chart = match &live.spec.layers[0] {
            LayerSpec::Chart(chart) => chart,
            _ => panic!("chart"),
        };
        let (width, height) = live.design_size();
        let rect = layer_rect(&chart.rect, width, height, live.spec.gap);
        let layout = chart_layout(chart, &rect);
        assert!(layout.legend.is_empty());
    }

    #[test]
    fn history_is_append_only_during_playback() {
        let spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        let mut live = LiveViz::new(spec);
        live.seek(100.0);
        let before: Vec<(f32, f32)> = live
            .history
            .iter()
            .map(|sample| (sample.time, sample.metrics.p50))
            .collect();
        assert_eq!(before.len(), CHART_POINTS);
        live.advance(SAMPLE_SECONDS);
        let after: Vec<(f32, f32)> = live
            .history
            .iter()
            .map(|sample| (sample.time, sample.metrics.p50))
            .collect();
        assert_eq!(after.len(), CHART_POINTS);
        assert_eq!(&before[1..], &after[..CHART_POINTS - 1]);
        assert!(
            (after[CHART_POINTS - 1].0 - (before[CHART_POINTS - 1].0 + SAMPLE_SECONDS)).abs()
                < 0.0001
        );
    }

    #[test]
    fn between_samples_only_the_viewport_moves() {
        let spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        let mut live = LiveViz::new(spec);
        let before = live.seek(100.0);
        let before_line = before
            .nodes
            .iter()
            .find_map(|node| match &node.content {
                Content::Polyline(line) => Some(line.points.clone()),
                _ => None,
            })
            .expect("line");
        live.advance(0.1);
        let after = live.scene();
        let after_line = after
            .nodes
            .iter()
            .find_map(|node| match &node.content {
                Content::Polyline(line) => Some(line.points.clone()),
                _ => None,
            })
            .expect("line");
        assert_eq!(before_line.len(), after_line.len());
        for (before, after) in before_line.iter().zip(&after_line) {
            assert!(
                (before[1] - after[1]).abs() < 0.0001,
                "y values are immutable"
            );
            assert!(
                (before[0] - after[0]).abs() > 0.000001,
                "x positions follow the viewport"
            );
        }
    }

    #[test]
    fn seeking_rebuilds_the_same_window() {
        let spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        let mut a = LiveViz::new(spec.clone());
        let mut b = LiveViz::new(spec);
        a.seek(140.0);
        b.seek(140.0);
        assert_eq!(a.history, b.history);
    }

    #[test]
    fn delta_time_changes_the_scene() {
        let spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        let mut live = LiveViz::new(spec);
        let first = live.advance(0.0);
        let later = live.advance(1.0);
        let first_polyline = first.nodes.iter().find_map(|node| match &node.content {
            Content::Polyline(line) => Some(line.points.clone()),
            _ => None,
        });
        let later_polyline = later.nodes.iter().find_map(|node| match &node.content {
            Content::Polyline(line) => Some(line.points.clone()),
            _ => None,
        });
        assert!(first_polyline.is_some());
        assert_ne!(first_polyline, later_polyline);
    }
}
