//! Resident live visualization for the native Soil host.
//!
//! Lua owns the UI: it writes a small `LiveSpec` describing layers, rects,
//! titles, palettes and styles. Rust owns the data streams and the frame loop:
//! it samples resident data, advances by real delta time, and builds a
//! `soil_scene` every frame. There is no per-frame Lua call and no FFI stream
//! bridge.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use soil_scene::{
    Area, Band, Candle, Candles, Content, DenseResource, GpuDense, GpuDenseMode, GpuPoints3D,
    GpuResource, Heatmap, Node, PointCloudResource, Points, Polyline, Rect, Scene, Text, Transform,
};
use sysinfo::{ProcessesToUpdate, System};

use crate::gpu::{camera_view_proj, project_to};
use crate::{Frame, Theme};

const TAU: f32 = std::f32::consts::PI * 2.0;
const CHART_POINTS: usize = 160;
const SAMPLE_SECONDS: f32 = 0.2;
const ATLAS_POINTS: usize = 240;
const INCIDENT_CYCLE: f32 = 240.0;
const INCIDENT_OFFSET: f32 = 32.0;

/// A new LiveViz invalidates resident GPU buffers even when the Lua spec keeps
/// the same resource id across a hot reload.
static NEXT_GPU_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_gpu_generation() -> u64 {
    NEXT_GPU_GENERATION.fetch_add(1, Ordering::Relaxed)
}

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
    /// Optional automatic panel grid. When present, layer `rect` values are
    /// ignored and panels are laid out in declaration order.
    #[serde(default)]
    pub grid: Option<GridSpec>,
    /// Optional host palette. The native host starts from `Theme::default()`.
    #[serde(default)]
    pub theme: Option<ThemeSpec>,
    #[serde(default)]
    pub layers: Vec<LayerSpec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ThemeSpec {
    #[serde(default)]
    pub background: Option<[u8; 3]>,
    #[serde(default)]
    pub surface: Option<[u8; 3]>,
    #[serde(default)]
    pub text: Option<[u8; 3]>,
    #[serde(default)]
    pub muted: Option<[u8; 3]>,
    #[serde(default)]
    pub primary: Option<[u8; 3]>,
    #[serde(default)]
    pub success: Option<[u8; 3]>,
    #[serde(default)]
    pub warning: Option<[u8; 3]>,
    #[serde(default)]
    pub danger: Option<[u8; 3]>,
    #[serde(default)]
    pub info: Option<[u8; 3]>,
    #[serde(default)]
    pub body: Option<f32>,
}

impl ThemeSpec {
    fn apply(&self, theme: &mut Theme) {
        if let Some(value) = self.background {
            theme.background = value;
        }
        if let Some(value) = self.surface {
            theme.surface = value;
        }
        if let Some(value) = self.text {
            theme.text = value;
        }
        if let Some(value) = self.muted {
            theme.muted = value;
        }
        if let Some(value) = self.primary {
            theme.primary = value;
        }
        if let Some(value) = self.success {
            theme.success = value;
        }
        if let Some(value) = self.warning {
            theme.warning = value;
        }
        if let Some(value) = self.danger {
            theme.danger = value;
        }
        if let Some(value) = self.info {
            theme.info = value;
        }
        if let Some(value) = self.body {
            theme.body = value.max(1.0);
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GridSpec {
    #[serde(default)]
    pub columns: Option<usize>,
    #[serde(default)]
    pub rows: Option<usize>,
    #[serde(default)]
    pub gap: Option<f32>,
    #[serde(default)]
    pub padding: Option<f32>,
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
    Histogram(HistogramSpec),
    Dense(DenseSpec),
    PointCloud3D(PointCloud3DSpec),
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
    pub annotations: Vec<AnnotationSpec>,
    /// Draw the interactive legend. Lua can hide it per chart.
    #[serde(default = "yes")]
    pub legend: bool,
    #[serde(default)]
    pub style: ChartStyle,
    #[serde(default)]
    pub y_min: Option<f32>,
    #[serde(default)]
    pub y_max: Option<f32>,
    /// Seconds of exponential smoothing for Y bounds. Zero snaps.
    #[serde(default)]
    pub y_lerp: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AnnotationSpec {
    Line(LineAnnotation),
    Band(BandAnnotation),
    Threshold(ThresholdAnnotation),
    #[serde(rename = "x_event")]
    XEvent(XEventAnnotation),
}

#[derive(Debug, Clone, Deserialize)]
pub struct LineAnnotation {
    /// Seconds into the incident cycle.
    pub at: f32,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default = "annotation_opacity")]
    pub opacity: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct XEventAnnotation {
    /// Seconds into the incident cycle.
    pub at: f32,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default = "annotation_opacity")]
    pub opacity: f32,
    /// Draw a faint vertical guide in addition to the axis marker.
    #[serde(default)]
    pub line: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BandAnnotation {
    /// Start and end, in seconds into the incident cycle.
    pub from: f32,
    pub to: f32,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default = "band_opacity")]
    pub opacity: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThresholdAnnotation {
    /// Data-space y value.
    pub value: f32,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default = "annotation_opacity")]
    pub opacity: f32,
}

fn annotation_opacity() -> f32 {
    0.7
}

fn band_opacity() -> f32 {
    0.08
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
    pub ink: Option<soil_scene::Ink>,
    #[serde(default)]
    pub color2: Option<[u8; 3]>,
    #[serde(default)]
    pub width: Option<f32>,
    #[serde(default)]
    pub fill_opacity: Option<f32>,
    #[serde(default)]
    pub smooth: bool,
    /// Smooth the newest line/area/band point across the sample interval.
    #[serde(default)]
    pub interpolate: bool,
    /// Draw line/area/band as step-after.
    #[serde(default)]
    pub step: bool,
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
    #[serde(rename = "stacked_bar")]
    StackedBar,
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
pub struct HistogramSpec {
    pub id: String,
    #[serde(default)]
    pub rect: RectSpec,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    pub source: String,
    #[serde(default = "default_bins")]
    pub bins: usize,
    #[serde(default)]
    pub min: Option<f32>,
    #[serde(default)]
    pub max: Option<f32>,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub ink: Option<soil_scene::Ink>,
    #[serde(default = "histogram_opacity")]
    pub opacity: f32,
    #[serde(default = "histogram_radius")]
    pub bar_radius: f32,
}

fn default_bins() -> usize {
    24
}

fn histogram_opacity() -> f32 {
    0.82
}

fn histogram_radius() -> f32 {
    2.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct DenseSpec {
    pub id: String,
    #[serde(default)]
    pub rect: RectSpec,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default = "default_dense_points")]
    pub points: usize,
    #[serde(default)]
    pub seed: u64,
    #[serde(default = "default_dense_threshold")]
    pub threshold: usize,
    #[serde(default)]
    pub mode: DenseKind,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DenseKind {
    #[default]
    Line,
    Scatter,
}

fn default_dense_points() -> usize {
    1_000_000
}

fn default_dense_threshold() -> usize {
    4_000
}

#[derive(Debug, Clone, Deserialize)]
pub struct PointCloud3DSpec {
    pub id: String,
    #[serde(default)]
    pub rect: RectSpec,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default = "default_cloud_points")]
    pub points: usize,
    #[serde(default)]
    pub seed: u64,
    #[serde(default = "default_pitch")]
    pub pitch: f32,
    #[serde(default = "default_point_size")]
    pub size: f32,
}

