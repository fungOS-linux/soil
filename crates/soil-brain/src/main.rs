//! The Elmers "brain" as a Soil scene.
//!
//! The dashboard's brain view is a force-directed graph in the browser: a few
//! hundred workflows, their links, and the function that owns each. This binary
//! turns the sidecar's `GET /graph` into a `soil_scene` document, so the same
//! picture can be drawn by the GPU path (`soil-viz --scene`) instead of by d3 in
//! a DOM.
//!
//! Deterministic on purpose: a force layout lands differently on every run, and
//! a scene whose nodes move when nothing changed is a scene nobody can point
//! at. Positions come from the graph's own order, not from a simulation.
//!
//! ```sh
//! curl -s http://127.0.0.1:8792/graph > graph.json
//! soil-brain --graph graph.json --out brain.json
//! soil-viz --scene brain.json
//! ```

use std::collections::BTreeMap;
use std::f32::consts::TAU;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;
use soil_scene::{Content, Node, Polyline, Rect, Scene, Text, Transform};

const CANVAS_W: f32 = 1600.0;
const CANVAS_H: f32 = 1000.0;
const RING_INNER: f32 = 210.0;
const RING_OUTER: f32 = 430.0;
const NODE_W: f32 = 132.0;
const NODE_H: f32 = 30.0;

const INK_TEXT: [u8; 3] = [0xdb, 0xe4, 0xee];
const INK_MUTED: [u8; 3] = [0x6d, 0x7b, 0x8c];
const INK_EDGE: [u8; 3] = [0x2c, 0x38, 0x46];
const INK_NODE: [u8; 3] = [0x1b, 0x24, 0x30];
const INK_SPEND: [u8; 3] = [0xf0, 0xa2, 0x6b];
const INK_GATED: [u8; 3] = [0xd1, 0x4b, 0x4b];

#[derive(Debug, Deserialize)]
struct Graph {
    #[serde(default)]
    functions: Vec<String>,
    #[serde(default)]
    workflows: Vec<Workflow>,
}

#[derive(Debug, Deserialize)]
struct Workflow {
    name: String,
    #[serde(default)]
    owners: Vec<String>,
    #[serde(default)]
    links: Vec<String>,
    #[serde(default)]
    model_spend: String,
    #[serde(default)]
    gated: bool,
}

#[derive(Default)]
struct Painter {
    nodes: Vec<Node>,
    z: f32,
}

impl Painter {
    fn push(&mut self, id: String, transform: Transform, content: Content) {
        self.z += 0.001;
        self.nodes.push(Node {
            id,
            parent: None,
            transform,
            content,
            transition_ms: Some(0),
        });
    }

    fn rect(&mut self, id: String, x: f32, y: f32, w: f32, h: f32, radius: f32, rgb: [u8; 3]) {
        self.push(
            id,
            Transform {
                translation: [x, y, 0.0],
                ..Transform::default()
            },
            Content::Rect(Rect {
                radius,
                rgb: Some(rgb),
                ..Rect::default()
            }),
        );
        let _ = (w, h);
    }

    fn text(&mut self, id: String, value: &str, x: f32, y: f32, size: f32, rgb: [u8; 3]) {
        let scale = size / 16.0;
        self.push(
            id,
            Transform {
                translation: [x, y, 0.0],
                rotation: [0.0; 3],
                scale: [scale, scale, 1.0],
            },
            Content::Text(Text {
                text: value.to_string(),
                ink: None,
                rgb: Some(rgb),
                opacity: 1.0,
            }),
        );
    }

    fn line(&mut self, id: String, points: Vec<[f32; 2]>, width: f32, rgb: [u8; 3]) {
        if points.len() < 2 {
            return;
        }
        self.push(
            id,
            Transform::default(),
            Content::Polyline(Polyline {
                points,
                ink: None,
                rgb: Some(rgb),
                rgb2: None,
                width,
                opacity: 1.0,
                join: soil_scene::LineJoin::Round,
                miter_limit: 4.0,
            }),
        );
    }

