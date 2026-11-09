//! A native Soil window for a DFM-annotated model, rendered with wgpu.
//!
//! This is the 3D sibling of `soil-viz`: same "Soil owns the host" idea, but the
//! scene is a triangle mesh rather than a rect/text document. It reads the same
//! `data.json` a Spore UI would be handed — mesh, per-triangle patch assignment,
//! and DFM findings — so nothing here re-derives an analysis.
//!
//! What it draws:
//!   * the mesh, lit with a key light, a hemisphere term and a view-dependent rim,
//!     which is what gives the amber highlight something to sit on;
//!   * the triangles belonging to a flagged patch, in the highlight material;
//!   * one billboard marker per finding that maps to geometry, sized in pixels and
//!     tinted by severity.
//!
//! `--screenshot out.bmp` renders offscreen and writes a 24-bit BMP, so the
//! renderer can be checked without a window or a GPU-driving display.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

// ---------------------------------------------------------------- shader

const SHADER: &str = r#"
struct Globals {
  view_proj: mat4x4<f32>,
  cam_right: vec4<f32>,
  cam_up: vec4<f32>,
  eye: vec4<f32>,
  light: vec4<f32>,
  base: vec4<f32>,
  highlight: vec4<f32>,
};
@group(0) @binding(0) var<uniform> g: Globals;

struct VIn {
  @location(0) pos: vec3<f32>,
  @location(1) nrm: vec3<f32>,
  @location(2) tint: f32,
};
struct VOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) nrm: vec3<f32>,
  @location(1) world: vec3<f32>,
  @location(2) tint: f32,
};

@vertex
fn vs_mesh(v: VIn) -> VOut {
  var o: VOut;
  o.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
  o.nrm = v.nrm;
  o.world = v.pos;
  o.tint = v.tint;
  return o;
}

@fragment
fn fs_mesh(i: VOut) -> @location(0) vec4<f32> {
  let n = normalize(i.nrm);
  let to_eye = normalize(g.eye.xyz - i.world);
  let key = max(dot(n, normalize(g.light.xyz)), 0.0);
  // Hemisphere: cool from below, warm from above, so unlit faces read as form
  // rather than as holes.
  let hemi = mix(vec3<f32>(0.045, 0.06, 0.085), vec3<f32>(0.34, 0.38, 0.44), n.y * 0.5 + 0.5);
  let rim = pow(1.0 - max(dot(n, to_eye), 0.0), 2.8) * 0.42;
  let albedo = mix(g.base.rgb, g.highlight.rgb, i.tint);
  var c = albedo * (hemi + key * 0.62);
  c = c + vec3<f32>(0.18, 0.30, 0.48) * rim;
  // A little self-lit lift so the flagged patch reads even when it faces away.
  c = c + g.highlight.rgb * i.tint * 0.22;
  return vec4<f32>(c, 1.0);
}

struct MIn {
  @location(0) center: vec3<f32>,
  @location(1) corner: vec2<f32>,
  @location(2) tint: f32,
  @location(3) hot: f32,
};
struct MOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) local: vec2<f32>,
  @location(1) tint: f32,
  @location(2) hot: f32,
};

@vertex
fn vs_marker(m: MIn) -> MOut {
  var o: MOut;
  // Constant pixel size: scale the billboard by the distance to the eye.
  let d = distance(g.eye.xyz, m.center);
  let px = select(9.0, 14.0, m.hot > 0.5) * d * 0.0016;
  let world = m.center + (g.cam_right.xyz * m.corner.x + g.cam_up.xyz * m.corner.y) * px;
  o.clip = g.view_proj * vec4<f32>(world, 1.0);
  o.local = m.corner;
  o.tint = m.tint;
  o.hot = m.hot;
  return o;
}

@fragment
fn fs_marker(i: MOut) -> @location(0) vec4<f32> {
  let r = length(i.local);
  let core = 1.0 - smoothstep(0.42, 0.50, r);
  let ring = smoothstep(0.72, 0.80, r) * (1.0 - smoothstep(0.96, 1.0, r));
  let halo = (1.0 - smoothstep(0.0, 1.0, r)) * 0.35 * i.hot;
  let alpha = max(max(core, ring), halo);
  if (alpha <= 0.01) { discard; }
  let sev = i.tint;
  var rgb: vec3<f32>;
  if (sev < 0.5) { rgb = vec3<f32>(1.0, 0.64, 0.17); }        // warning
  else if (sev < 1.5) { rgb = vec3<f32>(0.42, 0.66, 1.0); }   // note
  else { rgb = vec3<f32>(0.90, 0.28, 0.30); }                 // blocker
  return vec4<f32>(rgb, alpha);
}
"#;

