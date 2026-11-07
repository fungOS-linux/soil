//! What a Spore puts in the space the host gives it.
//!
//! The host's whole job for a surface is three things: how big it is, where the
//! pixels go, and what the input means. This crate is the middle one — the
//! payload a Spore presents. A Spore may hand over a scene (the host rasterizes),
//! a CPU-rendered RGBA frame, or a lease on a GPU texture it rendered itself.
//!
//! It is deliberately *not* a WIT interface. WIT describes what the host provides
//! — extent, present, input. This is data that crosses that boundary, and data
//! evolves faster than an ABI should: a new node kind must never require
//! rebuilding every Spore that renders. So the document carries its own version
//! and the host refuses what it cannot draw.

use serde::{Deserialize, Serialize};

pub mod shm;

/// The schema this crate speaks. Bumped when a change is not additive.
pub const SCENE_VERSION: u32 = 1;

/// Host-enforced ceilings. Declared here so a producer can budget against the
/// same numbers the host will refuse at.
pub const MAX_NODES: usize = 4096;
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

/// The only failure this crate produces: a document the host will not draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl Error {
    fn invalid(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// A payload a Spore presents into the space the host gave it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Present {
    /// Nodes the host lays out and draws. Resolution-independent, themed, and
    /// hit-testable, which is why it is the recommended payload.
    Scene(Scene),
    /// Pixels the Spore rendered itself: emulators, image tools, anything with
    /// its own rasterizer.
    Rgba(RgbaFrame),
    /// A texture the Spore rendered on the GPU. The document names a lease; the
    /// host owns extent and composition. Never a native handle: that is
    /// registration data on the socket, re-established every session.
    GpuLease(GpuLease),
}

