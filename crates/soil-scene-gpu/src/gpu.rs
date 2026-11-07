//! GPU-resident dense and 3D paths.
//!
//! These are deliberately outside the serialized scene document. `soil_scene`
//! carries a small resource reference and the style; this module owns the
//! upload, the compute decimation pass, and the depth-tested 3D draw.

use std::collections::HashMap;
use std::num::NonZeroU64;

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use soil_scene::{Content, DenseResource, GpuDenseMode, GpuResource, PointCloudResource, Scene};

use crate::{Theme, rgb_or_ink, world_bounds};

pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

const DENSE_COMPUTE: &str = r#"
struct Params {
  count: u32,
  buckets: u32,
  dispatch_x: u32,
  pad: u32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> points: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec2<f32>>;

var<workgroup> sums: array<f32, 256>;

@compute @workgroup_size(256)
fn main(
  @builtin(workgroup_id) workgroup: vec3<u32>,
  @builtin(local_invocation_id) local_id: vec3<u32>,
) {
  let bucket = workgroup.x + workgroup.y * params.dispatch_x;
  if (bucket >= params.buckets) {
    return;
  }
  // Avoid u32 overflow in `bucket * count`; keep the product <= count.
  let base = params.count / params.buckets;
  let extra = params.count % params.buckets;
  let start = bucket * base + min(bucket, extra);
  let end = start + base + select(0u, 1u, bucket < extra);
  let local = local_id.x;

  var sum = 0.0;
  for (var i = start + local; i < end; i = i + 256u) {
    sum = sum + points[i].y;
  }
  sums[local] = sum;
  workgroupBarrier();
  var stride = 128u;
  loop {
    if (stride == 0u) { break; }
    if (local < stride) { sums[local] = sums[local] + sums[local + stride]; }
    workgroupBarrier();
    stride = stride / 2u;
  }
  let average = sums[0] / f32(max(end - start, 1u));
  if (local == 0u) {
    let center = (f32(start) + f32(end - 1u)) * 0.5 / f32(max(params.count - 1u, 1u));
    output[bucket] = vec2<f32>(center, average);
  }
}
"#;

const DENSE_LINE: &str = r#"
struct Draw {
  rect: vec4<f32>,
  range: vec4<f32>,
  color: vec4<f32>,
  viewport: vec2<f32>,
  size: f32,
  pad: f32,
};
@group(0) @binding(0) var<uniform> draw: Draw;

@vertex
fn vs_main(@location(0) point: vec2<f32>) -> @builtin(position) vec4<f32> {
  let uv = clamp((point - draw.range.xy) / (draw.range.zw - draw.range.xy), vec2<f32>(0.0), vec2<f32>(1.0));
  return vec4<f32>(
    mix(draw.rect.x, draw.rect.z, uv.x),
    mix(draw.rect.y, draw.rect.w, uv.y),
    0.0,
    1.0,
  );
}
@fragment
fn fs_main() -> @location(0) vec4<f32> {
  return draw.color;
}
"#;

const DENSE_SCATTER: &str = r#"
struct Draw {
  rect: vec4<f32>,
  range: vec4<f32>,
  color: vec4<f32>,
  viewport: vec2<f32>,
  size: f32,
  pad: f32,
};
@group(0) @binding(0) var<uniform> draw: Draw;

struct VertexOut {
  @builtin(position) position: vec4<f32>,
  @location(0) local: vec2<f32>,
};

@vertex
fn vs_main(
  @builtin(vertex_index) vertex: u32,
  @location(0) point: vec2<f32>,
) -> VertexOut {
  let corners = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 1.0, -1.0),
    vec2<f32>( 1.0,  1.0),
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 1.0,  1.0),
    vec2<f32>(-1.0,  1.0),
  );
  let corner = corners[vertex];
  let uv = clamp((point - draw.range.xy) / (draw.range.zw - draw.range.xy), vec2<f32>(0.0), vec2<f32>(1.0));
  let center = vec2<f32>(
    mix(draw.rect.x, draw.rect.z, uv.x),
    mix(draw.rect.y, draw.rect.w, uv.y),
  );
  let offset = corner * draw.size * 2.0 / draw.viewport;
  return VertexOut(vec4<f32>(center + offset, 0.0, 1.0), corner);
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
  let distance = length(input.local);
  if (distance > 1.0) { discard; }
  let coverage = 1.0 - smoothstep(0.82, 1.0, distance);
  return vec4<f32>(draw.color.rgb, draw.color.a * coverage);
}
"#;

