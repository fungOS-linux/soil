//! Resident live visualization for the native Soil host.
//!
//! Lua owns the UI: it writes a small `LiveSpec` describing layers, rects,
//! titles, palettes and styles. Rust owns the data streams and the frame loop:
//! it samples resident data, advances by real delta time, and builds a
//! `soil_scene` every frame. There is no per-frame Lua call and no FFI stream
//! bridge.

use std::time::Instant;

use serde::Deserialize;
use soil_scene::{Area, Content, Heatmap, Node, Points, Polyline, Rect, Scene, Text, Transform};

use crate::{Frame, Theme};

const TAU: f32 = std::f32::consts::PI * 2.0;
const CHART_POINTS: usize = 160;
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
    #[serde(default)]
    pub layers: Vec<LayerSpec>,
}

fn one() -> u32 {
    1
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
    #[serde(default)]
    pub style: ChartStyle,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeriesSpec {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub kind: SeriesKind,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub width: Option<f32>,
    #[serde(default)]
    pub fill_opacity: Option<f32>,
    #[serde(default)]
    pub smooth: bool,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SeriesKind {
    #[default]
    Line,
    Area,
    Scatter,
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

pub struct LiveViz {
    spec: LiveSpec,
    theme: Theme,
    time: f32,
    frame: u64,
    last_update: Instant,
    atlas_points: Vec<(f32, f32, usize)>,
    hidden: Vec<Vec<bool>>,
    scale: f32,
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
                LayerSpec::Chart(chart) => vec![false; chart.series.len()],
                _ => Vec::new(),
            })
            .collect();
        Self {
            spec,
            theme: Theme::default(),
            time: 0.0,
            frame: 0,
            last_update: Instant::now(),
            atlas_points,
            hidden,
            scale: 1.0,
            pointer: None,
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

    /// Pointer position in spec/logical pixels.
    pub fn set_pointer(&mut self, pointer: Option<(f32, f32)>) {
        self.pointer = pointer;
    }

    /// Advance by an explicit delta, for a host that already owns the clock.
    pub fn advance(&mut self, dt: f32) -> Scene {
        self.time += dt.clamp(0.0, 0.25);
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
        let mut painter = Painter::new(
            &mut nodes,
            self.spec.width,
            self.spec.height,
            self.theme,
            self.scale,
        );
        for (layer_index, layer) in self.spec.layers.iter().enumerate() {
            match layer {
                LayerSpec::Chart(spec) => {
                    let hidden = self
                        .hidden
                        .get(layer_index)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    draw_chart(&mut painter, spec, self.time, gap, hidden, self.pointer);
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
        self.time = 0.0;
        self.frame = 0;
        for layer in &mut self.hidden {
            layer.fill(false);
        }
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
        for (layer_index, layer) in self.spec.layers.iter().enumerate() {
            let LayerSpec::Chart(spec) = layer else {
                continue;
            };
            let r = layer_rect(&spec.rect, self.spec.width, self.spec.height, gap);
            let left = r.x + 58.0;
            let top = r.y + 54.0;
            let plot_h = (r.h - 92.0).max(16.0);
            let legend_y = top + plot_h + 18.0;
            if y < legend_y || y > legend_y + 20.0 || x < left {
                continue;
            }
            for index in 0..spec.series.len() {
                let legend_x = left + index as f32 * 112.0;
                if x >= legend_x && x <= legend_x + 100.0 {
                    return Some(HitAction::ToggleSeries {
                        layer: layer_index,
                        series: index,
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
                radius,
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

fn source_params(source: Option<&str>, index: usize) -> (f32, f32, f32) {
    match source.unwrap_or("") {
        "latency.p50" => (46.0, 11.0, 0.0),
        "latency.p95" => (60.0, 16.0, 1.4),
        "latency.p99" => (74.0, 22.0, 2.8),
        _ => (
            46.0 + index as f32 * 14.0,
            11.0 + index as f32 * 5.0,
            index as f32 * 1.4,
        ),
    }
}

fn chart_value(base: f32, amplitude: f32, phase: f32, point: usize, time: f32) -> f32 {
    let x = point as f32;
    let wave =
        (x * 0.055 + time * 1.35 + phase).sin() + (x * 0.19 + time * 2.1 + phase).sin() * 0.14;
    base + wave * amplitude
}

fn draw_chart(
    painter: &mut Painter,
    spec: &ChartSpec,
    time: f32,
    gap: f32,
    hidden: &[bool],
    pointer: Option<(f32, f32)>,
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
    let left = r.x + 58.0;
    let top = r.y + 54.0;
    let plot_w = (r.w - 78.0).max(16.0);
    let plot_h = (r.h - 92.0).max(16.0);
    let plot_bottom = top + plot_h;
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
    let xhi = CHART_POINTS as f32;
    let ylo = 20.0;
    let yhi = 120.0;
    let sx = |x: f32| left + (x - xlo) / (xhi - xlo) * plot_w;
    let sy = |y: f32| plot_bottom - (y - ylo) / (yhi - ylo) * plot_h;
    for tick in 0..=5 {
        let value = ylo + (yhi - ylo) * tick as f32 / 5.0;
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
        painter.text(
            &format!("{}-ylabel-{tick}", spec.id),
            &format!("{value:.0}"),
            left - 32.0,
            y - 5.0,
            style.label_size,
            [145, 155, 170],
        );
    }
    for tick in 0..=4 {
        let value = xhi * tick as f32 / 4.0;
        let x = sx(value);
        painter.text(
            &format!("{}-xlabel-{tick}", spec.id),
            &format!("{value:.0}"),
            x - 8.0,
            plot_bottom + 8.0,
            style.label_size,
            [145, 155, 170],
        );
    }

    for (index, series) in spec.series.iter().enumerate() {
        if hidden.get(index).copied().unwrap_or(false) {
            continue;
        }
        let (base, amplitude, phase) = source_params(series.source.as_deref(), index);
        let color = series
            .color
            .unwrap_or(CHART_COLORS[index % CHART_COLORS.len()]);
        let mut points = Vec::with_capacity(CHART_POINTS);
        for point in 0..CHART_POINTS {
            let y = chart_value(base, amplitude, phase, point, time);
            points.push((sx(point as f32), sy(y)));
        }
        if series.smooth {
            points = smooth(&points, style.smooth_steps);
        }
        let paint = series.width.unwrap_or(style.line_width);
        if series.kind == SeriesKind::Area {
            painter.area(
                &format!("{}-{}-fill", spec.id, index),
                &points,
                sy(ylo),
                color,
                series.fill_opacity.unwrap_or(style.area_opacity),
            );
        }
        if style.glow > 0.0 {
            painter.polyline(
                &format!("{}-{}-glow-outer", spec.id, index),
                &points,
                paint + style.glow_width * 1.7,
                color,
                style.glow * 0.35,
            );
            painter.polyline(
                &format!("{}-{}-glow-inner", spec.id, index),
                &points,
                paint + style.glow_width * 0.8,
                color,
                style.glow * 0.72,
            );
        }
        if series.kind == SeriesKind::Scatter {
            painter.points(
                &format!("{}-{}-points", spec.id, index),
                &points,
                3.0,
                color,
                0.95,
            );
        } else {
            painter.polyline(
                &format!("{}-{}", spec.id, index),
                &points,
                paint,
                color,
                1.0,
            );
        }
        let label = series
            .label
            .clone()
            .unwrap_or_else(|| format!("series {}", index + 1));
        let legend_x = left + index as f32 * 112.0;
        painter.polyline(
            &format!("{}-legend-line-{index}", spec.id),
            &[
                (legend_x, plot_bottom + 24.0),
                (legend_x + 12.0, plot_bottom + 24.0),
            ],
            3.0,
            color,
            1.0,
        );
        painter.text(
            &format!("{}-legend-{index}", spec.id),
            &label,
            legend_x + 18.0,
            plot_bottom + 18.0,
            10.0,
            [180, 190, 205],
        );
    }

    if let Some((px, py)) = pointer
        && px >= left
        && px <= left + plot_w
        && py >= top
        && py <= plot_bottom
    {
        let data_x = xlo + (px - left) / plot_w * (xhi - xlo);
        let nearest = (((data_x - xlo) / (xhi - xlo)) * (CHART_POINTS - 1) as f32)
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
            let (base, amplitude, phase) = source_params(series.source.as_deref(), index);
            let value = chart_value(base, amplitude, phase, nearest, time);
            let color = series
                .color
                .unwrap_or(CHART_COLORS[index % CHART_COLORS.len()]);
            let y = sy(value);
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
            painter.text(
                &format!("{}-tooltip-title", spec.id),
                &format!("x {:.0}", nearest as f32),
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

    #[test]
    fn a_live_spec_builds_a_valid_scene() {
        let spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        let mut live = LiveViz::new(spec);
        let scene = live.advance(0.016);
        scene.validate().expect("valid scene");
        assert!(scene.nodes.len() > 20);
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
