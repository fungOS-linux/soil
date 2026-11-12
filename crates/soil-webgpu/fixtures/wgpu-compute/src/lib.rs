//! A real wgpu 29 program compiled to `wasm32-wasip2`.
//!
//! It uses `wgpu-wasi-webgpu::instance()` and ordinary wgpu calls; the
//! component imports `wasi:webgpu@0.3.0-rc.2` and runs in Soil.

wit_bindgen::generate!({
    path: "wit",
    world: "runner",
});

use wgpu::util::DeviceExt;

const SHADER: &str = r#"
@group(0) @binding(0)
var<storage, read_write> numbers: array<u32>;

fn collatz_steps(start: u32) -> u32 {
    var n = start;
    var steps = 0u;
    loop {
        if n <= 1u { break; }
        if n % 2u == 0u { n = n / 2u; } else {
            if n >= 1431655765u { return 4294967295u; }
            n = 3u * n + 1u;
        }
        steps = steps + 1u;
    }
    return steps;
}

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    numbers[id.x] = collatz_steps(numbers[id.x]);
}
"#;

fn collatz(numbers: &[u32]) -> Result<Vec<u32>, String> {
    let instance = wgpu_wasi_webgpu::instance();
    let adapter = wit_bindgen::block_on(instance.request_adapter(&Default::default()))
        .map_err(|e| format!("request-adapter: {e}"))?;
    let (device, queue) = wit_bindgen::block_on(adapter.request_device(&Default::default()))
        .map_err(|e| format!("request-device: {e}"))?;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("collatz"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let bytes: Vec<u8> = numbers.iter().flat_map(|n| n.to_le_bytes()).collect();
    let size = bytes.len() as u64;
    let storage = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("numbers"),
        contents: &bytes,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("steps"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("collatz"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("numbers"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: storage.as_entire_binding(),
        }],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("collatz"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(numbers.len() as u32, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&storage, 0, &staging, 0, size);
    queue.submit([encoder.finish()]);
    staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("poll: {e}"))?;
    let steps = staging
        .slice(..)
        .get_mapped_range()
        .map_err(|e| format!("map: {e}"))?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    staging.unmap();
    Ok(steps)
}

struct Component;

impl Guest for Component {
    async fn run() -> u32 {
        match collatz(&[1, 2, 3, 4]) {
            Ok(steps) if steps == [0, 1, 7, 2] => 0,
            _ => 1,
        }
    }
}

export!(Component);