// ---------------------------------------------------------------- data

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    cam_right: [f32; 4],
    cam_up: [f32; 4],
    eye: [f32; 4],
    light: [f32; 4],
    base: [f32; 4],
    highlight: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct MeshVert {
    pos: [f32; 3],
    nrm: [f32; 3],
    tint: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct MarkVert {
    center: [f32; 3],
    corner: [f32; 2],
    tint: f32,
    active: f32,
    _pad: f32,
}

struct Marker {
    center: [f32; 3],
    severity: f32,
    rule: String,
    message: String,
    measured: Option<f32>,
    limit: Option<f32>,
    /// Screen position, recomputed each frame for picking and card placement.
    pos: [f32; 2],
}

struct Scene3d {
    name: String,
    verts: Vec<MeshVert>,
    indices: Vec<u32>,
    markers: Vec<Marker>,
    #[allow(dead_code)]
    centre: [f32; 3],
    radius: f32,
    triangles: usize,
    highlighted: usize,
}

fn load_scene(path: &str) -> Result<Scene3d> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    let doc: serde_json::Value = serde_json::from_str(&text).context("parse scene json")?;

    let pos: Vec<f32> = doc["mesh"]["positions"]
        .as_array()
        .context("mesh.positions")?
        .iter()
        .map(|v| v.as_f64().unwrap_or(0.0) as f32)
        .collect();
    let idx: Vec<u32> = doc["mesh"]["indices"]
        .as_array()
        .context("mesh.indices")?
        .iter()
        .map(|v| v.as_u64().unwrap_or(0) as u32)
        .collect();
    let assign: Vec<i64> = doc["mesh"]["assign"]
        .as_array()
        .context("mesh.assign")?
        .iter()
        .map(|v| v.as_i64().unwrap_or(-1))
        .collect();

    let findings = doc["findings"].as_array().cloned().unwrap_or_default();
    // Every patch any finding points at.
    let mut pointed: Vec<i64> = Vec::new();
    for f in &findings {
        for p in f["patch_ids"].as_array().cloned().unwrap_or_default() {
            if let Some(v) = p.as_i64() {
                pointed.push(v);
            }
        }
    }

    // Smooth normals: accumulate face normals per vertex index.
    let mut nrm = vec![0.0f32; pos.len()];
    for t in 0..assign.len() {
        let (a, b, c) = (
            idx[t * 3] as usize,
            idx[t * 3 + 1] as usize,
            idx[t * 3 + 2] as usize,
        );
        let p = |i: usize| [pos[i * 3], pos[i * 3 + 1], pos[i * 3 + 2]];
        let (pa, pb, pc) = (p(a), p(b), p(c));
        let u = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
        let v = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        for corner in [a, b, c] {
            nrm[corner * 3] += n[0];
            nrm[corner * 3 + 1] += n[1];
            nrm[corner * 3 + 2] += n[2];
        }
    }

    // Per-triangle tint: 1.0 when the triangle's patch is pointed at.
    let mut tri_tint = vec![0.0f32; assign.len()];
    let mut highlighted = 0usize;
    for (t, patch) in assign.iter().enumerate() {
        if pointed.contains(patch) {
            tri_tint[t] = 1.0;
            highlighted += 1;
        }
    }

    // Bounds, then re-centre so the camera orbits the part rather than the origin.
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for i in 0..pos.len() / 3 {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(pos[i * 3 + axis]);
            hi[axis] = hi[axis].max(pos[i * 3 + axis]);
        }
    }
    let centre = [
        (lo[0] + hi[0]) * 0.5,
        (lo[1] + hi[1]) * 0.5,
        (lo[2] + hi[2]) * 0.5,
    ];
    let radius =
        ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt() * 0.5;

    // De-indexed vertices: a shared vertex has one tint per triangle otherwise.
    let mut verts = Vec::with_capacity(assign.len() * 3);
    let mut indices = Vec::with_capacity(assign.len() * 3);
    for t in 0..assign.len() {
        for corner in 0..3 {
            let v = idx[t * 3 + corner] as usize;
            let n = [nrm[v * 3], nrm[v * 3 + 1], nrm[v * 3 + 2]];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-9);
            verts.push(MeshVert {
                pos: [
                    pos[v * 3] - centre[0],
                    pos[v * 3 + 1] - centre[1],
                    pos[v * 3 + 2] - centre[2],
                ],
                nrm: [n[0] / len, n[1] / len, n[2] / len],
                tint: tri_tint[t],
                _pad: 0.0,
            });
            indices.push(indices.len() as u32);
        }
    }

    let mut markers = Vec::new();
    for f in &findings {
        let Some(a) = f["anchor"].as_array() else {
            continue;
        };
        if a.len() != 3 {
            continue;
        }
        let severity = match f["severity"].as_str().unwrap_or("note") {
            "blocker" => 2.0,
            "warning" => 0.0,
            _ => 1.0,
        };
        markers.push(Marker {
            center: [
                a[0].as_f64().unwrap_or(0.0) as f32 - centre[0],
                a[1].as_f64().unwrap_or(0.0) as f32 - centre[1],
                a[2].as_f64().unwrap_or(0.0) as f32 - centre[2],
            ],
            severity,
            rule: f["rule"].as_str().unwrap_or("finding").to_string(),
            message: f["message"].as_str().unwrap_or("").to_string(),
            measured: f["measured"].as_f64().map(|v| v as f32),
            limit: f["limit"].as_f64().map(|v| v as f32),
            pos: [0.0, 0.0],
        });
    }

    Ok(Scene3d {
        name: doc["part"].as_str().unwrap_or("part").to_string(),
        verts,
        indices,
        markers,
        centre,
        radius: radius.max(1e-3),
        triangles: assign.len(),
        highlighted,
    })
}