impl Present {
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Scene(scene) => scene.validate(),
            Self::Rgba(frame) => frame.validate(),
            Self::GpuLease(lease) => lease.validate(),
        }
    }

    /// Canonical bytes for the wire. CBOR would be smaller; JSON keeps a scene
    /// inspectable on a machine with no tooling, which is worth more right now
    /// than the bytes it saves.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| Error::invalid(format!("scene encode: {error}")))?;
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(Error::invalid(format!(
                "payload is {} bytes, over the {MAX_PAYLOAD_BYTES} byte ceiling",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(Error::invalid("payload over the byte ceiling"));
        }
        let present: Self = serde_json::from_slice(bytes)
            .map_err(|error| Error::invalid(format!("scene decode: {error}")))?;
        present.validate()?;
        Ok(present)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub schema: u32,
    pub nodes: Vec<Node>,
    /// Draw this scene *over* what is behind it instead of on a host ground.
    ///
    /// Off by default, and deliberately: a scene that assumes transparency and
    /// arrives a frame late shows the desktop through the gap, which reads as a
    /// flicker. A window has a ground unless its Spore asks for the composite.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub transparent: bool,
}

impl Scene {
    pub fn new(nodes: Vec<Node>) -> Self {
        Self {
            schema: SCENE_VERSION,
            nodes,
            transparent: false,
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.schema != SCENE_VERSION {
            return Err(Error::invalid(format!(
                "scene schema {} is not {SCENE_VERSION}",
                self.schema
            )));
        }
        if self.nodes.len() > MAX_NODES {
            return Err(Error::invalid(format!(
                "scene has {} nodes, over the {MAX_NODES} ceiling",
                self.nodes.len()
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for node in &self.nodes {
            if !seen.insert(node.id.as_str()) {
                return Err(Error::invalid(format!("duplicate node id {:?}", node.id)));
            }
            node.validate()?;
        }
        // A parent must exist and must precede its child: the renderer draws in
        // order, so a forward reference is a scene that draws differently
        // depending on where it was parsed.
        for (index, node) in self.nodes.iter().enumerate() {
            if let Some(parent) = &node.parent
                && !self.nodes[..index]
                    .iter()
                    .any(|candidate| &candidate.id == parent)
            {
                return Err(Error::invalid(format!(
                    "node {:?} names parent {parent:?} that does not precede it",
                    node.id
                )));
            }
        }
        Ok(())
    }
}

/// A scene node.
///
/// `deny_unknown_fields` is absent on purpose: `content` is flattened, and serde
/// cannot reconcile the two. The content variant rejects unknown fields itself,
/// which is where a typo in a node kind would actually appear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default)]
    pub transform: Transform,
    #[serde(flatten)]
    pub content: Content,
    /// Milliseconds to ease from this node's previous state. Absent means the
    /// host's default; zero means snap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_ms: Option<u64>,
}

impl Node {
    fn validate(&self) -> Result<(), Error> {
        if self.id.is_empty() || self.id.len() > 128 {
            return Err(Error::invalid("node id must be 1-128 bytes"));
        }
        if self.id.chars().any(char::is_control) {
            return Err(Error::invalid("node id must be printable"));
        }
        self.transform.validate()?;
        self.content.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    #[serde(default)]
    pub translation: [f32; 3],
    #[serde(default)]
    pub rotation: [f32; 3],
    #[serde(default = "unit_scale")]
    pub scale: [f32; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scale: unit_scale(),
        }
    }
}

const fn unit_scale() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

impl Transform {
    fn validate(&self) -> Result<(), Error> {
        for value in self
            .translation
            .iter()
            .chain(self.rotation.iter())
            .chain(self.scale.iter())
        {
            if !value.is_finite() {
                return Err(Error::invalid("transform must be finite"));
            }
        }
        Ok(())
    }
}

/// A theme role. Semantic inks keep the host's palette authoritative; a literal
/// colour is for things that own their identity (an app's mark, a brand).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ink {
    Text,
    Muted,
    Surface,
    Background,
    Primary,
    Success,
    Warning,
    Danger,
    Info,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Content {
    Group,
    Rect(Rect),
    Text(Text),
    Image(Image),
    /// A GPU line strip. Points are local to the node transform: translation
    /// and scale place the strip, so a chart can emit one node per series.
    Polyline(Polyline),
    /// A filled area between a polyline and a local baseline.
    Area(Area),
    /// A filled ribbon between an upper and lower polyline. Used for
    /// confidence bands and stacked areas.
    Band(Band),
    /// A dense point cloud, one primitive instead of one node per point.
    Points(Points),
    /// A row-major scalar field rendered through the host heat ramp.
    Heatmap(Heatmap),
    /// OHLC candles, one primitive instead of a wick and body node each.
    Candles(Candles),
    Shader(Shader),
}

impl Content {
    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Group => Ok(()),
            Self::Rect(rect) => rect.validate(),
            Self::Text(text) => text.validate(),
            Self::Image(image) => image.validate(),
            Self::Polyline(polyline) => polyline.validate(),
            Self::Area(area) => area.validate(),
            Self::Band(band) => band.validate(),
            Self::Points(points) => points.validate(),
            Self::Heatmap(heatmap) => heatmap.validate(),
            Self::Candles(candles) => candles.validate(),
            Self::Shader(shader) => shader.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink: Option<Ink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
    #[serde(default = "opaque")]
    pub opacity: f32,
    #[serde(default)]
    pub radius: f32,
}

impl Default for Rect {
    fn default() -> Self {
        Self {
            ink: None,
            rgb: None,
            opacity: opaque(),
            radius: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Text {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink: Option<Ink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
    #[serde(default = "opaque")]
    pub opacity: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    /// An asset id the Spore declared, never a path: a document must not be able
    /// to steer the host at arbitrary files.
    pub asset: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Polyline {
    /// At least two points. Each point is `[x, y]` in local scene units.
    pub points: Vec<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink: Option<Ink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
    /// Optional end colour. The renderer interpolates along the stroke.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb2: Option<[u8; 3]>,
    /// Stroke width in local scene units.
    #[serde(default = "unit")]
    pub width: f32,
    #[serde(default = "opaque")]
    pub opacity: f32,
    #[serde(default)]
    pub join: LineJoin,
    /// Miter joins longer than `miter_limit * width` fall back to a bevel.
    #[serde(default = "default_miter_limit")]
    pub miter_limit: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

fn default_miter_limit() -> f32 {
    4.0
}

impl Polyline {
    fn validate(&self) -> Result<(), Error> {
        if self.points.len() < 2 || self.points.len() > 1_000_000 {
            return Err(Error::invalid("polyline needs 2..=1,000,000 points"));
        }
        for point in &self.points {
            finite(point[0])?;
            finite(point[1])?;
        }
        if !self.width.is_finite() || self.width <= 0.0 {
            return Err(Error::invalid("polyline width must be positive"));
        }
        if !self.miter_limit.is_finite() || self.miter_limit <= 0.0 {
            return Err(Error::invalid("polyline miter_limit must be positive"));
        }
        finite(self.opacity)?;
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(Error::invalid("polyline opacity must be 0..=1"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Points {
    /// At least one point. Each point is `[x, y]` in local scene units.
    pub points: Vec<[f32; 2]>,
    /// Local width and height of each point.
    pub size: [f32; 2],
    #[serde(default)]
    pub shape: PointShape,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink: Option<Ink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
    #[serde(default = "opaque")]
    pub opacity: f32,
}

impl Points {
    fn validate(&self) -> Result<(), Error> {
        if self.points.is_empty() || self.points.len() > 1_000_000 {
            return Err(Error::invalid("points needs 1..=1,000,000 points"));
        }
        for point in &self.points {
            finite(point[0])?;
            finite(point[1])?;
        }
        for size in self.size {
            if !size.is_finite() || size <= 0.0 {
                return Err(Error::invalid("point size must be positive"));
            }
        }
        finite(self.opacity)?;
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(Error::invalid("points opacity must be 0..=1"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PointShape {
    #[default]
    Square,
    Circle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heatmap {
    pub rows: u32,
    pub cols: u32,
    /// Row-major, exactly `rows * cols` finite values.
    pub values: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f32>,
    #[serde(default = "opaque")]
    pub opacity: f32,
    /// Skip cells whose value is zero. Density fields use this so an empty
    /// region does not paint the low end of the colour ramp.
    #[serde(default)]
    pub zero_transparent: bool,
}

impl Heatmap {
    fn validate(&self) -> Result<(), Error> {
        if self.rows == 0 || self.cols == 0 || self.rows > 4096 || self.cols > 4096 {
            return Err(Error::invalid("heatmap needs 1..=4096 rows and cols"));
        }
        if self.values.len() != self.rows as usize * self.cols as usize {
            return Err(Error::invalid("heatmap values must be rows * cols"));
        }
        if self.values.len() > 4_000_000 {
            return Err(Error::invalid("heatmap is too large"));
        }
        for value in &self.values {
            finite(*value)?;
        }
        for bound in [self.min, self.max].into_iter().flatten() {
            finite(bound)?;
        }
        if let (Some(min), Some(max)) = (self.min, self.max)
            && min >= max
        {
            return Err(Error::invalid("heatmap min must be below max"));
        }
        finite(self.opacity)?;
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(Error::invalid("heatmap opacity must be 0..=1"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candles {
    pub candles: Vec<Candle>,
    /// Body width in local scene units.
    pub width: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_ink: Option<Ink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down_ink: Option<Ink>,
    /// Literal up/down colours for a series that owns its palette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_rgb: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down_rgb: Option<[u8; 3]>,
    #[serde(default = "opaque")]
    pub opacity: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candle {
    pub x: f32,
    pub open: f32,
    pub high: f32,
    pub low: f32,
    pub close: f32,
}

impl Candles {
    fn validate(&self) -> Result<(), Error> {
        if self.candles.is_empty() || self.candles.len() > 1_000_000 {
            return Err(Error::invalid("candles needs 1..=1,000,000 entries"));
        }
        if !self.width.is_finite() || self.width <= 0.0 {
            return Err(Error::invalid("candle width must be positive"));
        }
        for candle in &self.candles {
            for value in [candle.x, candle.open, candle.high, candle.low, candle.close] {
                finite(value)?;
            }
            if candle.high < candle.low {
                return Err(Error::invalid("candle high must be above low"));
            }
        }
        finite(self.opacity)?;
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(Error::invalid("candles opacity must be 0..=1"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Area {
    pub points: Vec<[f32; 2]>,
    /// Local y the fill drops to.
    pub baseline: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink: Option<Ink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
    #[serde(default = "opaque")]
    pub opacity: f32,
    /// Alpha at the baseline. `None` keeps `opacity` flat; `Some(0.0)` fades
    /// the fill out toward the baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_opacity: Option<f32>,
}

impl Area {
    fn validate(&self) -> Result<(), Error> {
        if self.points.len() < 2 || self.points.len() > 1_000_000 {
            return Err(Error::invalid("area needs 2..=1,000,000 points"));
        }
        for point in &self.points {
            finite(point[0])?;
            finite(point[1])?;
        }
        finite(self.baseline)?;
        finite(self.opacity)?;
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(Error::invalid("area opacity must be 0..=1"));
        }
        if let Some(base) = self.base_opacity {
            finite(base)?;
            if !(0.0..=1.0).contains(&base) {
                return Err(Error::invalid("area base opacity must be 0..=1"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Band {
    /// Upper boundary, one `[x, y]` per sample in local scene units.
    pub upper: Vec<[f32; 2]>,
    /// Lower boundary. Must have the same length as `upper`.
    pub lower: Vec<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink: Option<Ink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
    #[serde(default = "opaque")]
    pub opacity: f32,
}

impl Band {
    fn validate(&self) -> Result<(), Error> {
        if self.upper.len() < 2 || self.upper.len() > 1_000_000 {
            return Err(Error::invalid("band needs 2..=1,000,000 samples"));
        }
        if self.lower.len() != self.upper.len() {
            return Err(Error::invalid(
                "band upper and lower must have equal length",
            ));
        }
        for point in self.upper.iter().chain(self.lower.iter()) {
            finite(point[0])?;
            finite(point[1])?;
        }
        finite(self.opacity)?;
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(Error::invalid("band opacity must be 0..=1"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shader {
    /// WGSL. The host compiles and caches it; a new shader is data, not a build.
    pub wgsl: String,
    /// Floats the shader reads from its uniform buffer, in order.
    #[serde(default)]
    pub uniforms: Vec<f32>,
}

const fn opaque() -> f32 {
    1.0
}

const fn unit() -> f32 {
    1.0
}

impl Rect {
    fn validate(&self) -> Result<(), Error> {
        finite(self.opacity)?;
        finite(self.radius)?;
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(Error::invalid("rect opacity must be 0..=1"));
        }
        if self.radius < 0.0 {
            return Err(Error::invalid("rect radius must not be negative"));
        }
        Ok(())
    }
}

impl Text {
    fn validate(&self) -> Result<(), Error> {
        finite(self.opacity)?;
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(Error::invalid("text opacity must be 0..=1"));
        }
        if self.text.len() > MAX_TEXT_BYTES {
            return Err(Error::invalid(format!(
                "text is {} bytes, over the {MAX_TEXT_BYTES} ceiling",
                self.text.len()
            )));
        }
        if self
            .text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(Error::invalid("text must not contain control characters"));
        }
        Ok(())
    }
}

impl Image {
    fn validate(&self) -> Result<(), Error> {
        safe_id(&self.asset, "asset")
    }
}

impl Shader {
    fn validate(&self) -> Result<(), Error> {
        if self.wgsl.trim().is_empty() {
            return Err(Error::invalid("shader needs WGSL"));
        }
        if self.wgsl.len() > MAX_TEXT_BYTES * 4 {
            return Err(Error::invalid("shader source is too large"));
        }
        for value in &self.uniforms {
            finite(*value)?;
        }
        Ok(())
    }
}

/// A CPU-rendered frame, exactly the size the host asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, `width * height * 4` bytes.
    pub pixels: serde_bytes_lite::Bytes,
}

impl RgbaFrame {
    pub fn validate(&self) -> Result<(), Error> {
        if self.width == 0 || self.height == 0 {
            return Err(Error::invalid("frame needs a non-zero extent"));
        }
        let expected = u64::from(self.width) * u64::from(self.height) * 4;
        if expected != self.pixels.0.len() as u64 {
            return Err(Error::invalid(format!(
                "{}x{} RGBA8 is {expected} bytes, got {}",
                self.width,
                self.height,
                self.pixels.0.len()
            )));
        }
        Ok(())
    }
}

/// `serde_bytes` without the dependency: a byte vector that serialises as an
/// array of numbers is 4x the size for nothing.
pub mod serde_bytes_lite {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Bytes(pub Vec<u8>);

    impl Serialize for Bytes {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(&self.0)
        }
    }

    impl<'de> Deserialize<'de> for Bytes {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let bytes = Vec::<u8>::deserialize(deserializer)?;
            Ok(Self(bytes))
        }
    }
}

/// A lease on a texture the Spore rendered, not a handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpuLease {
    pub source_id: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub revision: u64,
}

impl GpuLease {
    fn validate(&self) -> Result<(), Error> {
        safe_id(&self.source_id, "lease")?;
        if self.width == 0 || self.height == 0 {
            return Err(Error::invalid("lease needs a non-zero extent"));
        }
        Ok(())
    }
}

fn finite(value: f32) -> Result<(), Error> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(Error::invalid("value must be finite"))
    }
}

fn safe_id(value: &str, what: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
    {
        return Err(Error::invalid(format!(
            "{what} id is not a safe identifier"
        )));
    }
    Ok(())
}
