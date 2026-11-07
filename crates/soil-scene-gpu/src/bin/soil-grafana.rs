//! Render a normalized Grafana dashboard with the Soil scene renderer.
//!
//! Input is the output of `fungos-sdk`'s `grafana-import`; data is a frame
//! fixture in the same JSON shape the dashboard contract uses. This is the
//! native preview path while the Unibus/Prometheus adapter is wired.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;
use soil_scene::{Content, Node, Polyline, Rect, Scene, Text, Transform};
use soil_scene_gpu::Theme;

#[derive(Debug, Deserialize)]
struct Dashboard {
    title: String,
    #[serde(default)]
    panels: Vec<Panel>,
}

#[derive(Debug, Deserialize)]
struct Panel {
    id: String,
    title: String,
    kind: serde_json::Value,
    rect: PanelRect,
    #[serde(default)]
    queries: Vec<Query>,
    #[serde(default)]
    field: FieldConfig,
    #[serde(default)]
    options: PanelOptions,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct PanelRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Debug, Deserialize)]
struct Query {
    ref_id: String,
    #[serde(default)]
    legend: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FieldConfig {
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    decimals: Option<u8>,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    thresholds: Option<Thresholds>,
}

#[derive(Debug, Deserialize)]
struct Thresholds {
    #[serde(default)]
    steps: Vec<ThresholdStep>,
}

#[derive(Debug, Deserialize)]
struct ThresholdStep {
    #[serde(default)]
    value: Option<f64>,
    color: [u8; 3],
}

#[derive(Debug, Default, Deserialize)]
struct PanelOptions {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    reduce: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DataFile {
    #[serde(default)]
    frames: Vec<DataFrame>,
}

#[derive(Debug, Deserialize)]
struct DataFrame {
    ref_id: String,
    #[serde(default)]
    panel: Option<String>,
    #[serde(default)]
    fields: Vec<Field>,
}

#[derive(Debug, Deserialize)]
struct Field {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    values: FieldValues,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", content = "values", rename_all = "snake_case")]
enum FieldValues {
    Time(Vec<Option<i64>>),
    Number(Vec<Option<f64>>),
    String(Vec<Option<String>>),
    Boolean(Vec<Option<bool>>),
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut dashboard = None;
    let mut data = None;
    let mut rgba = None;
    let mut width = 1200u32;
    let mut height = 840u32;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dashboard" => {
                dashboard = Some(PathBuf::from(args.next().context("--dashboard PATH")?))
            }
            "--data" => data = Some(PathBuf::from(args.next().context("--data PATH")?)),
            "--rgba" => rgba = Some(PathBuf::from(args.next().context("--rgba PATH")?)),
            "--width" => width = args.next().context("--width N")?.parse()?,
            "--height" => height = args.next().context("--height N")?.parse()?,
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let dashboard: Dashboard = serde_json::from_slice(
        &std::fs::read(dashboard.context("missing --dashboard")?).context("read dashboard JSON")?,
    )
    .context("parse normalized dashboard JSON")?;
    let data: DataFile = match data {
        Some(path) => serde_json::from_slice(&std::fs::read(path)?).context("parse data JSON")?,
        None => DataFile { frames: Vec::new() },
    };
    let mut painter = Painter::new(width, height);
    painter.text(
        "dashboard-title",
        &dashboard.title,
        18.0,
        16.0,
        22.0,
        painter.theme.text,
    );
    painter.text(
        "dashboard-schema",
        "Soil dashboard preview",
        18.0,
        height as f32 - 18.0,
        10.0,
        painter.theme.muted,
    );
    for panel in &dashboard.panels {
        draw_panel(&mut painter, panel, &data);
    }
    let theme = painter.theme;
    let scene = painter.finish();
    let frame = soil_scene_gpu::render(&scene, width, height, theme)?;
    let path = rgba.context("missing --rgba")?;
    std::fs::write(&path, frame.rgba).with_context(|| format!("write {}", path.display()))?;
    eprintln!(
        "soil-grafana: wrote {} {}x{}",
        path.display(),
        width,
        height
    );
    Ok(())
}

fn draw_panel(painter: &mut Painter, panel: &Panel, data: &DataFile) {
    let x = panel.rect.x * painter.width;
    let y = panel.rect.y * painter.height;
    let w = panel.rect.width * painter.width;
    let h = panel.rect.height * painter.height;
    if w < 24.0 || h < 24.0 {
        return;
    }
    painter.rect(
        &format!("{}-surface", panel.id),
        x,
        y,
        w,
        h,
        painter.theme.surface,
        0.92,
        8.0,
    );
    painter.text(
        &format!("{}-title", panel.id),
        &panel.title,
        x + 14.0,
        y + 12.0,
        14.0,
        painter.theme.text,
    );
    let content_y = y + 38.0;
    let content_h = (h - 48.0).max(12.0);
    let kind = kind_name(&panel.kind);
    match kind.as_str() {
        "time_series" => draw_timeseries(painter, panel, data, x, content_y, w, content_h),
        "stat" => draw_stat(painter, panel, data, x, content_y, w, content_h),
        "gauge" | "bar_gauge" => draw_gauge(painter, panel, data, x, content_y, w, content_h),
        "table" => draw_table(painter, panel, data, x, content_y, w, content_h),
        "text" => {
            let text = panel.options.text.as_deref().unwrap_or("Text panel");
            draw_multiline(
                painter,
                &panel.id,
                text,
                x + 14.0,
                content_y + 4.0,
                w - 28.0,
                12.0,
            );
        }
        other => {
            painter.text(
                &format!("{}-unsupported", panel.id),
                &format!("Unsupported panel: {other}"),
                x + 14.0,
                content_y + 8.0,
                12.0,
                painter.theme.warning,
            );
        }
    }
}

fn draw_timeseries(
    painter: &mut Painter,
    panel: &Panel,
    data: &DataFile,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let left = x + 54.0;
    let top = y + 8.0;
    let plot_w = (w - 68.0).max(16.0);
    let plot_h = (h - 28.0).max(16.0);
    let bottom = top + plot_h;
    let mut series: Vec<(String, [u8; 3], Vec<(i64, f64)>)> = Vec::new();
    for (query_index, query) in panel.queries.iter().enumerate() {
        for frame in data
            .frames
            .iter()
            .filter(|frame| frame_matches(panel, frame, &query.ref_id))
        {
            let Some(time) = frame.fields.iter().find_map(|field| match &field.values {
                FieldValues::Time(values) => Some(values),
                _ => None,
            }) else {
                continue;
            };
            for field in &frame.fields {
                let FieldValues::Number(values) = &field.values else {
                    continue;
                };
                let label = legend_label(query.legend.as_deref(), field);
                let points = time
                    .iter()
                    .zip(values)
                    .filter_map(|(time, value)| Some(((*time)?, (*value)?)))
                    .collect();
                let color = SERIES_COLORS[(query_index + series.len()) % SERIES_COLORS.len()];
                series.push((label, color, points));
            }
        }
    }
    if series.is_empty() {
        painter.text(
            &format!("{}-empty", panel.id),
            "No frames for this panel",
            left,
            top + 10.0,
            11.0,
            painter.theme.muted,
        );
        return;
    }
    let (mut t0, mut t1, mut y0, mut y1) = (i64::MAX, i64::MIN, f64::INFINITY, f64::NEG_INFINITY);
    for (_, _, points) in &series {
        for (time, value) in points {
            t0 = t0.min(*time);
            t1 = t1.max(*time);
            y0 = y0.min(*value);
            y1 = y1.max(*value);
        }
    }
    if panel.field.min.is_some() {
        y0 = panel.field.min.unwrap();
    }
    if panel.field.max.is_some() {
        y1 = panel.field.max.unwrap();
    }
    if t0 >= t1 {
        t1 = t0 + 1;
    }
    if !y0.is_finite() || !y1.is_finite() || y0 >= y1 {
        y0 = 0.0;
        y1 = 1.0;
    }
    let pad = (y1 - y0) * 0.08;
    y0 -= pad;
    y1 += pad;
    painter.rect(
        &format!("{}-plot", panel.id),
        left,
        top,
        plot_w,
        plot_h,
        painter.theme.background,
        0.28,
        4.0,
    );
    for tick in 0..=4 {
        let fraction = tick as f32 / 4.0;
        let gy = bottom - fraction * plot_h;
        painter.rect(
            &format!("{}-grid-{tick}", panel.id),
            left,
            gy,
            plot_w,
            1.0,
            painter.theme.muted,
            0.10,
            0.0,
        );
        let value = y0 + (y1 - y0) * fraction as f64;
        painter.text(
            &format!("{}-ylabel-{tick}", panel.id),
            &format_value(value, panel.field.unit.as_deref(), panel.field.decimals),
            x + 4.0,
            gy - 5.0,
            9.0,
            painter.theme.muted,
        );
    }
    let sx = |time: i64| left + (time - t0) as f32 / (t1 - t0) as f32 * plot_w;
    let sy = |value: f64| bottom - ((value - y0) / (y1 - y0)) as f32 * plot_h;
    for (index, (label, color, points)) in series.iter().enumerate() {
        let screen: Vec<[f32; 2]> = points
            .iter()
            .map(|(time, value)| [painter.nx(sx(*time)), painter.ny(sy(*value))])
            .collect();
        if screen.len() >= 2 {
            painter.polyline(
                &format!("{}-series-{index}", panel.id),
                &screen,
                1.5,
                *color,
            );
        }
        painter.text(
            &format!("{}-legend-{index}", panel.id),
            label,
            left + 6.0,
            top + 6.0 + index as f32 * 13.0,
            9.0,
            *color,
        );
    }
}

fn draw_stat(
    painter: &mut Painter,
    panel: &Panel,
    data: &DataFile,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let Some((value, label)) = last_number(panel, data) else {
        return;
    };
    let color = threshold_color(&panel.field, value, painter.theme);
    painter.text(
        &format!("{}-value", panel.id),
        &format_value(value, panel.field.unit.as_deref(), panel.field.decimals),
        x + 14.0,
        y + h * 0.28,
        (h * 0.38).clamp(18.0, 42.0),
        color,
    );
    painter.text(
        &format!("{}-label", panel.id),
        &label,
        x + 14.0,
        y + h - 18.0,
        10.0,
        painter.theme.muted,
    );
    let _ = w;
}

fn draw_gauge(
    painter: &mut Painter,
    panel: &Panel,
    data: &DataFile,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let Some((value, label)) = last_number(panel, data) else {
        return;
    };
    let min = panel.field.min.unwrap_or(0.0);
    let max = panel.field.max.unwrap_or(100.0).max(min + 1.0);
    let fraction = ((value - min) / (max - min)).clamp(0.0, 1.0) as f32;
    let color = threshold_color(&panel.field, value, painter.theme);
    let track_y = y + h * 0.42;
    painter.rect(
        &format!("{}-track", panel.id),
        x + 14.0,
        track_y,
        (w - 28.0).max(10.0),
        10.0,
        painter.theme.background,
        0.5,
        4.0,
    );
    painter.rect(
        &format!("{}-fill", panel.id),
        x + 14.0,
        track_y,
        ((w - 28.0) * fraction).max(2.0),
        10.0,
        color,
        0.95,
        4.0,
    );
    painter.text(
        &format!("{}-value", panel.id),
        &format_value(value, panel.field.unit.as_deref(), panel.field.decimals),
        x + 14.0,
        y + 6.0,
        (h * 0.28).clamp(16.0, 30.0),
        color,
    );
    painter.text(
        &format!("{}-label", panel.id),
        &label,
        x + 14.0,
        track_y + 16.0,
        9.0,
        painter.theme.muted,
    );
}

fn draw_table(
    painter: &mut Painter,
    panel: &Panel,
    data: &DataFile,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let frame = panel.queries.iter().find_map(|query| {
        data.frames
            .iter()
            .find(|frame| frame_matches(panel, frame, &query.ref_id))
    });
    let Some(frame) = frame else {
        return;
    };
    let columns: Vec<&Field> = frame
        .fields
        .iter()
        .filter(|field| field.kind != "time")
        .collect();
    let rows = columns
        .iter()
        .map(|field| match &field.values {
            FieldValues::Number(values) => values.len(),
            FieldValues::String(values) => values.len(),
            FieldValues::Boolean(values) => values.len(),
            FieldValues::Time(values) => values.len(),
        })
        .max()
        .unwrap_or(0)
        .min(((h - 24.0) / 18.0).max(1.0) as usize);
    let col_w = (w - 28.0) / columns.len().max(1) as f32;
    for (index, field) in columns.iter().enumerate() {
        painter.text(
            &format!("{}-head-{index}", panel.id),
            &field.name,
            x + 14.0 + index as f32 * col_w,
            y + 4.0,
            9.0,
            painter.theme.muted,
        );
    }
    for row in 0..rows {
        for (index, field) in columns.iter().enumerate() {
            let value = field_value(field, row);
            painter.text(
                &format!("{}-cell-{row}-{index}", panel.id),
                &value,
                x + 14.0 + index as f32 * col_w,
                y + 20.0 + row as f32 * 18.0,
                9.0,
                painter.theme.text,
            );
        }
    }
}

fn draw_multiline(
    painter: &mut Painter,
    id: &str,
    text: &str,
    x: f32,
    y: f32,
    _max_w: f32,
    size: f32,
) {
    for (index, line) in text.lines().take(12).enumerate() {
        painter.text(
            &format!("{id}-line-{index}"),
            line,
            x,
            y + index as f32 * (size * 1.35),
            size,
            painter.theme.text,
        );
    }
}

fn frame_matches(panel: &Panel, frame: &DataFrame, ref_id: &str) -> bool {
    frame.ref_id == ref_id
        && frame
            .panel
            .as_deref()
            .is_none_or(|panel_id| panel_id == panel.id)
}

fn legend_label(template: Option<&str>, field: &Field) -> String {
    let Some(template) = template else {
        return field
            .labels
            .get("instance")
            .cloned()
            .unwrap_or_else(|| field.name.clone());
    };
    let mut out = template.to_owned();
    for (key, value) in &field.labels {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    if out.contains("{{") {
        return field
            .labels
            .get("instance")
            .cloned()
            .unwrap_or_else(|| field.name.clone());
    }
    out
}

fn kind_name(value: &serde_json::Value) -> String {
    if let Some(value) = value.as_str() {
        return value.to_owned();
    }
    value
        .get("custom")
        .and_then(|custom| custom.get("kind"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("custom")
        .to_owned()
}

fn last_number(panel: &Panel, data: &DataFile) -> Option<(f64, String)> {
    for query in &panel.queries {
        for frame in data
            .frames
            .iter()
            .filter(|frame| frame_matches(panel, frame, &query.ref_id))
        {
            for field in &frame.fields {
                if let FieldValues::Number(values) = &field.values {
                    if let Some(value) = reduce_number(values, panel.options.reduce.as_deref()) {
                        return Some((
                            value,
                            query.legend.clone().unwrap_or_else(|| field.name.clone()),
                        ));
                    }
                }
            }
        }
    }
    None
}

fn reduce_number(values: &[Option<f64>], reduce: Option<&str>) -> Option<f64> {
    let mut present = values.iter().flatten().copied();
    match reduce.unwrap_or("last") {
        "first" | "firstNotNull" => present.next(),
        "min" => present.reduce(f64::min),
        "max" => present.reduce(f64::max),
        "mean" | "average" => {
            let all: Vec<f64> = present.collect();
            (!all.is_empty()).then(|| all.iter().sum::<f64>() / all.len() as f64)
        }
        "sum" | "total" => Some(present.sum()),
        "count" => Some(values.iter().flatten().count() as f64),
        _ => present.next_back(),
    }
}

fn field_value(field: &Field, row: usize) -> String {
    match &field.values {
        FieldValues::Time(values) => values
            .get(row)
            .copied()
            .flatten()
            .map(|v| v.to_string())
            .unwrap_or_default(),
        FieldValues::Number(values) => values
            .get(row)
            .copied()
            .flatten()
            .map(|v| format!("{v:.2}"))
            .unwrap_or_default(),
        FieldValues::String(values) => values.get(row).cloned().flatten().unwrap_or_default(),
        FieldValues::Boolean(values) => values
            .get(row)
            .copied()
            .flatten()
            .map(|v| v.to_string())
            .unwrap_or_default(),
    }
}

fn format_value(value: f64, unit: Option<&str>, decimals: Option<u8>) -> String {
    let decimals = decimals.unwrap_or(if value.abs() >= 100.0 { 0 } else { 1 }) as usize;
    let unit = unit.unwrap_or("");
    match unit {
        "percent" | "percentunit" => format!("{value:.0}%"),
        "ms" => format!("{value:.decimals$}ms"),
        "reqps" => format!("{value:.decimals$}/s"),
        "bytes" => format!("{value:.decimals$}B"),
        "" => format!("{value:.decimals$}"),
        other => format!("{value:.decimals$} {other}"),
    }
}

fn threshold_color(field: &FieldConfig, value: f64, theme: Theme) -> [u8; 3] {
    field
        .thresholds
        .as_ref()
        .and_then(|thresholds| {
            thresholds
                .steps
                .iter()
                .filter(|step| {
                    step.value
                        .map(|threshold| value >= threshold)
                        .unwrap_or(true)
                })
                .last()
                .map(|step| step.color)
        })
        .unwrap_or(theme.primary)
}

const SERIES_COLORS: [[u8; 3]; 6] = [
    [108, 168, 255],
    [112, 205, 255],
    [245, 190, 80],
    [122, 214, 142],
    [255, 122, 122],
    [200, 160, 255],
];

struct Painter {
    nodes: Vec<Node>,
    width: f32,
    height: f32,
    theme: Theme,
    z: f32,
}

impl Painter {
    fn new(width: u32, height: u32) -> Self {
        Self {
            nodes: Vec::new(),
            width: width as f32,
            height: height as f32,
            theme: Theme::default(),
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
        size / (self.theme.body * 1.25).max(1.0)
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

    fn polyline(&mut self, id: &str, points: &[[f32; 2]], width: f32, color: [u8; 3]) {
        if points.len() < 2 {
            return;
        }
        self.z += 0.02;
        self.nodes.push(Node {
            id: id.into(),
            parent: None,
            transition_ms: Some(0),
            transform: Transform::default(),
            content: Content::Polyline(Polyline {
                points: points.to_vec(),
                ink: None,
                rgb: Some(color),
                rgb2: None,
                width: self.sh(width),
                opacity: 1.0,
                join: soil_scene::LineJoin::Round,
                miter_limit: 4.0,
            }),
        });
    }
}

impl Painter {
    fn finish(self) -> Scene {
        Scene::new(self.nodes)
    }
}