// ---------------------------------------------------------------- camera

#[derive(Clone, Copy)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    dist: f32,
}

impl Orbit {
    fn for_scene(scene: &Scene3d) -> Self {
        Self {
            yaw: 0.62,
            pitch: 0.42,
            dist: scene.radius * 3.1,
        }
    }

    fn eye(&self) -> [f32; 3] {
        let (cp, sp) = (self.pitch.cos(), self.pitch.sin());
        [
            self.dist * cp * self.yaw.sin(),
            self.dist * sp,
            self.dist * cp * self.yaw.cos(),
        ]
    }

    fn basis(&self) -> ([f32; 3], [f32; 3]) {
        let eye = self.eye();
        let fwd = normalize([-eye[0], -eye[1], -eye[2]]);
        let right = normalize(cross(fwd, [0.0, 1.0, 0.0]));
        let up = cross(right, fwd);
        (right, up)
    }

    fn view_proj(&self, aspect: f32) -> [[f32; 4]; 4] {
        let proj = perspective(38.0f32.to_radians(), aspect, 0.1, 20000.0);
        let view = look_at(self.eye(), [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        mul(proj, view)
    }
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
    [v[0] / l, v[1] / l, v[2] / l]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fovy * 0.5).tan();
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), -1.0],
        [0.0, 0.0, (near * far) / (near - far), 0.0],
    ]
}

fn look_at(eye: [f32; 3], centre: [f32; 3], up: [f32; 3]) -> [[f32; 4]; 4] {
    let f = normalize([centre[0] - eye[0], centre[1] - eye[1], centre[2] - eye[2]]);
    let s = normalize(cross(f, up));
    let u = cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [
            -(s[0] * eye[0] + s[1] * eye[1] + s[2] * eye[2]),
            -(u[0] * eye[0] + u[1] * eye[1] + u[2] * eye[2]),
            f[0] * eye[0] + f[1] * eye[1] + f[2] * eye[2],
            1.0,
        ],
    ]
}

/// World point to pixel coordinates, matching the shader's clip-space maths.
fn project(m: &[[f32; 4]; 4], p: [f32; 3], w: f32, h: f32) -> [f32; 2] {
    let v = [p[0], p[1], p[2], 1.0];
    let clip =
        |col: usize| m[col][0] * v[0] + m[col][1] * v[1] + m[col][2] * v[2] + m[col][3] * v[3];
    let (x, y, ww) = (clip(0), clip(1), clip(3));
    if ww.abs() < 1e-6 {
        return [-1e6, -1e6];
    }
    [(x / ww * 0.5 + 0.5) * w, (1.0 - (y / ww * 0.5 + 0.5)) * h]
}

fn mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for row in 0..4 {
        for col in 0..4 {
            out[col][row] = (0..4).map(|k| a[k][row] * b[col][k]).sum();
        }
    }
    out
}

// ---------------------------------------------------------------- renderer

const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    globals: wgpu::Buffer,
    bind: wgpu::BindGroup,
    mesh_pipeline: wgpu::RenderPipeline,
    marker_pipeline: wgpu::RenderPipeline,
    mesh_verts: wgpu::Buffer,
    mesh_index: wgpu::Buffer,
    index_count: u32,
    markers: Vec<Marker>,
    overlay: Overlay,
}

#[allow(dead_code)]
struct DrawRange {
    start: usize,
    count: usize,
}

impl Renderer {
    fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        scene: &Scene3d,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("soil-dfm shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-dfm globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("soil-dfm globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("soil-dfm globals bind"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("soil-dfm pipeline layout"),
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });

        let mesh_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("soil-dfm mesh"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_mesh"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<MeshVert>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_mesh"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // Markers ignore depth so a pin is never swallowed by the part itself.
        let marker_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("soil-dfm markers"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_marker"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<MarkVert>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3, 1 => Float32x2, 2 => Float32, 3 => Float32
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_marker"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let mesh_verts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-dfm vertices"),
            size: (scene.verts.len() * std::mem::size_of::<MeshVert>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&mesh_verts, 0, bytemuck::cast_slice(&scene.verts));
        let mesh_index = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-dfm indices"),
            size: (scene.indices.len() * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&mesh_index, 0, bytemuck::cast_slice(&scene.indices));

        let overlay = Overlay::new(&device, &queue, format);

        Self {
            device,
            queue,
            globals,
            bind,
            mesh_pipeline,
            marker_pipeline,
            mesh_verts,
            mesh_index,
            index_count: scene.indices.len() as u32,
            markers: scene
                .markers
                .iter()
                .map(|m| Marker {
                    center: m.center,
                    severity: m.severity,
                    rule: m.rule.clone(),
                    message: m.message.clone(),
                    measured: m.measured,
                    limit: m.limit,
                    pos: [0.0, 0.0],
                })
                .collect(),
            overlay,
        }
    }

    fn write_globals(&self, orbit: &Orbit, aspect: f32) {
        let (right, up) = orbit.basis();
        let eye = orbit.eye();
        let g = Globals {
            view_proj: orbit.view_proj(aspect),
            cam_right: [right[0], right[1], right[2], 0.0],
            cam_up: [up[0], up[1], up[2], 0.0],
            eye: [eye[0], eye[1], eye[2], 1.0],
            light: [0.45, 0.75, 0.5, 0.0],
            base: [0.30, 0.33, 0.38, 1.0],
            highlight: [1.0, 0.62, 0.10, 1.0],
        };
        self.queue
            .write_buffer(&self.globals, 0, bytemuck::bytes_of(&g));
    }

    fn marker_buffer(&self, active: Option<usize>) -> (wgpu::Buffer, u32) {
        let corners = [
            [-1.0f32, -1.0],
            [1.0, -1.0],
            [1.0, 1.0],
            [-1.0, -1.0],
            [1.0, 1.0],
            [-1.0, 1.0],
        ];
        let mut verts = Vec::with_capacity(self.markers.len() * 6);
        for (i, m) in self.markers.iter().enumerate() {
            let hot = if active == Some(i) { 1.0 } else { 0.0 };
            for c in corners {
                verts.push(MarkVert {
                    center: m.center,
                    corner: c,
                    tint: m.severity,
                    active: hot,
                    _pad: 0.0,
                });
            }
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-dfm marker verts"),
            size: (verts.len() * std::mem::size_of::<MarkVert>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&buffer, 0, bytemuck::cast_slice(&verts));
        (buffer, verts.len() as u32)
    }

    fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        colour: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        size: (u32, u32),
        active: Option<usize>,
    ) {
        let (marker_buffer, marker_count) = self.marker_buffer(active);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("soil-dfm pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: colour,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.051,
                        g: 0.063,
                        b: 0.075,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_pipeline(&self.mesh_pipeline);
        pass.set_vertex_buffer(0, self.mesh_verts.slice(..));
        pass.set_index_buffer(self.mesh_index.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.index_count, 0, 0..1);

        if marker_count > 0 {
            pass.set_pipeline(&self.marker_pipeline);
            pass.set_vertex_buffer(0, marker_buffer.slice(..));
            pass.draw(0..marker_count, 0..1);
        }
        drop(pass);

        if let Some(index) = active
            && index < self.markers.len()
        {
            let verts = self
                .overlay
                .build_card(size.0 as f32, size.1 as f32, &self.markers, index);
            let count = self.overlay.upload(&self.device, &self.queue, &verts);
            self.overlay
                .queue_screen(&self.queue, size.0 as f32, size.1 as f32);
            let mut ui_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("soil-dfm overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: colour,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            ui_pass.set_pipeline(&self.overlay.pipeline);
            ui_pass.set_bind_group(0, &self.overlay.bind, &[]);
            ui_pass.set_bind_group(1, &self.overlay.screen_bind, &[]);
            ui_pass.set_vertex_buffer(0, self.overlay.buffer.slice(..));
            ui_pass.draw(0..count, 0..1);
        }
    }

    /// Projection of each marker to screen space, for picking and cards.
    fn place_markers(&mut self, orbit: &Orbit, size: (u32, u32)) {
        let vp = orbit.view_proj(size.0 as f32 / size.1.max(1) as f32);
        for m in &mut self.markers {
            m.pos = project(&vp, m.center, size.0 as f32, size.1 as f32);
        }
    }
}

fn depth_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("soil-dfm depth"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

// ---------------------------------------------------------------- screenshot

fn write_bmp(path: &str, rgba: &[u8], width: u32, height: u32) -> Result<()> {
    // 24-bit BGR, bottom-up. Dependency-free on purpose: the renderer should not
    // need an image crate just so a render can be checked.
    let row = (width * 3).div_ceil(4) * 4;
    let pixel_bytes = row * height;
    let mut out = Vec::with_capacity(54 + pixel_bytes as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + pixel_bytes).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&pixel_bytes.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..height).rev() {
        let start = (y * width * 4) as usize;
        for x in 0..width as usize {
            let p = start + x * 4;
            out.push(rgba[p + 2]);
            out.push(rgba[p + 1]);
            out.push(rgba[p]);
        }
        // One padding value, not a virtual Vec of them; `same_item_push`
        // only objects to the literal-zero variant of the macro.
        out.extend(std::iter::repeat_n(0u8, (row - width * 3) as usize));
    }
    std::fs::write(path, out).with_context(|| format!("write {path}"))?;
    Ok(())
}

fn screenshot(scene: &Scene3d, path: &str, width: u32, height: u32, orbit: &Orbit) -> Result<()> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("soil-dfm-viz"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
        experimental_features: Default::default(),
    }))?;

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("soil-dfm target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = depth_texture(&device, width, height);

    let mut renderer = Renderer::new(device.clone(), queue.clone(), format, scene);
    renderer.write_globals(orbit, width as f32 / height as f32);
    renderer.place_markers(orbit, (width, height));
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("soil-dfm screenshot"),
    });
    renderer.encode(&mut encoder, &view, &depth, (width, height), Some(0));

    let unpadded = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("soil-dfm readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    receiver.recv()??;

    let data = slice.get_mapped_range();
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let start = (y * padded) as usize;
        rgba.extend_from_slice(&data[start..start + unpadded as usize]);
    }
    drop(data);
    readback.unmap();

    write_bmp(path, &rgba, width, height)?;
    Ok(())
}

