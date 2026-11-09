//! Resolve a JSON layout tree with the same Taffy host Canvas uses.
//!
//! This is a development/integration tool: `tend --scene` can call it so a
//! captured `soil_scene` has real flex geometry instead of the fake row layout.

use soil_layout::{LayoutResult, Node, PlacedKind, Size, TextMeasurer, TextStyle, solve};
use std::io::Read;

struct ApproxText;

impl TextMeasurer for ApproxText {
    fn measure(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Size {
        let size = if style.size > 0.0 { style.size } else { 16.0 };
        let width = text.chars().count() as f32 * size * 0.55;
        let width = max_width.map_or(width, |limit| width.min(limit));
        Size::new(width, style.line_height.max(size))
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("soil-layout: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut input = None;
    let mut width = 1200.0_f32;
    let mut height = 900.0_f32;
    let mut output = "json";
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input = args.next(),
            "--width" => {
                width = args
                    .next()
                    .ok_or("--width requires a number")?
                    .parse()
                    .map_err(|_| "invalid width")?
            }
            "--height" => {
                height = args
                    .next()
                    .ok_or("--height requires a number")?
                    .parse()
                    .map_err(|_| "invalid height")?
            }
            "--lua" => output = "lua",
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let input = input.ok_or("missing --input")?;
    let mut source = String::new();
    std::fs::File::open(&input)
        .map_err(|error| format!("open {input}: {error}"))?
        .read_to_string(&mut source)
        .map_err(|error| format!("read {input}: {error}"))?;
    let tree: Node =
        serde_json::from_str(&source).map_err(|error| format!("parse {input}: {error}"))?;
    let mut measurer = ApproxText;
    let result =
        solve(&tree, Size::new(width, height), &mut measurer).map_err(|error| error.to_string())?;
    match output {
        "lua" => print!("{}", lua_result(&result)),
        _ => println!(
            "{}",
            serde_json::to_string(&result).map_err(|error| error.to_string())?
        ),
    }
    Ok(())
}

fn lua_result(result: &LayoutResult) -> String {
    let mut out = String::from("return {root={x=");
    out.push_str(&result.root.x.to_string());
    out.push_str(",y=");
    out.push_str(&result.root.y.to_string());
    out.push_str(",width=");
    out.push_str(&result.root.width.to_string());
    out.push_str(",height=");
    out.push_str(&result.root.height.to_string());
    out.push_str("},nodes={");
    for (index, node) in result.nodes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{id=");
        out.push_str(&lua_string(&node.id));
        out.push_str(",parent=");
        match &node.parent {
            Some(parent) => out.push_str(&lua_string(parent)),
            None => out.push_str("nil"),
        }
        out.push_str(",rect={x=");
        out.push_str(&node.rect.x.to_string());
        out.push_str(",y=");
        out.push_str(&node.rect.y.to_string());
        out.push_str(",width=");
        out.push_str(&node.rect.width.to_string());
        out.push_str(",height=");
        out.push_str(&node.rect.height.to_string());
        out.push('}');
        match &node.kind {
            PlacedKind::Container => out.push_str(",kind=\"container\""),
            PlacedKind::Rule => out.push_str(",kind=\"rule\""),
            PlacedKind::Spacer => out.push_str(",kind=\"spacer\""),
            PlacedKind::Text { text, style } => {
                out.push_str(",kind=\"text\",text=");
                out.push_str(&lua_string(text));
                out.push_str(",style={size=");
                out.push_str(&style.size.to_string());
                out.push_str(",line_height=");
                out.push_str(&style.line_height.to_string());
                out.push_str(",wrap=");
                out.push_str(if style.wrap { "true" } else { "false" });
                out.push('}');
            }
        }
        out.push('}');
    }
    out.push_str("}}\n");
    out
}

fn lua_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{{{:x}}}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}
