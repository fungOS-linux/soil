//! Minimal `wgpu`-shaped workload over `wasi:webgpu`.
//!
//! It exercises adapter/device request, WGSL compilation, storage buffer
//! upload, compute dispatch, copy-to-staging, map/readback, and returns 0 only
//! if the Collatz results are the expected [0, 1, 7, 2].

use wasi::webgpu::webgpu;

wit_bindgen::generate!({
    path: "../../wit",
    world: "runner",
    generate_all,
});

export!(Component);

struct Component;

impl Guest for Component {
    async fn run() -> u32 {
        let numbers = [1u32, 2, 3, 4];
        let expected = [0u32, 1, 7, 2];
        let Some(steps) = execute_gpu(&numbers).await else {
            return 2;
        };
        if steps == expected { 0 } else { 3 }
    }
}

async fn execute_gpu(numbers: &[u32]) -> Option<Vec<u32>> {
    let adapter = webgpu::get_gpu().request_adapter(None).await?;
    let device = adapter.request_device(None).await.ok()?;

    let shader = device.create_shader_module(&webgpu::GpuShaderModuleDescriptor {
        label: Some("collatz".to_string()),
        code: include_str!("shader.wgsl").to_string(),
        compilation_hints: None,
    });

    let bytes: Vec<u8> = numbers.iter().flat_map(|n| n.to_le_bytes()).collect();
    let size = bytes.len() as webgpu::GpuSize64;

    let staging = device.create_buffer(&webgpu::GpuBufferDescriptor {
        label: Some("staging".to_string()),
        size,
        usage: webgpu::GpuBufferUsage::MAP_READ | webgpu::GpuBufferUsage::COPY_DST,
        mapped_at_creation: None,
    });

    let storage = device.create_buffer(&webgpu::GpuBufferDescriptor {
        label: Some("storage".to_string()),
        size,
        usage: webgpu::GpuBufferUsage::STORAGE
            | webgpu::GpuBufferUsage::COPY_DST
            | webgpu::GpuBufferUsage::COPY_SRC,
        mapped_at_creation: Some(true),
    });

    storage
        .get_mapped_range_set_with_copy(&bytes, None, None)
        .ok()?;
    storage.unmap().ok()?;

    let pipeline = device.create_compute_pipeline(webgpu::GpuComputePipelineDescriptor {
        label: Some("collatz".to_string()),
        layout: webgpu::GpuLayoutMode::Auto,
        compute: webgpu::GpuProgrammableStage {
            module: &shader,
            entry_point: Some("main".to_string()),
            constants: None,
        },
    });

    let layout = pipeline.get_bind_group_layout(0);
    let bind_group = device.create_bind_group(&webgpu::GpuBindGroupDescriptor {
        label: Some("collatz".to_string()),
        layout: &layout,
        entries: vec![webgpu::GpuBindGroupEntry {
            binding: 0,
            resource: webgpu::GpuBindingResource::GpuBufferBinding(webgpu::GpuBufferBinding {
                buffer: &storage,
                offset: Some(0),
                size: None,
            }),
        }],
    });

    let encoder =
        device.create_command_encoder(Some(&webgpu::GpuCommandEncoderDescriptor { label: None }));
    {
        let pass = encoder.begin_compute_pass(None);
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, Some(&bind_group), None, None, None)
            .ok()?;
        pass.dispatch_workgroups(numbers.len() as u32, Some(1), Some(1));
        pass.end();
    }
    encoder.copy_buffer_to_buffer(&storage, None, &staging, None, None);
    device.queue().submit(&[&encoder.finish(None)]);

    staging
        .map_async(webgpu::GpuMapMode::READ, Some(0), None)
        .await
        .ok()?;

    let data = staging.get_mapped_range_get_with_copy(None, None).ok()?;
    let result = data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| u32::from_le_bytes(*chunk))
        .collect();
    drop(data);
    staging.unmap().ok()?;
    Some(result)
}