// ---------------------------------------------------------------- window

struct App {
    scene: Arc<Scene3d>,
    orbit: Orbit,
    active: Option<usize>,
    state: Option<WindowState>,
}

struct WindowState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    depth: wgpu::TextureView,
    dragging: bool,
    last: (f64, f64),
    #[allow(dead_code)]
    started: Instant,
}

impl App {
    fn new(scene: Scene3d) -> Self {
        let orbit = Orbit::for_scene(&scene);
        Self {
            scene: Arc::new(scene),
            orbit,
            active: Some(0),
            state: None,
        }
    }

    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let attrs = Window::default_attributes()
            .with_title(format!("soil-dfm — {}", self.scene.name))
            .with_inner_size(PhysicalSize::new(1280, 900));
        let window = Arc::new(event_loop.create_window(attrs)?);
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("soil-dfm-viz"),
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
        let size = window.inner_size();
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
        let renderer = Renderer::new(device, queue, format, &self.scene);
        let depth = depth_texture(&renderer.device, config.width, config.height);
        self.state = Some(WindowState {
            window,
            surface,
            config,
            renderer,
            depth,
            dragging: false,
            last: (0.0, 0.0),
            started: Instant::now(),
        });
        Ok(())
    }

    fn redraw(&mut self) {
        let Some(s) = self.state.as_mut() else { return };
        let frame = match s.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(_) => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        s.renderer
            .write_globals(&self.orbit, s.config.width as f32 / s.config.height as f32);
        s.renderer
            .place_markers(&self.orbit, (s.config.width, s.config.height));
        let mut encoder =
            s.renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("soil-dfm frame"),
                });
        s.renderer.encode(
            &mut encoder,
            &view,
            &s.depth,
            (s.config.width, s.config.height),
            self.active,
        );
        s.renderer.queue.submit([encoder.finish()]);
        frame.present();
        s.window.request_redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_none()
            && let Err(error) = self.init(event_loop)
        {
            eprintln!("soil-dfm: {error:#}");
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(s) = self.state.as_mut()
                    && size.width > 0
                    && size.height > 0
                {
                    s.config.width = size.width;
                    s.config.height = size.height;
                    s.surface.configure(&s.renderer.device, &s.config);
                    s.depth = depth_texture(&s.renderer.device, size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape)
                {
                    event_loop.exit();
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(s) = self.state.as_mut() {
                    s.dragging = state == ElementState::Pressed;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(s) = self.state.as_mut() {
                    if s.dragging {
                        let dx = (position.x - s.last.0) as f32;
                        let dy = (position.y - s.last.1) as f32;
                        self.orbit.yaw -= dx * 0.008;
                        self.orbit.pitch = (self.orbit.pitch + dy * 0.008).clamp(-1.45, 1.45);
                    }
                    s.last = (position.x, position.y);
                    // Pick the nearest marker within a comfortable radius.
                    let mut best: Option<(usize, f32)> = None;
                    for (i, m) in s.renderer.markers.iter().enumerate() {
                        let dx = m.pos[0] - position.x as f32;
                        let dy = m.pos[1] - position.y as f32;
                        let d = (dx * dx + dy * dy).sqrt();
                        if d < 26.0 && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                            best = Some((i, d));
                        }
                    }
                    if let Some((i, _)) = best {
                        self.active = Some(i);
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let step = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 0.1,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 * 0.002,
                };
                let floor = self.scene.radius * 0.6;
                let ceiling = self.scene.radius * 9.0;
                self.orbit.dist = (self.orbit.dist * (1.0 - step)).clamp(floor, ceiling);
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(s) = self.state.as_ref() {
            s.window.request_redraw();
        }
    }
}

// ---------------------------------------------------------------- main

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut scene_path = "web/spores/dfm/data.json".to_string();
    let mut shot: Option<String> = None;
    let mut size = (1280u32, 900u32);
    let mut orbit_override: Option<Orbit> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scene" => scene_path = args.next().context("--scene needs a path")?,
            "--screenshot" => shot = Some(args.next().context("--screenshot needs a path")?),
            "--size" => {
                let raw = args.next().context("--size needs WxH")?;
                let (w, h) = raw.split_once('x').context("--size wants WxH")?;
                size = (w.parse()?, h.parse()?);
            }
            "--yaw" => {
                let v = args.next().context("--yaw needs a value")?.parse()?;
                let orbit = orbit_override.get_or_insert(Orbit {
                    yaw: 0.0,
                    pitch: 0.0,
                    dist: 0.0,
                });
                orbit.yaw = v;
            }
            "-h" | "--help" => {
                println!(
                    "soil-dfm-viz — native wgpu view of a DFM-annotated model\n\n\
                     --scene <data.json>     mesh + patch assignment + findings\n\
                     --screenshot <out.bmp>  render offscreen instead of opening a window\n\
                     --size <WxH>            frame size (default 1280x900)\n\
                     --yaw <rad>             override the default orbit"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other:?}"),
        }
    }

    let scene = load_scene(&scene_path)?;
    eprintln!(
        "soil-dfm: {} — {} triangles, {} highlighted, {} marker(s)",
        scene.name,
        scene.triangles,
        scene.highlighted,
        scene.markers.len()
    );
    for m in &scene.markers {
        eprintln!("  marker {} (severity {:.0})", m.rule, m.severity);
    }

    if let Some(path) = shot {
        let mut orbit = Orbit::for_scene(&scene);
        if let Some(o) = orbit_override {
            orbit.yaw = o.yaw;
        }
        screenshot(&scene, &path, size.0, size.1, &orbit)?;
        eprintln!("wrote {path}");
        return Ok(());
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(scene);
    event_loop.run_app(&mut app)?;
    Ok(())
}

// ---------------------------------------------------------------- text overlay
//
// A DFM marker without its finding is just a dot. This draws the finding as a
// card: a rasterised glyph atlas from a system font, one quad per glyph, and a
// pair of solid quads for the panel behind it. It is deliberately small — the
// host supplies the words, this only places them.

const FONT_CANDIDATES: [&str; 4] = [
    "/System/Library/Fonts/Supplemental/Andale Mono.ttf",
    "/System/Library/Fonts/Supplemental/Courier New.ttf",
    "/Library/Fonts/Arial.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
];

const ATLAS: u32 = 1024;

#[derive(Clone, Copy)]
struct Glyph {
    uv: [f32; 4],
    size: [f32; 2],
    offset: [f32; 2],
    advance: f32,
}

struct Font {
    glyphs: Vec<Option<Glyph>>, // indexed by codepoint, ASCII only
    px: f32,
}

impl Font {
    fn load(px: f32) -> Option<(Self, Vec<u8>)> {
        let bytes = FONT_CANDIDATES.iter().find_map(|p| std::fs::read(p).ok())?;
        let font = fontdue::Font::from_bytes(
            bytes,
            fontdue::FontSettings {
                scale: px,
                ..Default::default()
            },
        )
        .ok()?;

        let mut atlas = vec![0u8; (ATLAS * ATLAS) as usize];
        let mut glyphs = vec![None; 128];
        let mut cursor = (0u32, 0u32, 0u32); // x, y, row height
        for code in 32u8..127 {
            let ch = code as char;
            let (metrics, bitmap) = font.rasterize(ch, px);
            if metrics.width == 0 || metrics.height == 0 {
                glyphs[code as usize] = Some(Glyph {
                    uv: [0.0; 4],
                    size: [0.0, 0.0],
                    offset: [0.0, 0.0],
                    advance: metrics.advance_width,
                });
                continue;
            }
            if cursor.0 + metrics.width as u32 + 1 >= ATLAS {
                cursor.0 = 0;
                cursor.1 += cursor.2 + 1;
                cursor.2 = 0;
            }
            if cursor.1 + metrics.height as u32 + 1 >= ATLAS {
                return None; // atlas overflow: refuse rather than clip glyphs
            }
            for row in 0..metrics.height {
                let dst = ((cursor.1 + row as u32) * ATLAS + cursor.0) as usize;
                let src = row * metrics.width;
                atlas[dst..dst + metrics.width].copy_from_slice(&bitmap[src..src + metrics.width]);
            }
            let inv = 1.0 / ATLAS as f32;
            glyphs[code as usize] = Some(Glyph {
                uv: [
                    cursor.0 as f32 * inv,
                    cursor.1 as f32 * inv,
                    (cursor.0 + metrics.width as u32) as f32 * inv,
                    (cursor.1 + metrics.height as u32) as f32 * inv,
                ],
                size: [metrics.width as f32, metrics.height as f32],
                offset: [metrics.xmin as f32, metrics.ymin as f32],
                advance: metrics.advance_width,
            });
            cursor.0 += metrics.width as u32 + 1;
            cursor.2 = cursor.2.max(metrics.height as u32);
        }
        Some((Self { glyphs, px }, atlas))
    }

    fn width(&self, text: &str) -> f32 {
        text.chars()
            .map(|c| {
                self.glyphs
                    .get(c as usize)
                    .and_then(|g| g.as_ref())
                    .map(|g| g.advance)
                    .unwrap_or(self.px * 0.5)
            })
            .sum()
    }
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct UiVert {
    pos: [f32; 2],
    uv: [f32; 2],
    colour: [f32; 4],
}

const UI_SHADER: &str = r#"
@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VIn {
  @location(0) pos: vec2<f32>,
  @location(1) uv: vec2<f32>,
  @location(2) colour: vec4<f32>,
};
struct VOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) colour: vec4<f32>,
};
struct Screen { size: vec4<f32> };
@group(1) @binding(0) var<uniform> screen: Screen;