const POINTS_3D: &str = r#"
struct Camera {
  view_proj: mat4x4<f32>,
  color: vec4<f32>,
  rect: vec4<f32>,
  viewport: vec2<f32>,
  size: f32,
  pad: f32,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexOut {
  @builtin(position) position: vec4<f32>,
  @location(0) local: vec2<f32>,
  @location(1) intensity: f32,
};

@vertex
fn vs_main(
  @builtin(vertex_index) vertex: u32,
  @location(0) point: vec3<f32>,
  @location(1) intensity: f32,
) -> VertexOut {
  let corners = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 1.0, -1.0),
    vec2<f32>( 1.0,  1.0),
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 1.0,  1.0),
    vec2<f32>(-1.0,  1.0),
  );
  let corner = corners[vertex];
  let clip = camera.view_proj * vec4<f32>(point, 1.0);
  let ndc = clip.xy / clip.w;
  let mapped = vec2<f32>(
    mix(camera.rect.x, camera.rect.z, ndc.x * 0.5 + 0.5),
    mix(camera.rect.y, camera.rect.w, 0.5 - ndc.y * 0.5),
  );
  let offset = corner * camera.size * 2.0 / camera.viewport * clip.w;
  return VertexOut(vec4<f32>(mapped * clip.w, clip.z, clip.w) + vec4<f32>(offset, 0.0, 0.0), corner, intensity);
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
  let distance = length(input.local);
  if (distance > 1.0) { discard; }
  let t = clamp(input.intensity, 0.0, 1.0);
  let rgb = mix(camera.color.rgb * 0.38, camera.color.rgb, t);
  let coverage = 1.0 - smoothstep(0.78, 1.0, distance);
  return vec4<f32>(rgb, camera.color.a * coverage);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DenseParams {
    count: u32,
    buckets: u32,
    dispatch_x: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DenseDraw {
    rect: [f32; 4],
    range: [f32; 4],
    color: [f32; 4],
    viewport: [f32; 2],
    size: f32,
    pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraDraw {
    view_proj: [[f32; 4]; 4],
    color: [f32; 4],
    rect: [f32; 4],
    viewport: [f32; 2],
    size: f32,
    pad: f32,
}

struct DenseGpu {
    generation: u64,
    buckets: u32,
    output: wgpu::Buffer,
    draw: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

struct CloudGpu {
    generation: u64,
    count: u32,
    points: wgpu::Buffer,
    draw: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

#[derive(Debug, Clone)]
pub(crate) enum GpuDraw {
    Dense { node: String, scatter: bool },
    Points3D { node: String },
}

pub(crate) struct GpuState {
    compute_layout: wgpu::BindGroupLayout,
    draw_layout: wgpu::BindGroupLayout,
    camera_layout: wgpu::BindGroupLayout,
    dense_compute: wgpu::ComputePipeline,
    dense_line: wgpu::RenderPipeline,
    dense_scatter: wgpu::RenderPipeline,
    points3d: wgpu::RenderPipeline,
    dense: HashMap<String, DenseGpu>,
    clouds: HashMap<String, CloudGpu>,
}

impl GpuState {
    pub(crate) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> Result<Self> {
        let compute_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("soil-gpu dense compute layout"),
            entries: &[
                uniform_entry(
                    0,
                    wgpu::ShaderStages::COMPUTE,
                    std::mem::size_of::<DenseParams>(),
                ),
                storage_entry(1, wgpu::ShaderStages::COMPUTE, true),
                storage_entry(2, wgpu::ShaderStages::COMPUTE, false),
            ],
        });
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("soil-gpu dense draw layout"),
            entries: &[uniform_entry(
                0,
                wgpu::ShaderStages::VERTEX_FRAGMENT,
                std::mem::size_of::<DenseDraw>(),
            )],
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("soil-gpu 3d layout"),
            entries: &[uniform_entry(
                0,
                wgpu::ShaderStages::VERTEX_FRAGMENT,
                std::mem::size_of::<CameraDraw>(),
            )],
        });

        let compute_module = shader(device, "soil-gpu dense compute", DENSE_COMPUTE);
        let compute_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("soil-gpu dense compute"),
                bind_group_layouts: &[&compute_layout],
                push_constant_ranges: &[],
            });
        let dense_compute = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("soil-gpu dense compute"),
            layout: Some(&compute_pipeline_layout),
            module: &compute_module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let dense_line_module = shader(device, "soil-gpu dense line", DENSE_LINE);
        let dense_scatter_module = shader(device, "soil-gpu dense scatter", DENSE_SCATTER);
        let points_module = shader(device, "soil-gpu points3d", POINTS_3D);
        let draw_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("soil-gpu dense draw"),
            bind_group_layouts: &[&draw_layout],
            push_constant_ranges: &[],
        });
        let camera_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("soil-gpu 3d"),
                bind_group_layouts: &[&camera_layout],
                push_constant_ranges: &[],
            });

        let dense_line = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("soil-gpu dense line"),
            layout: Some(&draw_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &dense_line_module,
                entry_point: Some("vs_main"),
                buffers: &[dense_vertex_layout(wgpu::VertexStepMode::Vertex)],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &dense_line_module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineStrip,
                ..Default::default()
            },
            depth_stencil: Some(depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview: None,
            cache: None,
        });

        let dense_scatter = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("soil-gpu dense scatter"),
            layout: Some(&draw_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &dense_scatter_module,
                entry_point: Some("vs_main"),
                buffers: &[dense_vertex_layout(wgpu::VertexStepMode::Instance)],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &dense_scatter_module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview: None,
            cache: None,
        });

        let points3d = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("soil-gpu points3d"),
            layout: Some(&camera_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &points_module,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<[f32; 4]>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &points_module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview: None,
            cache: None,
        });

        Ok(Self {
            compute_layout,
            draw_layout,
            camera_layout,
            dense_compute,
            dense_line,
            dense_scatter,
            points3d,
            dense: HashMap::new(),
            clouds: HashMap::new(),
        })
    }

    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        width: u32,
        height: u32,
        theme: Theme,
    ) -> Result<Vec<GpuDraw>> {
        let mut draws = Vec::new();
        for node in &scene.nodes {
            let (x, y, w, h) = world_bounds(node, &scene.nodes);
            // The painter works in a -4..4 design space; GPU clip space is
            // -1..1. NDC y is up, so the rect is bottom -> top.
            let rect = [x / 4.0, (y - h) / 4.0, (x + w) / 4.0, y / 4.0];
            match &node.content {
                Content::GpuDense(content) => {
                    let resource = scene
                        .gpu
                        .iter()
                        .find(|resource| resource.id() == content.resource)
                        .with_context(|| {
                            format!("node {:?} references missing GPU resource", node.id)
                        })?;
                    let GpuResource::Dense(resource) = resource.as_ref() else {
                        anyhow::bail!("node {:?} references a non-dense GPU resource", node.id);
                    };
                    self.ensure_dense(device, queue, &node.id, content, resource)?;
                    if let Some(cache) = self.dense.get_mut(&node.id) {
                        let color = rgb_or_ink(theme, content.rgb, content.ink);
                        let uniform = DenseDraw {
                            rect,
                            range: content.range,
                            color: rgba(color, content.opacity),
                            viewport: [width.max(1) as f32, height.max(1) as f32],
                            size: content.size,
                            pad: 0.0,
                        };
                        queue.write_buffer(&cache.draw, 0, bytemuck::bytes_of(&uniform));
                    }
                    draws.push(GpuDraw::Dense {
                        node: node.id.clone(),
                        scatter: content.mode == GpuDenseMode::Scatter,
                    });
                }
                Content::GpuPoints3D(content) => {
                    let resource = scene
                        .gpu
                        .iter()
                        .find(|resource| resource.id() == content.resource)
                        .with_context(|| {
                            format!("node {:?} references missing GPU resource", node.id)
                        })?;
                    let GpuResource::PointCloud(resource) = resource.as_ref() else {
                        anyhow::bail!("node {:?} references a non-point-cloud resource", node.id);
                    };
                    self.ensure_cloud(device, queue, &node.id, resource)?;
                    if let Some(cache) = self.clouds.get_mut(&node.id) {
                        let pixel_w = (w / 8.0 * width.max(1) as f32).abs().max(1.0);
                        let pixel_h = (h / 8.0 * height.max(1) as f32).abs().max(1.0);
                        let view_proj = camera_view_proj(
                            content.yaw,
                            content.pitch,
                            content.zoom,
                            pixel_w / pixel_h,
                        );
                        let color = rgb_or_ink(theme, content.rgb, content.ink);
                        let uniform = CameraDraw {
                            view_proj,
                            color: rgba(color, content.opacity),
                            rect,
                            viewport: [pixel_w, pixel_h],
                            size: content.size,
                            pad: 0.0,
                        };
                        queue.write_buffer(&cache.draw, 0, bytemuck::bytes_of(&uniform));
                    }
                    draws.push(GpuDraw::Points3D {
                        node: node.id.clone(),
                    });
                }
                _ => {}
            }
        }
        Ok(draws)
    }

    fn ensure_dense(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        node: &str,
        content: &soil_scene::GpuDense,
        resource: &DenseResource,
    ) -> Result<()> {
        let reusable = self.dense.get(node).is_some_and(|cache| {
            cache.generation == resource.generation && cache.buckets == content.buckets
        });
        if reusable {
            return Ok(());
        }
        let count = resource.points.len();
        anyhow::ensure!(count >= 2, "dense resource needs at least two points");
        let buckets = content.buckets.min(count as u32).max(2);
        let input = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-gpu dense input"),
            size: (count * std::mem::size_of::<[f32; 2]>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&input, 0, bytemuck::cast_slice(&resource.points));
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-gpu dense output"),
            size: (buckets as usize * std::mem::size_of::<[f32; 2]>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX,
            mapped_at_creation: false,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-gpu dense params"),
            size: std::mem::size_of::<DenseParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dispatch_x = buckets.min(65_535);
        let dispatch_y = buckets.div_ceil(dispatch_x);
        let params_value = DenseParams {
            count: count as u32,
            buckets,
            dispatch_x,
            pad: 0,
        };
        queue.write_buffer(&params, 0, bytemuck::bytes_of(&params_value));
        let compute_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("soil-gpu dense compute"),
            layout: &self.compute_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("soil-gpu dense decimation"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("soil-gpu dense decimation"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.dense_compute);
            pass.set_bind_group(0, &compute_bind, &[]);
            pass.dispatch_workgroups(dispatch_x, dispatch_y, 1);
        }
        queue.submit([encoder.finish()]);
        let draw = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-gpu dense draw"),
            size: std::mem::size_of::<DenseDraw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("soil-gpu dense draw"),
            layout: &self.draw_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: draw.as_entire_binding(),
            }],
        });
        self.dense.insert(
            node.to_owned(),
            DenseGpu {
                generation: resource.generation,
                buckets,
                output,
                draw,
                bind_group,
            },
        );
        Ok(())
    }

    fn ensure_cloud(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        node: &str,
        resource: &PointCloudResource,
    ) -> Result<()> {
        let reusable = self
            .clouds
            .get(node)
            .is_some_and(|cache| cache.generation == resource.generation);
        if reusable {
            return Ok(());
        }
        let count = resource.points.len();
        anyhow::ensure!(count > 0, "point cloud resource needs points");
        let points = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-gpu points3d"),
            size: (count * std::mem::size_of::<[f32; 4]>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&points, 0, bytemuck::cast_slice(&resource.points));
        let draw = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("soil-gpu points3d camera"),
            size: std::mem::size_of::<CameraDraw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("soil-gpu points3d camera"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: draw.as_entire_binding(),
            }],
        });
        self.clouds.insert(
            node.to_owned(),
            CloudGpu {
                generation: resource.generation,
                count: count as u32,
                points,
                draw,
                bind_group,
            },
        );
        Ok(())
    }

    pub(crate) fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, draws: &[GpuDraw]) {
        for draw in draws {
            match draw {
                GpuDraw::Dense { node, scatter } => {
                    let Some(cache) = self.dense.get(node) else {
                        continue;
                    };
                    if *scatter {
                        pass.set_pipeline(&self.dense_scatter);
                        pass.set_bind_group(0, &cache.bind_group, &[]);
                        pass.set_vertex_buffer(0, cache.output.slice(..));
                        pass.draw(0..6, 0..cache.buckets);
                    } else {
                        pass.set_pipeline(&self.dense_line);
                        pass.set_bind_group(0, &cache.bind_group, &[]);
                        pass.set_vertex_buffer(0, cache.output.slice(..));
                        pass.draw(0..cache.buckets, 0..1);
                    }
                }
                GpuDraw::Points3D { node } => {
                    let Some(cache) = self.clouds.get(node) else {
                        continue;
                    };
                    pass.set_pipeline(&self.points3d);
                    pass.set_bind_group(0, &cache.bind_group, &[]);
                    pass.set_vertex_buffer(0, cache.points.slice(..));
                    pass.draw(0..6, 0..cache.count);
                }
            }
        }
    }
}

