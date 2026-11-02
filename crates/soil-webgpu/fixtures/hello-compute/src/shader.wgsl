@group(0) @binding(0)
var<storage, read_write> data: array<u32>;

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
  let i = id.x;
  var n = data[i];
  var steps = 0u;
  loop {
    if (n == 1u) { break; }
    if (n % 2u == 0u) { n = n / 2u; } else { n = n * 3u + 1u; }
    steps = steps + 1u;
  }
  data[i] = steps;
}