fn default_cloud_points() -> usize {
    20_000
}

fn default_pitch() -> f32 {
    0.35
}

fn default_point_size() -> f32 {
    2.2
}

#[derive(Debug, Clone)]
struct DenseData {
    resource: Arc<GpuResource>,
}

impl DenseData {
    fn new(spec: &DenseSpec) -> Self {
        let count = spec.points.clamp(2, 5_000_000);
        let seed = spec.seed as f32 * 0.017 + 1.0;
        let mut raw = Vec::with_capacity(count);
        for index in 0..count {
            let t = index as f32 / (count - 1) as f32;
            let y = 0.5
                + 0.23 * (t * TAU * 3.1 + seed).sin()
                + 0.14 * (t * TAU * 17.3 + seed * 1.7).sin()
                + 0.06 * (t * TAU * 91.7 + seed * 2.3).sin()
                + 0.03 * (t * TAU * 311.0 + seed * 4.1).sin();
            raw.push([t, y]);
        }
        let resource = Arc::new(GpuResource::Dense(DenseResource {
            id: spec.id.clone(),
            generation: next_gpu_generation(),
            points: raw,
        }));
        Self { resource }
    }

    fn raw(&self) -> &[[f32; 2]] {
        match self.resource.as_ref() {
            GpuResource::Dense(resource) => &resource.points,
            GpuResource::PointCloud(_) => &[],
        }
    }

    fn raw_len(&self) -> usize {
        self.raw().len()
    }
}

#[derive(Debug, Clone)]
struct CloudData {
    resource: Arc<GpuResource>,
}

impl CloudData {
    fn new(spec: &PointCloud3DSpec) -> Self {
        let count = spec.points.clamp(64, 500_000);
        let mut points = Vec::with_capacity(count);
        for index in 0..count {
            let i = index as f32;
            let u = hash01(i * 12.9898 + spec.seed as f32 * 0.17);
            let v = hash01(i * 78.233 + spec.seed as f32 * 0.31);
            let theta = u * TAU;
            let phi = (v * 2.0 - 1.0).acos();
            let radius = 0.72 + 0.28 * hash01(i * 37.719 + spec.seed as f32);
            let x = radius * phi.sin() * theta.cos();
            let y = radius * phi.cos() * 0.7;
            let z = radius * phi.sin() * theta.sin();
            points.push([x, y, z, hash01(i * 5.13 + spec.seed as f32 * 0.73)]);
        }
        let resource = Arc::new(GpuResource::PointCloud(PointCloudResource {
            id: spec.id.clone(),
            generation: next_gpu_generation(),
            points,
        }));
        Self { resource }
    }

    fn point_count(&self) -> usize {
        match self.resource.as_ref() {
            GpuResource::PointCloud(resource) => resource.points.len(),
            GpuResource::Dense(resource) => resource.points.len(),
        }
    }
}