fn shader(device: &wgpu::Device, label: &str, source: &'static str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

fn uniform_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    size: usize,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: NonZeroU64::new(size as u64),
        },
        count: None,
    }
}

fn storage_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn dense_vertex_layout(step_mode: wgpu::VertexStepMode) -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<[f32; 2]>() as u64,
        step_mode,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2],
    }
}

pub(crate) fn depth_state(depth_write: bool) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: depth_write,
        depth_compare: if depth_write {
            wgpu::CompareFunction::Less
        } else {
            wgpu::CompareFunction::Always
        },
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

fn rgba(color: [u8; 3], opacity: f32) -> [f32; 4] {
    [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        opacity.clamp(0.0, 1.0),
    ]
}

/// The same camera matrix used by the GPU 3D pipeline, exposed for projected
/// overlays such as axes and labels.
pub(crate) fn camera_view_proj(yaw: f32, pitch: f32, zoom: f32, aspect: f32) -> [[f32; 4]; 4] {
    let eye = [0.0, 0.0, 3.1 / zoom.max(0.01)];
    let view = look_at(eye, [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    let model = mul(rotation_y(yaw), rotation_x(pitch));
    mul(
        mul(perspective(0.9, aspect.max(0.01), 0.01, 100.0), view),
        model,
    )
}

pub(crate) fn project_to(
    matrix: &[[f32; 4]; 4],
    point: [f32; 3],
    rect: [f32; 4],
) -> Option<(f32, f32)> {
    let clip = transform(matrix, [point[0], point[1], point[2], 1.0]);
    if clip[3].abs() <= f32::EPSILON || clip[3] <= 0.0 {
        return None;
    }
    let ndc = [clip[0] / clip[3], clip[1] / clip[3]];
    Some((
        rect[0] + (ndc[0] * 0.5 + 0.5) * rect[2],
        rect[1] + (0.5 - ndc[1] * 0.5) * rect[3],
    ))
}

fn transform(matrix: &[[f32; 4]; 4], vector: [f32; 4]) -> [f32; 4] {
    let mut out = [0.0; 4];
    for row in 0..4 {
        out[row] = (0..4).map(|col| matrix[col][row] * vector[col]).sum();
    }
    out
}

fn mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            out[col][row] = (0..4).map(|k| a[k][row] * b[col][k]).sum();
        }
    }
    out
}

fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fov_y * 0.5).tan();
    let nf = 1.0 / (near - far);
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far * nf, -1.0],
        [0.0, 0.0, far * near * nf, 0.0],
    ]
}

fn look_at(eye: [f32; 3], center: [f32; 3], up: [f32; 3]) -> [[f32; 4]; 4] {
    let f = normalize(sub(center, eye));
    let s = normalize(cross(f, up));
    let u = cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [-dot(s, eye), -dot(u, eye), dot(f, eye), 1.0],
    ]
}

fn rotation_y(angle: f32) -> [[f32; 4]; 4] {
    let (s, c) = angle.sin_cos();
    [
        [c, 0.0, -s, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [s, 0.0, c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn rotation_x(angle: f32) -> [[f32; 4]; 4] {
    let (s, c) = angle.sin_cos();
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, c, s, 0.0],
        [0.0, -s, c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = dot(v, v).sqrt().max(f32::EPSILON);
    [v[0] / length, v[1] / length, v[2] / length]
}

#[cfg(test)]
mod tests {
    use super::*;
    use soil_scene::{
        Content, DenseResource, GpuDense, GpuDenseMode, GpuResource, Node, Scene, Transform,
    };
    use std::sync::Arc;

    #[test]
    fn gpu_dense_line_stays_inside_its_rect() {
        let points: Vec<[f32; 2]> = (0..10_000)
            .map(|i| {
                let x = i as f32 / 9_999.0;
                [x, 0.5 + 0.4 * x]
            })
            .collect();
        let gpu = vec![Arc::new(GpuResource::Dense(DenseResource {
            id: "dense".into(),
            generation: 1,
            points,
        }))];
        let scene = Scene::with_gpu(
            vec![Node {
                id: "line".into(),
                parent: None,
                transition_ms: None,
                transform: Transform {
                    translation: [-2.0, 1.0, 0.0],
                    rotation: [0.0; 3],
                    scale: [4.0, 2.0, 1.0],
                },
                content: Content::GpuDense(GpuDense {
                    resource: "dense".into(),
                    mode: GpuDenseMode::Line,
                    range: [0.0, 0.0, 1.0, 1.0],
                    buckets: 6000,
                    ink: None,
                    rgb: Some([255, 255, 255]),
                    opacity: 1.0,
                    size: 2.0,
                }),
            }],
            gpu,
        );
        let frame = crate::render(&scene, 400, 200, Theme::default()).expect("render");
        let mut min_y = 999u32;
        let mut max_y = 0u32;
        let mut lit = 0;
        for (i, px) in frame.rgba.as_chunks::<4>().0.iter().enumerate() {
            if px[0] > 180 && px[1] > 180 && px[2] > 180 {
                let y = (i as u32) / 400;
                min_y = min_y.min(y);
                max_y = max_y.max(y);
                lit += 1;
            }
        }
        assert!(lit > 100);
        assert!(
            min_y >= 70 && max_y <= 110,
            "line escaped panel: {min_y}..{max_y}"
        );
    }
}