    fn finish(self) -> Scene {
        Scene::new(self.nodes)
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut graph = None;
    let mut out = None;
    let mut width = CANVAS_W;
    let mut height = CANVAS_H;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--graph" => graph = Some(PathBuf::from(args.next().context("--graph PATH")?)),
            "--out" => out = Some(PathBuf::from(args.next().context("--out PATH")?)),
            "--width" => width = args.next().context("--width N")?.parse()?,
            "--height" => height = args.next().context("--height N")?.parse()?,
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let graph: Graph = serde_json::from_slice(
        &std::fs::read(graph.context("missing --graph PATH")?).context("read graph JSON")?,
    )
    .context("parse GET /graph JSON")?;

    let scene = build(&graph, width, height);
    scene.validate().map_err(|error| anyhow::anyhow!(error))?;
    let out = out.context("missing --out PATH")?;
    std::fs::write(&out, serde_json::to_vec_pretty(&scene)?)
        .with_context(|| format!("write {}", out.display()))?;
    eprintln!(
        "soil-brain: {} workflows, {} nodes -> {}",
        graph.workflows.len(),
        scene.nodes.len(),
        out.display()
    );
    Ok(())
}

fn build(graph: &Graph, width: f32, height: f32) -> Scene {
    let mut painter = Painter::default();
    let cx = width / 2.0;
    let cy = height / 2.0;

    painter.text(
        "brain-title".into(),
        "Elmers — workflow brain",
        24.0,
        24.0,
        22.0,
        INK_TEXT,
    );
    painter.text(
        "brain-count".into(),
        &format!("{} workflows", graph.workflows.len()),
        24.0,
        54.0,
        12.0,
        INK_MUTED,
    );

    // Group workflows by their first owner so a function's work sits together.
    // A workflow with no owner still has to appear: it lands in its own arc
    // rather than being silently dropped from the picture.
    let mut by_owner: BTreeMap<&str, Vec<&Workflow>> = BTreeMap::new();
    for workflow in &graph.workflows {
        let owner = workflow
            .owners
            .first()
            .map(String::as_str)
            .unwrap_or("unowned");
        by_owner.entry(owner).or_default().push(workflow);
    }
    let mut sectors: Vec<(&str, Vec<&Workflow>)> = by_owner.into_iter().collect();
    // The sidecar's function order is the order a person reads the org in, so
    // the arcs follow it; anything the graph knows but the list does not lands
    // at the end rather than vanishing.
    let rank = |name: &str| {
        graph
            .functions
            .iter()
            .position(|f| f == name)
            .unwrap_or(graph.functions.len())
    };
    sectors.sort_by(|a, b| rank(a.0).cmp(&rank(b.0)).then_with(|| a.0.cmp(b.0)));

    let total: usize = sectors.iter().map(|(_, list)| list.len()).sum();
    let mut placed: BTreeMap<&str, (f32, f32)> = BTreeMap::new();
    let mut angle_cursor = -0.25 * TAU;
    let node_scale = if total > 220 { 0.62 } else { 1.0 };

    for (owner, list) in &sectors {
        let share = list.len() as f32 / total.max(1) as f32;
        let arc = share * TAU;
        let mid = angle_cursor + arc / 2.0;

        // The hub sits inside the ring its workflows occupy: the label is the
        // function, and the rings are its work at increasing radius.
        let hub_r = RING_INNER * 0.55;
        let hub_x = cx + hub_r * mid.cos();
        let hub_y = cy + hub_r * mid.sin();
        painter.rect(
            format!("hub-{owner}"),
            hub_x,
            hub_y,
            10.0,
            10.0,
            5.0,
            INK_SPEND,
        );
        painter.text(
            format!("hub-label-{owner}"),
            *owner,
            hub_x + 10.0,
            hub_y - 5.0,
            13.0,
            INK_TEXT,
        );

        for (index, workflow) in list.iter().enumerate() {
            let step = if list.len() <= 1 {
                0.5
            } else {
                index as f32 / (list.len() - 1) as f32
            };
            let angle = angle_cursor + arc * (0.08 + 0.84 * step);
            let ring = (index % 3) as f32;
            let radius = RING_INNER + (RING_OUTER - RING_INNER) * (ring / 2.0);
            let x = cx + radius * angle.cos();
            let y = cy + radius * angle.sin();
            placed.insert(workflow.name.as_str(), (x, y));

            let fill = if workflow.gated {
                INK_GATED
            } else if workflow.model_spend != "none" && !workflow.model_spend.is_empty() {
                INK_SPEND
            } else {
                INK_NODE
            };
            let w = NODE_W * node_scale;
            let h = NODE_H * node_scale;
            painter.rect(
                format!("node-{}", workflow.name),
                x - w / 2.0,
                y - h / 2.0,
                w,
                h,
                6.0,
                fill,
            );
            // Only the labels that fit: at this density a wall of 6px text is
            // noise, so short names get their title and the rest are shapes.
            if total <= 120 || workflow.name.len() <= 22 {
                painter.text(
                    format!("label-{}", workflow.name),
                    &workflow.name,
                    x - w / 2.0 + 6.0,
                    y - 5.0,
                    9.0,
                    INK_TEXT,
                );
            }
        }
        angle_cursor += arc;
    }

    // Links last, so a line never covers the node it starts from.
    for workflow in &graph.workflows {
        let Some(&(x0, y0)) = placed.get(workflow.name.as_str()) else {
            continue;
        };
        for (index, target) in workflow.links.iter().enumerate() {
            let Some(&(x1, y1)) = placed.get(target.as_str()) else {
                continue;
            };
            painter.line(
                format!("edge-{}-{index}", workflow.name),
                vec![[x0, y0], [x1, y1]],
                1.0,
                INK_EDGE,
            );
        }
    }

    painter.text(
        "brain-legend".into(),
        "amber: spends a model   red: gated   grey: neither",
        24.0,
        height - 24.0,
        11.0,
        INK_MUTED,
    );

    painter.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow(name: &str, owner: &str, links: &[&str]) -> Workflow {
        Workflow {
            name: name.to_string(),
            owners: vec![owner.to_string()],
            links: links.iter().map(|l| (*l).to_string()).collect(),
            model_spend: "none".to_string(),
            gated: false,
        }
    }

    #[test]
    fn a_graph_becomes_a_valid_scene() {
        let graph = Graph {
            functions: vec!["cto".into()],
            workflows: vec![workflow("a", "cto", &["b"]), workflow("b", "cto", &[])],
        };
        let scene = build(&graph, CANVAS_W, CANVAS_H);
        scene.validate().expect("scene is valid");
        assert!(scene.nodes.iter().any(|n| n.id == "node-a"));
        assert!(scene.nodes.iter().any(|n| n.id.starts_with("edge-a")));
    }

    #[test]
    fn layout_is_deterministic() {
        let graph = Graph {
            functions: vec![],
            workflows: vec![workflow("a", "x", &[]), workflow("b", "y", &["a"])],
        };
        let first = serde_json::to_string(&build(&graph, CANVAS_W, CANVAS_H)).unwrap();
        let second = serde_json::to_string(&build(&graph, CANVAS_W, CANVAS_H)).unwrap();
        assert_eq!(first, second, "no simulation, no drift");
    }
}
