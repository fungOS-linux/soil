//! Deterministic, host-owned UI layout.
//!
//! A guest or Lua module describes a bounded tree; the host measures text and
//! resolves it with Taffy. The result is a flat list of absolute rectangles the
//! renderer can draw. No IO, clock, font handle, or pixels cross this boundary.

use serde::{Deserialize, Serialize};

/// A width/height pair in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const ZERO: Self = Self {
        width: 0.0,
        height: 0.0,
    };
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// An axis-aligned rectangle in logical pixels, relative to the root origin.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Per-edge padding in logical pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub const fn all(value: f32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    pub const fn xy(x: f32, y: f32) -> Self {
        Self {
            top: y,
            right: x,
            bottom: y,
            left: x,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Justify {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// Layout constraints shared by containers and leaves.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutStyle {
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub min_width: Option<f32>,
    pub max_width: Option<f32>,
    pub min_height: Option<f32>,
    pub max_height: Option<f32>,
    pub padding: Edges,
    pub gap: f32,
    pub direction: Direction,
    pub align_items: Align,
    pub justify_content: Justify,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Option<f32>,
}

impl Default for LayoutStyle {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            min_width: None,
            max_width: None,
            min_height: None,
            max_height: None,
            padding: Edges::default(),
            gap: 0.0,
            direction: Direction::Column,
            align_items: Align::Stretch,
            justify_content: Justify::Start,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: None,
        }
    }
}

impl LayoutStyle {
    pub fn row() -> Self {
        Self {
            direction: Direction::Row,
            ..Self::default()
        }
    }

    pub fn column() -> Self {
        Self::default()
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    pub fn min_height(mut self, height: f32) -> Self {
        self.min_height = Some(height);
        self
    }

    pub fn padding(mut self, padding: Edges) -> Self {
        self.padding = padding;
        self
    }

    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align_items = align;
        self
    }

    pub fn justify(mut self, justify: Justify) -> Self {
        self.justify_content = justify;
        self
    }

    pub fn grow(mut self, grow: f32) -> Self {
        self.flex_grow = grow;
        self
    }

    pub fn shrink(mut self, shrink: f32) -> Self {
        self.flex_shrink = shrink;
        self
    }
}

/// Font metrics a host must provide. The layout engine never guesses glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextStyle {
    pub size: f32,
    pub line_height: f32,
    pub wrap: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size: 14.0,
            line_height: 20.0,
            wrap: true,
        }
    }
}

/// The drawable payload of a node. Layout style lives on [`Node::layout`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKind {
    Container,
    Text { text: String, style: TextStyle },
    Rule,
    Spacer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(default)]
    pub layout: LayoutStyle,
    #[serde(flatten)]
    pub kind: NodeKind,
    #[serde(default)]
    pub children: Vec<Node>,
}

impl Node {
    pub fn container(id: impl Into<String>, layout: LayoutStyle, children: Vec<Node>) -> Self {
        Self {
            id: id.into(),
            layout,
            kind: NodeKind::Container,
            children,
        }
    }

    pub fn text(
        id: impl Into<String>,
        text: impl Into<String>,
        style: TextStyle,
        layout: LayoutStyle,
    ) -> Self {
        Self {
            id: id.into(),
            layout,
            kind: NodeKind::Text {
                text: text.into(),
                style,
            },
            children: Vec::new(),
        }
    }

    pub fn rule(id: impl Into<String>, layout: LayoutStyle) -> Self {
        Self {
            id: id.into(),
            layout,
            kind: NodeKind::Rule,
            children: Vec::new(),
        }
    }