@vertex
fn vs_ui(v: VIn) -> VOut {
  var o: VOut;
  // Pixel space, top-left origin, to clip space.
  let ndc = vec2<f32>(v.pos.x / screen.size.x * 2.0 - 1.0,
                      1.0 - v.pos.y / screen.size.y * 2.0);
  o.clip = vec4<f32>(ndc, 0.0, 1.0);
  o.uv = v.uv;
  o.colour = v.colour;
  return o;
}

@fragment
fn fs_ui(i: VOut) -> @location(0) vec4<f32> {
  let a = textureSample(atlas, samp, i.uv).r;
  return vec4<f32>(i.colour.rgb, i.colour.a * a);
}
"#;

struct Overlay {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    screen: wgpu::Buffer,
    screen_bind: wgpu::BindGroup,
    font: Option<Font>,
    buffer: wgpu::Buffer,
    capacity: usize,
}

impl Overlay {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("soil-dfm ui shader"),
            source: wgpu::ShaderSource::Wgsl(UI_SHADER.into()),
        });

        let (font, atlas) = match Font::load(13.0) {
            Some((font, mut atlas)) => {
                // 2x2 white block in the last corner, for solid fills.
                for dy in 0..2u32 {
                    for dx in 0..2u32 {
                        let at = ((ATLAS - 1 - dx) * ATLAS + (ATLAS - 1 - dy)) as usize;
                        atlas[at] = 255;
                    }
                }
                (Some(font), atlas)
            }
            None => {
                eprintln!("soil-dfm: no system font found; cards will draw without text");
                (None, vec![0u8; (ATLAS * ATLAS) as usize])
            }
        };

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("soil-dfm glyphs"),
            size: wgpu::Extent3d {
                width: ATLAS,
                height: ATLAS,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS),
                rows_per_image: Some(ATLAS),
            },
            wgpu::Extent3d {
                width: ATLAS,
                height: ATLAS,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("soil-dfm glyph sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("soil-dfm ui atlas layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("soil-dfm ui atlas bind"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let screen = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-dfm screen"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let screen_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("soil-dfm screen layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let screen_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("soil-dfm screen bind"),
            layout: &screen_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen.as_entire_binding(),
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("soil-dfm ui pipeline layout"),
            bind_group_layouts: &[&atlas_layout, &screen_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("soil-dfm ui"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_ui"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UiVert>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_ui"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let capacity = 4096;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-dfm ui verts"),
            size: (capacity * std::mem::size_of::<UiVert>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind,
            screen,
            screen_bind,
            font,
            buffer,
            capacity,
        }
    }

    fn white_uv() -> [f32; 2] {
        let c = (ATLAS as f32 - 1.5) / ATLAS as f32;
        [c, c]
    }

    fn quad(v: &mut Vec<UiVert>, x: f32, y: f32, w: f32, h: f32, uv: [f32; 4], colour: [f32; 4]) {
        let white = Self::white_uv();
        let uv_of = |corner: usize| -> [f32; 2] {
            if uv[2] > uv[0] {
                match corner {
                    0 => [uv[0], uv[1]],
                    1 => [uv[2], uv[1]],
                    2 => [uv[2], uv[3]],
                    _ => [uv[0], uv[3]],
                }
            } else {
                white
            }
        };
        let corners = [[x, y], [x + w, y], [x + w, y + h], [x, y + h]];
        for i in [0usize, 1, 2, 0, 2, 3] {
            v.push(UiVert {
                pos: corners[i],
                uv: uv_of(i),
                colour,
            });
        }
    }

    fn text(&self, out: &mut Vec<UiVert>, x: f32, y: f32, text: &str, colour: [f32; 4]) -> f32 {
        let Some(font) = self.font.as_ref() else {
            return x;
        };
        let mut pen = x;
        for ch in text.chars() {
            let Some(Some(g)) = font.glyphs.get(ch as usize) else {
                pen += font.px * 0.5;
                continue;
            };
            if g.size[0] > 0.0 {
                Self::quad(
                    out,
                    pen + g.offset[0],
                    y - g.size[1] - g.offset[1],
                    g.size[0],
                    g.size[1],
                    g.uv,
                    colour,
                );
            }
            pen += g.advance;
        }
        pen
    }

    fn wrap(&self, text: &str, max_px: f32) -> Vec<String> {
        let measure = |t: &str| {
            self.font
                .as_ref()
                .map(|f| f.width(t))
                .unwrap_or(t.len() as f32 * 7.0)
        };
        let mut lines: Vec<String> = Vec::new();
        let mut line = String::new();
        for word in text.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && measure(&candidate) > max_px {
                lines.push(std::mem::take(&mut line));
                line.push_str(word);
            } else {
                line = candidate;
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
        lines
    }

    fn build_card(&self, w: f32, h: f32, markers: &[Marker], index: usize) -> Vec<UiVert> {
        let mut v = Vec::new();
        let marker = &markers[index];
        let width = 340.0f32;
        let pad = 12.0;
        let body = self.wrap(&marker.message, width - pad * 2.0 - 6.0);
        let has_value = marker.measured.is_some() && marker.limit.is_some();
        let height = 66.0 + body.len() as f32 * 16.0 + if has_value { 20.0 } else { 0.0 };

        let mut x = marker.pos[0] + 22.0;
        if x + width > w - 12.0 {
            x = marker.pos[0] - width - 22.0;
        }
        let x = x.max(12.0);
        let y = (marker.pos[1] - 18.0).clamp(12.0, (h - height - 12.0).max(12.0));

        // panel + accent bar
        Self::quad(
            &mut v,
            x,
            y,
            width,
            height,
            [0.0; 4],
            [0.055, 0.067, 0.078, 0.94],
        );
        let accent = match marker.severity {
            2.0 => [0.90, 0.28, 0.30, 1.0],
            0.0 => [1.0, 0.64, 0.17, 1.0],
            _ => [0.42, 0.66, 1.0, 1.0],
        };
        Self::quad(&mut v, x, y, 3.0, height, [0.0; 4], accent);

        let mut cursor = y + pad + 11.0;
        self.text(
            &mut v,
            x + pad,
            cursor,
            &marker.rule.to_uppercase(),
            [0.92, 0.95, 0.98, 1.0],
        );
        cursor += 17.0;
        let sev = match marker.severity {
            2.0 => "BLOCKING",
            0.0 => "WARNING",
            _ => "NOTE",
        };
        self.text(&mut v, x + pad, cursor, sev, accent);
        cursor += 18.0;
        for line in &body {
            self.text(&mut v, x + pad, cursor, line, [0.72, 0.78, 0.85, 1.0]);
            cursor += 16.0;
        }
        if let (Some(measured), Some(limit)) = (marker.measured, marker.limit) {
            let line = format!("{measured:.3} VS {limit:.3} MM");
            self.text(
                &mut v,
                x + pad,
                y + height - pad,
                &line,
                [0.86, 0.90, 0.95, 1.0],
            );
        }
        v
    }

    fn queue_screen(&self, queue: &wgpu::Queue, w: f32, h: f32) {
        queue.write_buffer(&self.screen, 0, bytemuck::cast_slice(&[w, h, 0.0, 0.0]));
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, verts: &[UiVert]) -> u32 {
        if verts.len() > self.capacity {
            self.capacity = verts.len().next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("soil-dfm ui verts"),
                size: (self.capacity * std::mem::size_of::<UiVert>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !verts.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(verts));
        }
        verts.len() as u32
    }
}