fn hash01(mut value: f32) -> f32 {
    value = (value * 0.1031).fract();
    value *= value + 33.33;
    value = (value * (value + value)).fract();
    value
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

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct SystemSample {
    cpu: f32,
    memory: f32,
    load1: f32,
    load5: f32,
    load15: f32,
    processes: f32,
}

struct SystemSource {
    system: Option<System>,
    sample: SystemSample,
    last_process_refresh: Instant,
}

impl SystemSource {
    fn new() -> Self {
        let mut source = Self {
            system: System::new().ok(),
            sample: SystemSample::default(),
            last_process_refresh: Instant::now(),
        };
        if let Some(system) = source.system.as_mut() {
            system.refresh_cpu_usage();
            system.refresh_memory();
            system.refresh_processes(ProcessesToUpdate::All, true);
            source.last_process_refresh = Instant::now();
        }
        source.sample = source.read();
        source
    }

    fn read(&self) -> SystemSample {
        let Some(system) = &self.system else {
            return SystemSample::default();
        };
        let load = System::load_average().unwrap_or(sysinfo::LoadAvg {
            one: 0.0,
            five: 0.0,
            fifteen: 0.0,
        });
        let total = system.total_memory();
        let memory = if total > 0 {
            system.used_memory() as f32 / total as f32 * 100.0
        } else {
            0.0
        };
        SystemSample {
            cpu: system.global_cpu_usage().clamp(0.0, 100.0),
            memory: memory.clamp(0.0, 100.0),
            load1: load.one as f32,
            load5: load.five as f32,
            load15: load.fifteen as f32,
            processes: system.processes().len() as f32,
        }
    }

    fn snapshot(&self) -> SystemSample {
        self.sample
    }

    fn refresh(&mut self) -> SystemSample {
        if let Some(system) = self.system.as_mut() {
            system.refresh_cpu_usage();
            system.refresh_memory();
            if self.last_process_refresh.elapsed() >= Duration::from_secs(2) {
                system.refresh_processes(ProcessesToUpdate::All, true);
                self.last_process_refresh = Instant::now();
            }
        }
        self.sample = self.read();
        self.sample
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    time: f32,
    metrics: Metrics,
    system: SystemSample,
}

pub struct LiveViz {
    spec: LiveSpec,
    theme: Theme,
    time: f32,
    frame: u64,
    last_update: Instant,
    history: VecDeque<Sample>,
    last_index: i64,
    system: SystemSource,
    dense: Vec<Option<DenseData>>,
    clouds: Vec<Option<CloudData>>,
    y_bounds: Vec<Option<(f32, f32)>>,
    last_scene_time: f32,
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
        let mut theme = Theme::default();
        if let Some(theme_spec) = &spec.theme {
            theme_spec.apply(&mut theme);
        }
        let system = SystemSource::new();
        let dense = spec
            .layers
            .iter()
            .map(|layer| match layer {
                LayerSpec::Dense(spec) => Some(DenseData::new(spec)),
                _ => None,
            })
            .collect();
        let clouds = spec
            .layers
            .iter()
            .map(|layer| match layer {
                LayerSpec::PointCloud3D(spec) => Some(CloudData::new(spec)),
                _ => None,
            })
            .collect();
        let layer_count = spec.layers.len();
        let mut live = Self {
            spec,
            theme,
            time: 0.0,
            frame: 0,
            last_update: Instant::now(),
            history: VecDeque::with_capacity(CHART_POINTS),
            last_index: 0,
            system,
            dense,
            clouds,
            y_bounds: vec![None; layer_count],
            last_scene_time: 0.0,
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

    fn sample(index: i64, system: SystemSample) -> Sample {
        let raw = index as f32 * SAMPLE_SECONDS;
        Sample {
            time: raw,
            metrics: metrics(raw),
            system,
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
        self.y_bounds.fill(None);
        let first = self.last_index - CHART_POINTS as i64 + 1;
        let system = self.system.snapshot();
        for index in first..=self.last_index {
            self.history.push_back(Self::sample(index, system));
        }
    }

    fn append_until(&mut self, time: f32) {
        let target = Self::sample_index(time);
        if self.last_index >= target {
            return;
        }
        let system = self.system.refresh();
        while self.last_index < target {
            self.last_index += 1;
            self.history
                .push_back(Self::sample(self.last_index, system));
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

    fn linked_cursor(&self, width: f32, height: f32, gap: f32) -> Option<LinkedCursor> {
        let (px, py) = self.pointer?;
        let phase = viewport_phase(&self.history, self.time);
        for (index, layer) in self.spec.layers.iter().enumerate() {
            let LayerSpec::Chart(spec) = layer else {
                continue;
            };
            let r = self.rect_for(index, &spec.rect, width, height, gap);
            let layout = chart_layout(spec, &r);
            if px < layout.left
                || px > layout.left + layout.plot_w
                || py < layout.top
                || py > layout.plot_bottom
            {
                continue;
            }
            let xhi = (CHART_POINTS - 1) as f32;
            let data_x = (px - layout.left) / layout.plot_w * xhi;
            let sample = (data_x + phase)
                .round()
                .clamp(0.0, (CHART_POINTS - 1) as f32) as usize;
            return Some(LinkedCursor {
                layer: index,
                sample,
                tooltip: Some((px, py)),
            });
        }
        None
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

    pub fn scene(&mut self) -> Scene {
        let dt = (self.time - self.last_scene_time).clamp(0.0, 0.25);
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
        let cursor = self.linked_cursor(width, height, gap);
        for (layer_index, layer) in self.spec.layers.iter().enumerate() {
            let rect = match layer {
                LayerSpec::Chart(spec) => &spec.rect,
                LayerSpec::Graph(spec) => &spec.rect,
                LayerSpec::Map(spec) => &spec.rect,
                LayerSpec::Atlas(spec) => &spec.rect,
                LayerSpec::Histogram(spec) => &spec.rect,
                LayerSpec::Dense(spec) => &spec.rect,
                LayerSpec::PointCloud3D(spec) => &spec.rect,
            };
            let r = self.rect_for(layer_index, rect, width, height, gap);
            match layer {
                LayerSpec::Chart(spec) => {
                    let hidden = self
                        .hidden
                        .get(layer_index)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    let layer_cursor = cursor.map(|cursor| LinkedCursor {
                        tooltip: if cursor.layer == layer_index {
                            cursor.tooltip
                        } else {
                            None
                        },
                        ..cursor
                    });
                    let target = chart_bounds(spec, hidden, &self.history);
                    let bounds = if let Some(tau) = spec.y_lerp.filter(|tau| *tau > 0.0) {
                        if let Some(previous) = self.y_bounds.get(layer_index).copied().flatten() {
                            let alpha = 1.0 - (-dt / tau.max(0.01)).exp();
                            (
                                previous.0 + (target.0 - previous.0) * alpha,
                                previous.1 + (target.1 - previous.1) * alpha,
                            )
                        } else {
                            target
                        }
                    } else {
                        target
                    };
                    if let Some(slot) = self.y_bounds.get_mut(layer_index) {
                        *slot = Some(bounds);
                    }
                    draw_chart(
                        &mut painter,
                        spec,
                        self.time,
                        bounds,
                        r,
                        hidden,
                        layer_cursor,
                        &self.history,
                    );
                }
                LayerSpec::Graph(spec) => draw_graph(&mut painter, spec, self.time, r),
                LayerSpec::Map(spec) => draw_map(&mut painter, spec, self.time, r),
                LayerSpec::Atlas(spec) => {
                    draw_atlas(&mut painter, spec, self.time, r, &self.atlas_points)
                }
                LayerSpec::Histogram(spec) => draw_histogram(&mut painter, spec, r, &self.history),
                LayerSpec::Dense(spec) => {
                    if let Some(data) = self.dense.get(layer_index).and_then(Option::as_ref) {
                        draw_dense_chart(&mut painter, spec, data, r);
                    }
                }
                LayerSpec::PointCloud3D(spec) => {
                    if let Some(cloud) = self.clouds.get(layer_index).and_then(Option::as_ref) {
                        draw_pointcloud3d(&mut painter, spec, cloud, self.time, r);
                    }
                }
            }
        }
        self.last_scene_time = self.time;
        let gpu = self
            .dense
            .iter()
            .flatten()
            .map(|data| data.resource.clone())
            .chain(
                self.clouds
                    .iter()
                    .flatten()
                    .map(|data| data.resource.clone()),
            )
            .collect();
        Scene::with_gpu(nodes, gpu)
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

    fn rect_for(
        &self,
        index: usize,
        rect: &RectSpec,
        width: f32,
        height: f32,
        gap: f32,
    ) -> LayerRect {
        let Some(grid) = &self.spec.grid else {
            return layer_rect(rect, width, height, gap);
        };
        let count = self.spec.layers.len().max(1);
        let columns = grid
            .columns
            .unwrap_or(if count > 4 { 3 } else { 2 })
            .clamp(1, count);
        let rows = grid.rows.unwrap_or(count.div_ceil(columns)).max(1);
        let cell_gap = grid.gap.unwrap_or(gap).max(0.0);
        let padding = grid.padding.unwrap_or(0.0).max(0.0);
        let cell_w = (width - padding * 2.0 - cell_gap * (columns.saturating_sub(1)) as f32)
            / columns as f32;
        let cell_h =
            (height - padding * 2.0 - cell_gap * (rows.saturating_sub(1)) as f32) / rows as f32;
        let column = index % columns;
        let row = index / columns;
        LayerRect {
            x: padding + column as f32 * (cell_w + cell_gap),
            y: padding + row as f32 * (cell_h + cell_gap),
            w: cell_w.max(1.0),
            h: cell_h.max(1.0),
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
            let r = self.rect_for(layer_index, &spec.rect, width, height, gap);
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

    pub fn render(&mut self, width: u32, height: u32) -> anyhow::Result<Frame> {
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

    #[allow(clippy::too_many_arguments)]
    fn gpu_dense(
        &mut self,
        id: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        resource: &str,
        mode: GpuDenseMode,
        range: [f32; 4],
        buckets: u32,
        color: [u8; 3],
        opacity: f32,
        size: f32,
    ) {
        self.z += 0.03;
        self.nodes.push(Node {
            id: format!("{id}-gpu"),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [self.nx(x), self.ny(y), self.z],
                rotation: [0.0; 3],
                scale: [self.sw(w.max(1.0)), self.sh(h.max(1.0)), 1.0],
            },
            content: Content::GpuDense(GpuDense {
                resource: resource.into(),
                mode,
                range,
                buckets,
                ink: None,
                rgb: Some(color),
                opacity,
                size,
            }),
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn gpu_points3d(
        &mut self,
        id: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        resource: &str,
        yaw: f32,
        pitch: f32,
        zoom: f32,
        size: f32,
        color: [u8; 3],
        opacity: f32,
    ) {
        self.z += 0.03;
        self.nodes.push(Node {
            id: format!("{id}-gpu"),
            parent: None,
            transition_ms: Some(0),
            transform: Transform {
                translation: [self.nx(x), self.ny(y), self.z],
                rotation: [0.0; 3],
                scale: [self.sw(w.max(1.0)), self.sh(h.max(1.0)), 1.0],
            },
            content: Content::GpuPoints3D(GpuPoints3D {
                resource: resource.into(),
                yaw,
                pitch,
                zoom,
                size,
                ink: None,
                rgb: Some(color),
                opacity,
            }),
        });
    }

    #[allow(clippy::too_many_arguments)]
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

    #[allow(clippy::too_many_arguments)]
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

fn theme_palette(theme: &Theme) -> [[u8; 3]; 6] {
    [
        theme.info,
        theme.primary,
        theme.success,
        theme.warning,
        theme.danger,
        theme.muted,
    ]
}

fn ink_color(theme: &Theme, ink: soil_scene::Ink) -> [u8; 3] {
    match ink {
        soil_scene::Ink::Text => theme.text,
        soil_scene::Ink::Muted => theme.muted,
        soil_scene::Ink::Surface => theme.surface,
        soil_scene::Ink::Background => theme.background,
        soil_scene::Ink::Primary => theme.primary,
        soil_scene::Ink::Success => theme.success,
        soil_scene::Ink::Warning => theme.warning,
        soil_scene::Ink::Danger => theme.danger,
        soil_scene::Ink::Info => theme.info,
    }
}
const GRAPH_COLORS: [[u8; 3]; 3] = [[0, 229, 255], [0, 245, 160], [255, 107, 0]];
const MAP_COLORS: [[u8; 3]; 3] = [[0, 229, 255], [0, 245, 160], [255, 211, 0]];
const ATLAS_COLORS: [[u8; 3]; 3] = [[0, 229, 255], [255, 45, 149], [176, 38, 255]];

fn panel(painter: &mut Painter, id: &str, r: &LayerRect, title: Option<&str>) {
    let theme = painter.theme;
    painter.rect(
        &format!("{id}-panel"),
        r.x,
        r.y,
        r.w,
        r.h,
        theme.surface,
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
            theme.text,
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
    let value = ((elapsed * 12.9898 + seed * 78.233).sin() * 43_758.547).fract();
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
    let cost = cost_ohlc(elapsed);
    // The demo narrative loops instead of settling into a flat recovery. The
    // offset lets the pre-roll show baseline before the leak begins.
    let elapsed = (elapsed + INCIDENT_OFFSET).rem_euclid(INCIDENT_CYCLE);
    let memory = if elapsed < 60.0 {
        45.0 + noise(3.0, elapsed, 1.0) + sine_wave(elapsed, 30.0, 0.9)
    } else if elapsed < 120.0 {
        45.0 + (elapsed - 60.0) * 0.5 + noise(3.0, elapsed, 2.0) + sine_wave(elapsed, 30.0, 0.9)
    } else if elapsed < 155.0 {
        75.0 + (elapsed - 120.0) * 0.8 + noise(3.0, elapsed, 3.0) + sine_wave(elapsed, 30.0, 0.9)
    } else if elapsed < 175.0 {
        95.0 + noise(1.0, elapsed, 4.0)
    } else {
        50.0 + (95.0 - 50.0) * (-0.09 * (elapsed - 175.0)).exp() + noise(2.0, elapsed, 5.0)
    };
    let memory = clamp(memory, 0.0, 100.0);
    let gc_pressure = (memory - 60.0).max(0.0) * 1.2;
    let cpu = clamp(25.0 + gc_pressure + noise(3.0, elapsed, 6.0), 0.0, 100.0);
    let gc_jitter = (memory - 55.0).max(0.0) * 2.0;
    let p50 = (45.0 + gc_jitter + noise(5.0, elapsed, 7.0)).max(0.0);
    let p95_multiplier = 1.8 + (memory - 70.0).max(0.0) * 0.03;
    let p95 = p50 * p95_multiplier + noise(10.0, elapsed, 8.0);
    let p99_multiplier = 1.5 + (memory - 75.0).max(0.0) * 0.03;
    let p99 = (p95 * p99_multiplier + noise(20.0, elapsed, 9.0)).min(950.0);
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
    let (cost_open, cost_high, cost_low, cost_close) = cost;
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

fn source_value(source: &str, metrics: Metrics, system: SystemSample) -> Option<f32> {
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
        "system.cpu" => system.cpu,
        "system.memory" => system.memory,
        "system.load1" => system.load1,
        "system.load5" => system.load5,
        "system.load15" => system.load15,
        "system.processes" => system.processes,
        _ => return None,
    })
}

/// Each chart reads the same append-only resident history, oldest at x=0.
fn chart_value(samples: &VecDeque<Sample>, source: Option<&str>, point: usize) -> f32 {
    let source = source.unwrap_or("latency.p50");
    samples
        .get(point)
        .and_then(|sample| source_value(source, sample.metrics, sample.system))
        .unwrap_or(0.0)
}

fn interpolated_value(
    samples: &VecDeque<Sample>,
    source: Option<&str>,
    point: usize,
    phase: f32,
    interpolate: bool,
) -> f32 {
    let current = chart_value(samples, source, point);
    if interpolate && point + 1 == CHART_POINTS && point > 0 && phase < 1.0 {
        let previous = chart_value(samples, source, point - 1);
        let t = phase * phase * (3.0 - 2.0 * phase);
        previous + (current - previous) * t
    } else {
        current
    }
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

fn band_values(
    samples: &VecDeque<Sample>,
    series: &SeriesSpec,
    point: usize,
    phase: f32,
) -> (f32, f32) {
    let upper = interpolated_value(
        samples,
        series.source.as_deref(),
        point,
        phase,
        series.interpolate,
    );
    let lower = series
        .band_source
        .as_deref()
        .map(|source| interpolated_value(samples, Some(source), point, phase, series.interpolate))
        .unwrap_or(upper);
    (upper, lower)
}

fn series_value(samples: &VecDeque<Sample>, series: &SeriesSpec, point: usize, phase: f32) -> f32 {
    match series.kind {
        SeriesKind::Candle => candle_values(samples, series, point).3,
        SeriesKind::Band => {
            let (upper, lower) = band_values(samples, series, point, phase);
            (upper + lower) * 0.5
        }
        _ => interpolated_value(
            samples,
            series.source.as_deref(),
            point,
            phase,
            series.interpolate,
        ),
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
                    let (upper, lower) = band_values(samples, series, point, 1.0);
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
            SeriesKind::StackedBar => {
                bar_lo = bar_lo.min(0.0);
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
    for annotation in &spec.annotations {
        if let AnnotationSpec::Threshold(threshold) = annotation {
            lo = lo.min(threshold.value);
            hi = hi.max(threshold.value);
        }
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
    } else if step >= 1.0 {
        format!("{value:.0}")
    } else if step >= 0.1 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

fn series_color(series: &SeriesSpec, index: usize, theme: &Theme) -> [u8; 3] {
    if let Some(color) = series.color {
        color
    } else if let Some(ink) = series.ink {
        ink_color(theme, ink)
    } else {
        let palette = theme_palette(theme);
        palette[index % palette.len()]
    }
}

fn series_point_color(
    samples: &VecDeque<Sample>,
    series: &SeriesSpec,
    index: usize,
    point: usize,
    theme: &Theme,
) -> [u8; 3] {
    if series.kind == SeriesKind::Candle {
        let (open, _, _, close) = candle_values(samples, series, point);
        return if close >= open {
            series.up_color.unwrap_or(theme.success)
        } else {
            series.down_color.unwrap_or(theme.danger)
        };
    }
    series_color(series, index, theme)
}

#[derive(Debug, Clone)]
struct LegendItem {
    series: usize,
    x: f32,
    y: f32,
    width: f32,
}

#[derive(Debug, Clone, Copy)]
struct LinkedCursor {
    layer: usize,
    sample: usize,
    tooltip: Option<(f32, f32)>,
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

fn viewport_phase(samples: &VecDeque<Sample>, time: f32) -> f32 {
    samples
        .back()
        .map(|sample| ((time - sample.time) / SAMPLE_SECONDS).clamp(0.0, 1.0))
        .unwrap_or(0.0)
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

#[allow(clippy::too_many_arguments)]
fn draw_chart(
    painter: &mut Painter,
    spec: &ChartSpec,
    time: f32,
    bounds: (f32, f32),
    r: LayerRect,
    hidden: &[bool],
    cursor: Option<LinkedCursor>,
    samples: &VecDeque<Sample>,
) {
    let style = &spec.style;
    let theme = painter.theme;
    panel(painter, &spec.id, &r, spec.title.as_deref());
    if let Some(subtitle) = &spec.subtitle {
        painter.text(
            &format!("{}-subtitle", spec.id),
            subtitle,
            r.x + 16.0,
            r.y + 36.0,
            10.0,
            theme.muted,
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
        theme.background,
        style.plot_opacity,
        10.0,
    );
    painter.rect(
        &format!("{}-axis-y", spec.id),
        left,
        top,
        1.0,
        plot_h,
        theme.muted,
        style.axis_opacity,
        0.0,
    );
    painter.rect(
        &format!("{}-axis-x", spec.id),
        left,
        plot_bottom,
        plot_w,
        1.0,
        theme.muted,
        style.axis_opacity,
        0.0,
    );

    let xlo = 0.0;
    let xhi = (CHART_POINTS - 1) as f32;
    let (ylo, yhi) = bounds;
    // Data values are immutable. The viewport, not the samples, moves: a real
    // sample clock leaves a fractional phase between appends, so old marks
    // drift left smoothly instead of being rewritten in place.
    let phase = samples
        .back()
        .map(|sample| ((time - sample.time) / SAMPLE_SECONDS).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let sx = |x: f32| left + (x - phase - xlo) / (xhi - xlo) * plot_w;
    let sy = |y: f32| plot_bottom - (y - ylo) / (yhi - ylo) * plot_h;
    let first_time = samples.front().map(|sample| sample.time).unwrap_or(0.0);
    let last_time = samples.back().map(|sample| sample.time).unwrap_or(0.0);
    let cycle_x = |at: f32| -> Option<f32> {
        let base = at - INCIDENT_OFFSET;
        let k = ((first_time - base) / INCIDENT_CYCLE).ceil();
        let target = base + k * INCIDENT_CYCLE;
        if target < first_time - 0.0001 || target > last_time + 0.0001 {
            None
        } else {
            Some(sx((target - first_time) / SAMPLE_SECONDS))
        }
    };
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
            theme.muted,
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
            theme.muted,
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
            theme.muted,
        );
    }

    for (index, annotation) in spec.annotations.iter().enumerate() {
        let AnnotationSpec::Band(band) = annotation else {
            continue;
        };
        let (Some(x0), Some(x1)) = (cycle_x(band.from), cycle_x(band.to)) else {
            continue;
        };
        let color = band.color.unwrap_or(theme.warning);
        painter.rect(
            &format!("{}-annotation-band-{index}", spec.id),
            x0.min(x1),
            top,
            (x0 - x1).abs().max(1.0),
            plot_h,
            color,
            band.opacity.clamp(0.0, 1.0),
            0.0,
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
        let color = series_color(series, index, &theme);
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
            SeriesKind::StackedBar => {
                let group_slot = plot_w / bar_slots;
                let width =
                    (group_slot * series.bar_width.unwrap_or(0.78).clamp(0.1, 1.0)).max(2.0);
                let radius = series.bar_radius.unwrap_or(2.0);
                for point in points {
                    let lower = stack[point];
                    let upper = lower + chart_value(samples, series.source.as_deref(), point);
                    stack[point] = upper;
                    let y0 = sy(lower.clamp(ylo, yhi));
                    let y1 = sy(upper.clamp(ylo, yhi));
                    let center = sx(point as f32);
                    painter.rect(
                        &format!("{}-{}-stack-bar-{point}", spec.id, index),
                        center - width * 0.5,
                        y0.min(y1),
                        width * 0.82,
                        (y0 - y1).abs().max(1.0),
                        color,
                        series.fill_opacity.unwrap_or(0.9),
                        radius,
                    );
                }
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
                        let (upper, _) = band_values(samples, series, point, phase);
                        (sx(point as f32), sy(upper.clamp(ylo, yhi)))
                    })
                    .collect();
                let lower: Vec<(f32, f32)> = points
                    .iter()
                    .map(|&point| {
                        let (_, lower) = band_values(samples, series, point, phase);
                        (sx(point as f32), sy(lower.clamp(ylo, yhi)))
                    })
                    .collect();
                let (upper, lower) = if series.step {
                    (step_points(&upper), step_points(&lower))
                } else {
                    (upper, lower)
                };
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
                        let y = interpolated_value(
                            samples,
                            series.source.as_deref(),
                            point,
                            phase,
                            series.interpolate,
                        )
                        .clamp(ylo, yhi);
                        (sx(point as f32), sy(y))
                    })
                    .collect();
                if series.smooth {
                    screen = smooth(&screen, style.smooth_steps);
                }
                if series.step && series.kind != SeriesKind::Scatter {
                    screen = step_points(&screen);
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

    for (index, annotation) in spec.annotations.iter().enumerate() {
        match annotation {
            AnnotationSpec::Line(line) => {
                let Some(x) = cycle_x(line.at) else {
                    continue;
                };
                let color = line.color.unwrap_or(theme.warning);
                painter.rect(
                    &format!("{}-annotation-line-{index}", spec.id),
                    x,
                    top,
                    1.0,
                    plot_h,
                    color,
                    line.opacity.clamp(0.0, 1.0),
                    0.0,
                );
                painter.rect(
                    &format!("{}-annotation-mark-{index}", spec.id),
                    x - 2.0,
                    top,
                    4.0,
                    4.0,
                    color,
                    1.0,
                    1.0,
                );
                if let Some(label) = &line.label {
                    let width = approx_text_width(label, 9.0);
                    let label_x = (x + 5.0).min(left + plot_w - width - 4.0).max(left + 4.0);
                    painter.text(
                        &format!("{}-annotation-label-{index}", spec.id),
                        label,
                        label_x,
                        top + 5.0,
                        9.0,
                        color,
                    );
                }
            }
            AnnotationSpec::XEvent(event) => {
                let Some(x) = cycle_x(event.at) else {
                    continue;
                };
                let color = event.color.unwrap_or(theme.primary);
                if event.line.unwrap_or(true) {
                    painter.rect(
                        &format!("{}-annotation-x-line-{index}", spec.id),
                        x,
                        top,
                        1.0,
                        plot_h,
                        color,
                        (event.opacity * 0.45).clamp(0.0, 1.0),
                        0.0,
                    );
                }
                painter.points(
                    &format!("{}-annotation-x-mark-{index}", spec.id),
                    &[(x, plot_bottom - 3.0)],
                    7.0,
                    color,
                    1.0,
                );
                if let Some(label) = &event.label {
                    let width = approx_text_width(label, 9.0);
                    let label_x = (x - width * 0.5).clamp(left, (left + plot_w - width).max(left));
                    let label_y = plot_bottom - 18.0;
                    painter.rect(
                        &format!("{}-annotation-x-label-bg-{index}", spec.id),
                        label_x - 3.0,
                        label_y - 2.0,
                        width + 6.0,
                        12.0,
                        theme.background,
                        0.92,
                        2.0,
                    );
                    painter.text(
                        &format!("{}-annotation-x-label-{index}", spec.id),
                        label,
                        label_x,
                        label_y,
                        9.0,
                        color,
                    );
                }
            }
            AnnotationSpec::Threshold(threshold) => {
                if threshold.value < ylo || threshold.value > yhi {
                    continue;
                }
                let y = sy(threshold.value);
                let color = threshold.color.unwrap_or(theme.danger);
                painter.rect(
                    &format!("{}-annotation-threshold-{index}", spec.id),
                    left,
                    y,
                    plot_w,
                    1.0,
                    color,
                    threshold.opacity.clamp(0.0, 1.0),
                    0.0,
                );
                if let Some(label) = &threshold.label {
                    let width = approx_text_width(label, 9.0);
                    painter.text(
                        &format!("{}-annotation-label-{index}", spec.id),
                        label,
                        (left + plot_w - width - 6.0).max(left + 4.0),
                        (y - 11.0).max(top + 2.0),
                        9.0,
                        color,
                    );
                }
            }
            AnnotationSpec::Band(_) => {}
        }
    }

    let legend_font = spec.style.label_size.max(9.0);
    for item in &layout.legend {
        let index = item.series;
        let series = &spec.series[index];
        let color = if series.kind == SeriesKind::Candle {
            series
                .up_color
                .unwrap_or_else(|| series_color(series, index, &theme))
        } else {
            series_color(series, index, &theme)
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
            SeriesKind::Bar | SeriesKind::StackedBar | SeriesKind::Candle => painter.rect(
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
                theme.muted
            },
        );
    }

    if let Some(cursor) = cursor {
        let nearest = cursor.sample.min(CHART_POINTS - 1);
        let cx = sx(nearest as f32);
        painter.polyline(
            &format!("{}-crosshair", spec.id),
            &[(cx, top), (cx, plot_bottom)],
            1.0,
            theme.text,
            0.34,
        );
        let mut rows = Vec::new();
        for (index, series) in spec.series.iter().enumerate() {
            if hidden.get(index).copied().unwrap_or(false) {
                continue;
            }
            let value = series_value(samples, series, nearest, phase);
            let color = series_point_color(samples, series, index, nearest, &theme);
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
        if let Some((_px, py)) = cursor.tooltip
            && !rows.is_empty()
        {
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
                theme.surface,
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
                theme.muted,
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
                    theme.text,
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

fn format_count(value: usize) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f32 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.0}k", value as f32 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn draw_dense_chart(painter: &mut Painter, spec: &DenseSpec, data: &DenseData, r: LayerRect) {
    let theme = painter.theme;
    panel(painter, &spec.id, &r, spec.title.as_deref());
    let subtitle = spec
        .subtitle
        .clone()
        .unwrap_or_else(|| "resident dense series · GPU bucket decimation".into());
    painter.text(
        &format!("{}-subtitle", spec.id),
        &subtitle,
        r.x + 16.0,
        r.y + 36.0,
        10.0,
        theme.muted,
    );
    let left = r.x + 52.0;
    let top = r.y + 54.0;
    let plot_w = (r.w - 70.0).max(16.0);
    let plot_h = (r.h - 92.0).max(16.0);
    let bottom = top + plot_h;
    painter.rect(
        &format!("{}-plot", spec.id),
        left,
        top,
        plot_w,
        plot_h,
        theme.background,
        0.14,
        10.0,
    );
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for point in data.raw() {
        lo = lo.min(point[1]);
        hi = hi.max(point[1]);
    }
    if !lo.is_finite() || !hi.is_finite() {
        return;
    }
    let (ylo, yhi) = nice_bounds(lo, hi);
    let sy = |y: f32| bottom - (y - ylo) / (yhi - ylo) * plot_h;
    let sx = |x: f32| left + x * plot_w;
    let step = nice_step(yhi - ylo, 5);
    let ticks = (((yhi - ylo) / step).round() as usize).clamp(1, 10);
    for tick in 0..=ticks {
        let value = ylo + tick as f32 * step;
        let y = sy(value);
        painter.rect(
            &format!("{}-grid-{tick}", spec.id),
            left,
            y,
            plot_w,
            1.0,
            theme.muted,
            0.08,
            0.0,
        );
        let label = format_axis_value(value, step);
        let width = approx_text_width(&label, 9.0);
        painter.text(
            &format!("{}-ylabel-{tick}", spec.id),
            &label,
            left - width - 8.0,
            y - 5.0,
            9.0,
            theme.muted,
        );
    }
    for tick in 0..=4 {
        let value = tick as f32 / 4.0;
        let x = sx(value);
        let label = format!("{:.0}%", value * 100.0);
        let width = approx_text_width(&label, 9.0);
        painter.text(
            &format!("{}-xlabel-{tick}", spec.id),
            &label,
            (x - width * 0.5).clamp(left, (left + plot_w - width).max(left)),
            bottom + 8.0,
            9.0,
            theme.muted,
        );
    }
    let color = spec.mode_color(theme);
    let mode = match spec.mode {
        DenseKind::Line => GpuDenseMode::Line,
        DenseKind::Scatter => GpuDenseMode::Scatter,
    };
    painter.gpu_dense(
        &spec.id,
        left,
        top,
        plot_w,
        plot_h,
        data.resource.id(),
        mode,
        [0.0, ylo, 1.0, yhi],
        spec.threshold.clamp(2, 200_000) as u32,
        color,
        1.0,
        2.0,
    );
    painter.text(
        &format!("{}-density", spec.id),
        &format!(
            "{} → {} GPU buckets",
            format_count(data.raw_len()),
            format_count(spec.threshold.clamp(2, 200_000))
        ),
        left,
        bottom + 20.0,
        9.0,
        theme.muted,
    );
}

impl DenseSpec {
    fn mode_color(&self, theme: Theme) -> [u8; 3] {
        match self.mode {
            DenseKind::Line => theme.primary,
            DenseKind::Scatter => theme.info,
        }
    }
}

fn draw_pointcloud3d(
    painter: &mut Painter,
    spec: &PointCloud3DSpec,
    cloud: &CloudData,
    time: f32,
    r: LayerRect,
) {
    let theme = painter.theme;
    panel(painter, &spec.id, &r, spec.title.as_deref());
    if let Some(subtitle) = &spec.subtitle {
        painter.text(
            &format!("{}-subtitle", spec.id),
            subtitle,
            r.x + 16.0,
            r.y + 36.0,
            10.0,
            theme.muted,
        );
    }
    let left = r.x + 16.0;
    let top = r.y + 48.0;
    let plot_w = (r.w - 32.0).max(16.0);
    let plot_h = (r.h - 72.0).max(16.0);
    let yaw = time * 0.32 + spec.seed as f32 * 0.013;
    let pitch = spec.pitch.clamp(-1.2, 1.2);
    painter.gpu_points3d(
        &spec.id,
        left,
        top,
        plot_w,
        plot_h,
        cloud.resource.id(),
        yaw,
        pitch,
        1.0,
        spec.size,
        theme.info,
        0.82,
    );
    let camera = camera_view_proj(yaw, pitch, 1.0, plot_w / plot_h.max(1.0));
    let rect = [left, top, plot_w, plot_h];
    if let Some((x, y)) = project_to(&camera, [0.0, 0.0, 0.0], rect) {
        for (index, axis) in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            .iter()
            .enumerate()
        {
            if let Some((ex, ey)) = project_to(&camera, *axis, rect) {
                let color = [theme.danger, theme.success, theme.info][index];
                painter.polyline(
                    &format!("{}-axis-{index}", spec.id),
                    &[(x, y), (ex, ey)],
                    1.2,
                    color,
                    0.55,
                );
            }
        }
    }
    painter.text(
        &format!("{}-density", spec.id),
        &format!("{} GPU-projected points", format_count(cloud.point_count())),
        r.x + 16.0,
        r.y + r.h - 20.0,
        9.0,
        theme.muted,
    );
}

fn draw_histogram(
    painter: &mut Painter,
    spec: &HistogramSpec,
    r: LayerRect,
    samples: &VecDeque<Sample>,
) {
    let theme = painter.theme;
    panel(painter, &spec.id, &r, spec.title.as_deref());
    if let Some(subtitle) = &spec.subtitle {
        painter.text(
            &format!("{}-subtitle", spec.id),
            subtitle,
            r.x + 16.0,
            r.y + 36.0,
            10.0,
            theme.muted,
        );
    }
    let left = r.x + 52.0;
    let top = r.y + 54.0;
    let plot_w = (r.w - 70.0).max(16.0);
    let plot_h = (r.h - 92.0).max(16.0);
    let bottom = top + plot_h;
    painter.rect(
        &format!("{}-plot", spec.id),
        left,
        top,
        plot_w,
        plot_h,
        theme.background,
        0.14,
        10.0,
    );
    painter.rect(
        &format!("{}-axis-y", spec.id),
        left,
        top,
        1.0,
        plot_h,
        theme.muted,
        0.18,
        0.0,
    );
    painter.rect(
        &format!("{}-axis-x", spec.id),
        left,
        bottom,
        plot_w,
        1.0,
        theme.muted,
        0.18,
        0.0,
    );

    let mut values: Vec<f32> = (0..CHART_POINTS)
        .map(|index| chart_value(samples, Some(&spec.source), index))
        .filter(|value| value.is_finite())
        .collect();
    if values.is_empty() {
        return;
    }
    let mut lo = spec
        .min
        .unwrap_or_else(|| values.iter().copied().fold(f32::INFINITY, f32::min));
    let mut hi = spec
        .max
        .unwrap_or_else(|| values.iter().copied().fold(f32::NEG_INFINITY, f32::max));
    if (hi - lo).abs() < f32::EPSILON {
        let pad = hi.abs().max(1.0) * 0.5;
        lo -= pad;
        hi += pad;
    }
    let bins = spec.bins.clamp(1, 128);
    let mut counts = vec![0.0f32; bins];
    for value in values.drain(..) {
        let t = ((value - lo) / (hi - lo)).clamp(0.0, 0.9999);
        let bin = (t * bins as f32).floor() as usize;
        counts[bin] += 1.0;
    }
    let max_count = counts.iter().copied().fold(1.0f32, f32::max);
    let count_step = nice_step(max_count, 5);
    let count_ticks = ((max_count / count_step).ceil() as usize).clamp(1, 8);
    for tick in 0..=count_ticks {
        let value = tick as f32 * count_step;
        let y = bottom - value / max_count * plot_h;
        painter.rect(
            &format!("{}-grid-{tick}", spec.id),
            left,
            y,
            plot_w,
            1.0,
            theme.muted,
            0.08,
            0.0,
        );
        let label = format_axis_value(value, count_step);
        let width = approx_text_width(&label, 9.0);
        painter.text(
            &format!("{}-ylabel-{tick}", spec.id),
            &label,
            left - width - 8.0,
            y - 5.0,
            9.0,
            theme.muted,
        );
    }
    let bar_w = plot_w / bins as f32;
    let color = spec
        .color
        .unwrap_or_else(|| spec.ink.map_or(theme.primary, |ink| ink_color(&theme, ink)));
    for (index, count) in counts.iter().enumerate() {
        if *count <= 0.0 {
            continue;
        }
        let height = (*count / max_count * plot_h).max(1.0);
        painter.rect(
            &format!("{}-bar-{index}", spec.id),
            left + index as f32 * bar_w + bar_w * 0.08,
            bottom - height,
            (bar_w * 0.84).max(1.0),
            height,
            color,
            spec.opacity.clamp(0.0, 1.0),
            spec.bar_radius,
        );
    }
    for tick in 0..=4 {
        let value = lo + (hi - lo) * tick as f32 / 4.0;
        let label = format_axis_value(value, (hi - lo) / 4.0);
        let width = approx_text_width(&label, 9.0);
        let x = left + plot_w * tick as f32 / 4.0;
        painter.text(
            &format!("{}-xlabel-{tick}", spec.id),
            &label,
            (x - width * 0.5).clamp(left, (left + plot_w - width).max(left)),
            bottom + 8.0,
            9.0,
            theme.muted,
        );
    }
}

fn draw_graph(painter: &mut Painter, spec: &GraphSpec, time: f32, r: LayerRect) {
    let theme = painter.theme;
    panel(painter, &spec.id, &r, spec.title.as_deref());
    let count = spec.node_count.clamp(3, 12);
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
            theme.muted,
            0.30,
        );
        if index % 2 == 0 {
            let chord = (index + count / 2) % count;
            painter.polyline(
                &format!("{}-chord-{index}", spec.id),
                &[nodes[index], nodes[chord]],
                1.0,
                theme.muted,
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

fn draw_map(painter: &mut Painter, spec: &MapSpec, time: f32, r: LayerRect) {
    let theme = painter.theme;
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
            theme.muted,
            0.08,
            0.0,
        );
        painter.text(
            &format!("{}-lon-label-{lon}", spec.id),
            &format!("{lon}"),
            x + 3.0,
            r.y + r.h - 16.0,
            8.0,
            theme.muted,
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
            theme.muted,
            0.08,
            0.0,
        );
        painter.text(
            &format!("{}-lat-label-{lat}", spec.id),
            &format!("{lat}"),
            r.x + 3.0,
            y + 2.0,
            8.0,
            theme.muted,
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
    r: LayerRect,
    base_points: &[(f32, f32, usize)],
) {
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
    let lat = lat.clamp(-85.051_13, 85.051_13);
    (std::f32::consts::FRAC_PI_4 + (lat.to_radians()) / 2.0)
        .tan()
        .ln()
}

fn step_points(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    if points.len() < 2 {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(points.len() * 2);
    out.push(points[0]);
    for pair in points.windows(2) {
        out.push((pair[1].0, pair[0].1));
        out.push(pair[1]);
    }
    out
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
    fn histogram_layer_builds_scene_bars() {
        let spec: LiveSpec = serde_json::from_str(
            r#"{
                "width": 800,
                "height": 600,
                "layers": [{
                    "kind": "histogram",
                    "id": "cpu-dist",
                    "source": "system.cpu",
                    "bins": 8
                }]
            }"#,
        )
        .expect("spec");
        let mut live = LiveViz::new(spec);
        let scene = live.seek(1.0);
        scene.validate().expect("valid scene");
        assert!(
            scene
                .nodes
                .iter()
                .any(|node| node.id.starts_with("cpu-dist-bar-"))
        );
    }

    #[test]
    fn theme_spec_overrides_the_host_palette() {
        let mut theme = Theme::default();
        ThemeSpec {
            primary: Some([255, 0, 128]),
            background: Some([10, 20, 30]),
            ..ThemeSpec::default()
        }
        .apply(&mut theme);
        assert_eq!(theme.primary, [255, 0, 128]);
        assert_eq!(theme.background, [10, 20, 30]);
    }

    #[test]
    fn interpolation_moves_only_the_newest_point() {
        let mut samples = VecDeque::new();
        for index in 0..CHART_POINTS {
            let time = index as f32 * SAMPLE_SECONDS;
            samples.push_back(Sample {
                time,
                metrics: metrics(time),
                system: SystemSample::default(),
            });
        }
        let last = CHART_POINTS - 1;
        let raw = chart_value(&samples, Some("memory"), last);
        let previous = chart_value(&samples, Some("memory"), last - 1);
        let start = interpolated_value(&samples, Some("memory"), last, 0.0, true);
        let end = interpolated_value(&samples, Some("memory"), last, 1.0, true);
        assert_eq!(start, previous);
        assert_eq!(end, raw);
        let mid = interpolated_value(&samples, Some("memory"), last, 0.5, true);
        assert!(mid > start.min(end) && mid < start.max(end));
    }

    #[test]
    fn cycle_annotations_become_scene_nodes() {
        let mut spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        if let LayerSpec::Chart(chart) = &mut spec.layers[0] {
            chart.annotations = vec![
                AnnotationSpec::Line(LineAnnotation {
                    at: 132.0,
                    label: Some("event".into()),
                    color: Some([255, 0, 0]),
                    opacity: 1.0,
                }),
                AnnotationSpec::Threshold(ThresholdAnnotation {
                    value: 100.0,
                    label: Some("SLO".into()),
                    color: Some([255, 255, 0]),
                    opacity: 1.0,
                }),
                AnnotationSpec::XEvent(XEventAnnotation {
                    at: 132.0,
                    label: Some("deploy".into()),
                    color: Some([0, 255, 0]),
                    opacity: 1.0,
                    line: Some(false),
                }),
            ];
        }
        let mut live = LiveViz::new(spec);
        let scene = live.seek(100.0);
        assert!(
            scene
                .nodes
                .iter()
                .any(|node| node.id == "latency-annotation-line-0")
        );
        assert!(
            scene
                .nodes
                .iter()
                .any(|node| node.id == "latency-annotation-threshold-1")
        );
        assert!(
            scene
                .nodes
                .iter()
                .any(|node| node.id == "latency-annotation-x-mark-2")
        );
    }

    #[test]
    fn auto_grid_places_layers_in_declaration_order() {
        let mut spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        spec.grid = Some(GridSpec {
            columns: Some(2),
            rows: Some(1),
            gap: Some(10.0),
            padding: Some(4.0),
        });
        let live = LiveViz::new(spec);
        let first = live.rect_for(0, &RectSpec::default(), 800.0, 600.0, 8.0);
        let second = live.rect_for(1, &RectSpec::default(), 800.0, 600.0, 8.0);
        assert_eq!(first.x, 4.0);
        assert_eq!(first.y, 4.0);
        assert!(second.x > first.x + first.w);
        assert_eq!(second.y, first.y);
        assert_eq!(first.w, second.w);
    }

    #[test]
    fn linked_cursor_uses_the_hovered_chart_sample() {
        let mut spec: LiveSpec = serde_json::from_str(SPEC).expect("spec");
        spec.grid = None;
        let mut live = LiveViz::new(spec);
        let chart = match &live.spec.layers[0] {
            LayerSpec::Chart(chart) => chart,
            _ => panic!("chart"),
        };
        let (width, height) = live.design_size();
        let rect = layer_rect(&chart.rect, width, height, live.spec.gap);
        let layout = chart_layout(chart, &rect);
        live.set_pointer(Some((
            layout.left + layout.plot_w * 0.5,
            layout.top + layout.plot_h * 0.5,
        )));
        let cursor = live
            .linked_cursor(width, height, live.spec.gap)
            .expect("cursor");
        assert!(cursor.tooltip.is_some());
        assert!(cursor.sample < CHART_POINTS);
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
        let mut live = LiveViz::new(spec);
        live.seek(140.0);
        let first = live.history.clone();
        live.seek(140.0);
        assert_eq!(first, live.history);
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