    pub fn spacer(id: impl Into<String>, layout: LayoutStyle) -> Self {
        Self {
            id: id.into(),
            layout,
            kind: NodeKind::Spacer,
            children: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlacedKind {
    Container,
    Text { text: String, style: TextStyle },
    Rule,
    Spacer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacedNode {
    pub id: String,
    pub parent: Option<String>,
    pub rect: Rect,
    #[serde(flatten)]
    pub kind: PlacedKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutResult {
    pub root: Rect,
    pub nodes: Vec<PlacedNode>,
}

/// Text metrics supplied by the host.
pub trait TextMeasurer {
    /// Measure `text` at `style`. When `max_width` is present, wrap to it.
    fn measure(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Size;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    EmptyTree,
    DuplicateId(String),
    InvalidStyle(String),
    InvalidAvailableSize,
    Taffy(String),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyTree => write!(f, "layout tree is empty"),
            Self::DuplicateId(id) => write!(f, "duplicate layout node id: {id}"),
            Self::InvalidStyle(id) => {
                write!(f, "layout node has a non-finite or negative style: {id}")
            }
            Self::InvalidAvailableSize => {
                write!(f, "available layout size must be finite and non-negative")
            }
            Self::Taffy(error) => write!(f, "taffy layout failed: {error}"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Resolve `root` into absolute rectangles.
pub fn solve(
    root: &Node,
    available: Size,
    measurer: &mut impl TextMeasurer,
) -> Result<LayoutResult, LayoutError> {
    if root.id.is_empty() {
        return Err(LayoutError::EmptyTree);
    }
    if !available.width.is_finite()
        || !available.height.is_finite()
        || available.width < 0.0
        || available.height < 0.0
    {
        return Err(LayoutError::InvalidAvailableSize);
    }
    let mut root = root.clone();
    // A host surface is a viewport: the root fills it unless the caller asked
    // for a smaller root explicitly.
    root.layout.width.get_or_insert(available.width);
    root.layout.height.get_or_insert(available.height);
    validate(&root, &mut std::collections::BTreeSet::new())?;

    let mut tree: taffy::TaffyTree<NodeKind> = taffy::TaffyTree::new();
    let taffy_root = build(&mut tree, &root)?;
    let available_space = taffy::Size {
        width: taffy::AvailableSpace::Definite(available.width),
        height: taffy::AvailableSpace::Definite(available.height),
    };
    tree.compute_layout_with_measure(taffy_root, available_space, |input, _id, context, style| {
        let text = match context {
            Some(NodeKind::Text { text, style }) => Some((text.as_str(), *style)),
            _ => None,
        };
        taffy::compute_leaf_layout(
            input,
            style,
            |_, _| 0.0,
            |known, available| {
                let Some((text, text_style)) = text else {
                    return taffy::Size::ZERO;
                };
                let max_width = known.width.or_else(|| match available.width {
                    taffy::AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let measured = measurer.measure(
                    text,
                    &text_style,
                    if text_style.wrap { max_width } else { None },
                );
                taffy::Size {
                    width: known.width.unwrap_or(measured.width),
                    height: known.height.unwrap_or(measured.height),
                }
            },
        )
    })
    .map_err(|error| LayoutError::Taffy(error.to_string()))?;

    let mut nodes = Vec::new();
    collect(&tree, taffy_root, None, &root, &mut nodes)?;
    let layout = tree
        .layout(taffy_root)
        .map_err(|error| LayoutError::Taffy(error.to_string()))?;
    Ok(LayoutResult {
        root: Rect {
            x: layout.location.x,
            y: layout.location.y,
            width: layout.size.width,
            height: layout.size.height,
        },
        nodes,
    })
}

fn validate(node: &Node, ids: &mut std::collections::BTreeSet<String>) -> Result<(), LayoutError> {
    if node.id.is_empty() || !ids.insert(node.id.clone()) {
        return Err(LayoutError::DuplicateId(node.id.clone()));
    }
    validate_style(&node.layout, &node.id)?;
    if let NodeKind::Text { style, .. } = &node.kind
        && (!style.size.is_finite()
            || style.size <= 0.0
            || !style.line_height.is_finite()
            || style.line_height < style.size)
    {
        return Err(LayoutError::InvalidStyle(node.id.clone()));
    }
    for child in &node.children {
        validate(child, ids)?;
    }
    Ok(())
}

fn validate_style(style: &LayoutStyle, id: &str) -> Result<(), LayoutError> {
    let dimensions = [
        style.width,
        style.height,
        style.min_width,
        style.max_width,
        style.min_height,
        style.max_height,
        style.flex_basis,
    ];
    let edges = [
        style.padding.top,
        style.padding.right,
        style.padding.bottom,
        style.padding.left,
    ];
    if dimensions
        .iter()
        .flatten()
        .chain(edges.iter())
        .any(|value| !value.is_finite() || *value < 0.0)
        || !style.gap.is_finite()
        || style.gap < 0.0
        || !style.flex_grow.is_finite()
        || style.flex_grow < 0.0
        || !style.flex_shrink.is_finite()
        || style.flex_shrink < 0.0
    {
        return Err(LayoutError::InvalidStyle(id.to_owned()));
    }
    if let (Some(min), Some(max)) = (style.min_width, style.max_width)
        && min > max
    {
        return Err(LayoutError::InvalidStyle(id.to_owned()));
    }
    if let (Some(min), Some(max)) = (style.min_height, style.max_height)
        && min > max
    {
        return Err(LayoutError::InvalidStyle(id.to_owned()));
    }
    Ok(())
}

fn build(tree: &mut taffy::TaffyTree<NodeKind>, node: &Node) -> Result<taffy::NodeId, LayoutError> {
    match &node.kind {
        NodeKind::Container => {
            let children = node
                .children
                .iter()
                .map(|child| build(tree, child))
                .collect::<Result<Vec<_>, _>>()?;
            if children.is_empty() {
                tree.new_leaf(to_taffy(&node.layout))
                    .map_err(|error| LayoutError::Taffy(error.to_string()))
            } else {
                tree.new_with_children(to_taffy(&node.layout), &children)
                    .map_err(|error| LayoutError::Taffy(error.to_string()))
            }
        }
        NodeKind::Text { text, style } => tree
            .new_leaf_with_context(
                to_taffy(&node.layout),
                NodeKind::Text {
                    text: text.clone(),
                    style: *style,
                },
            )
            .map_err(|error| LayoutError::Taffy(error.to_string())),
        NodeKind::Rule | NodeKind::Spacer => tree
            .new_leaf(to_taffy(&node.layout))
            .map_err(|error| LayoutError::Taffy(error.to_string())),
    }
}

fn collect(
    tree: &taffy::TaffyTree<NodeKind>,
    id: taffy::NodeId,
    parent: Option<&str>,
    source: &Node,
    out: &mut Vec<PlacedNode>,
) -> Result<(), LayoutError> {
    let layout = tree
        .layout(id)
        .map_err(|error| LayoutError::Taffy(error.to_string()))?;
    let kind = match &source.kind {
        NodeKind::Container => PlacedKind::Container,
        NodeKind::Text { text, style } => PlacedKind::Text {
            text: text.clone(),
            style: *style,
        },
        NodeKind::Rule => PlacedKind::Rule,
        NodeKind::Spacer => PlacedKind::Spacer,
    };
    out.push(PlacedNode {
        id: source.id.clone(),
        parent: parent.map(str::to_owned),
        rect: Rect {
            x: layout.location.x,
            y: layout.location.y,
            width: layout.size.width,
            height: layout.size.height,
        },
        kind,
    });
    let child_ids = tree
        .children(id)
        .map_err(|error| LayoutError::Taffy(error.to_string()))?;
    for (child_source, child_id) in source.children.iter().zip(child_ids) {
        collect(tree, child_id, Some(&source.id), child_source, out)?;
    }
    Ok(())
}

fn to_taffy(style: &LayoutStyle) -> taffy::Style {
    taffy::Style {
        display: taffy::Display::Flex,
        flex_direction: match style.direction {
            Direction::Row => taffy::FlexDirection::Row,
            Direction::Column => taffy::FlexDirection::Column,
        },
        size: taffy::Size {
            width: dimension(style.width),
            height: dimension(style.height),
        },
        min_size: taffy::Size {
            width: dimension_auto(style.min_width),
            height: dimension_auto(style.min_height),
        },
        max_size: taffy::Size {
            width: dimension_auto(style.max_width),
            height: dimension_auto(style.max_height),
        },
        padding: taffy::Rect {
            top: taffy::LengthPercentage::length(style.padding.top),
            right: taffy::LengthPercentage::length(style.padding.right),
            bottom: taffy::LengthPercentage::length(style.padding.bottom),
            left: taffy::LengthPercentage::length(style.padding.left),
        },
        gap: taffy::Size {
            width: taffy::LengthPercentage::length(style.gap),
            height: taffy::LengthPercentage::length(style.gap),
        },
        align_items: match style.align_items {
            Align::Start => taffy::AlignItems::FLEX_START,
            Align::Center => taffy::AlignItems::CENTER,
            Align::End => taffy::AlignItems::FLEX_END,
            Align::Stretch => taffy::AlignItems::STRETCH,
        },
        justify_content: match style.justify_content {
            Justify::Start => taffy::JustifyContent::FLEX_START,
            Justify::Center => taffy::JustifyContent::CENTER,
            Justify::End => taffy::JustifyContent::FLEX_END,
            Justify::SpaceBetween => taffy::JustifyContent::SPACE_BETWEEN,
            Justify::SpaceAround => taffy::JustifyContent::SPACE_AROUND,
            Justify::SpaceEvenly => taffy::JustifyContent::SPACE_EVENLY,
        },
        flex_grow: style.flex_grow,
        flex_shrink: style.flex_shrink,
        flex_basis: style
            .flex_basis
            .map_or(taffy::Dimension::auto(), taffy::Dimension::length),
        ..Default::default()
    }
}

fn dimension(value: Option<f32>) -> taffy::Dimension {
    value.map_or(taffy::Dimension::auto(), taffy::Dimension::length)
}

fn dimension_auto(value: Option<f32>) -> taffy::LengthPercentageAuto {
    value.map_or(
        taffy::LengthPercentageAuto::auto(),
        taffy::LengthPercentageAuto::length,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockMeasurer {
        calls: usize,
    }

    impl TextMeasurer for MockMeasurer {
        fn measure(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Size {
            self.calls += 1;
            let char_width = style.size * 0.55;
            let natural = text.chars().count() as f32 * char_width;
            let width = max_width.map_or(natural, |max| natural.min(max));
            let lines = if max_width.is_some() && natural > width {
                (natural / width.max(1.0)).ceil()
            } else {
                1.0
            };
            Size::new(width, lines * style.line_height)
        }
    }

    fn text(id: &str, value: &str) -> Node {
        Node::text(id, value, TextStyle::default(), LayoutStyle::default())
    }

    fn container(id: &str, style: LayoutStyle, children: Vec<Node>) -> Node {
        Node::container(id, style, children)
    }

    #[test]
    fn column_pads_and_gaps() {
        let tree = container(
            "root",
            LayoutStyle::default().padding(Edges::all(10.0)).gap(8.0),
            vec![
                container("a", LayoutStyle::default().height(20.0), vec![]),
                container("b", LayoutStyle::default().height(30.0), vec![]),
            ],
        );
        let result = solve(&tree, Size::new(200.0, 100.0), &mut MockMeasurer::default()).unwrap();
        assert_eq!(
            result.root,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0
            }
        );
        assert_eq!(
            result.nodes[1].rect,
            Rect {
                x: 10.0,
                y: 10.0,
                width: 180.0,
                height: 20.0
            }
        );
        assert_eq!(
            result.nodes[2].rect,
            Rect {
                x: 10.0,
                y: 38.0,
                width: 180.0,
                height: 30.0
            }
        );
    }

    #[test]
    fn row_allocates_fixed_and_growing_children() {
        let tree = container(
            "root",
            LayoutStyle::row().height(44.0).gap(8.0),
            vec![
                container("fixed", LayoutStyle::default().width(80.0), vec![]),
                container("grow", LayoutStyle::default().grow(1.0), vec![]),
            ],
        );
        let result = solve(&tree, Size::new(300.0, 44.0), &mut MockMeasurer::default()).unwrap();
        assert_eq!(result.nodes[1].rect.width, 80.0);
        assert_eq!(result.nodes[2].rect.width, 212.0);
        assert_eq!(result.nodes[2].rect.x, 88.0);
    }

    #[test]
    fn text_wraps_to_available_width() {
        let tree = container(
            "root",
            LayoutStyle::default().padding(Edges::all(10.0)),
            vec![text("body", "one two three four five six seven eight")],
        );
        let mut measurer = MockMeasurer::default();
        let result = solve(&tree, Size::new(100.0, 400.0), &mut measurer).unwrap();
        let body = &result.nodes[1];
        assert_eq!(body.rect.width, 80.0);
        assert!(body.rect.height > 20.0);
        assert!(measurer.calls > 0);
    }

    #[test]
    fn rejects_duplicate_ids() {
        let tree = container(
            "root",
            LayoutStyle::default(),
            vec![text("same", "a"), text("same", "b")],
        );
        assert_eq!(
            solve(&tree, Size::new(100.0, 100.0), &mut MockMeasurer::default()),
            Err(LayoutError::DuplicateId("same".into()))
        );
    }

    #[test]
    fn rejects_invalid_sizes() {
        let tree = container("root", LayoutStyle::default().width(-1.0), vec![]);
        assert!(matches!(
            solve(&tree, Size::new(100.0, 100.0), &mut MockMeasurer::default()),
            Err(LayoutError::InvalidStyle(_))
        ));
        assert_eq!(
            solve(
                &tree,
                Size::new(f32::NAN, 100.0),
                &mut MockMeasurer::default()
            ),
            Err(LayoutError::InvalidAvailableSize)
        );
    }

    #[test]
    fn partial_style_uses_defaults() {
        let node: Node = serde_json::from_str(
            r#"{"id":"root","kind":"container","layout":{"direction":"row","gap":4}}"#,
        )
        .unwrap();
        assert_eq!(node.layout.direction, Direction::Row);
        assert_eq!(node.layout.gap, 4.0);
        assert_eq!(node.layout.align_items, Align::Stretch);
    }

    #[test]
    fn serde_round_trip() {
        let tree = container(
            "root",
            LayoutStyle::row().gap(4.0),
            vec![text("label", "hello")],
        );
        let json = serde_json::to_string(&tree).unwrap();
        let parsed: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(tree, parsed);
    }
}
