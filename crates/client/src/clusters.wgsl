@group(0) @binding(0) var<uniform> params: LightParams;
// View-space centre and radius of each light, in the same order as the light buffer.
@group(0) @binding(1) var<storage, read> light_view: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> masks: array<u32>;

// One thread per cluster: set a bit for every light whose sphere reaches the
// cluster's view-space bounds.
@compute @workgroup_size(64)
fn cs_cull(@builtin(global_invocation_id) id: vec3<u32>) {
    let gx = params.grid.x;
    let gy = params.grid.y;
    let gz = params.grid.z;
    let cluster = id.x;
    if (cluster >= gx * gy * gz) { return; }
    let x = cluster % gx;
    let y = (cluster / gx) % gy;
    let z = cluster / (gx * gy);
    let split = params.depth.y;
    let ratio = params.depth.z;
    var near_depth = params.depth.x;
    var far_depth = split;
    if (z > 0u) {
        near_depth = split * pow(ratio, f32(z - 1u));
        far_depth = select(split * pow(ratio, f32(z)), params.range.x, z == gz - 1u);
    }
    let left = -1.0 + 2.0 * f32(x) / f32(gx);
    let right = -1.0 + 2.0 * f32(x + 1u) / f32(gx);
    let top = 1.0 - 2.0 * f32(y) / f32(gy);
    let bottom = 1.0 - 2.0 * f32(y + 1u) / f32(gy);
    var low = vec3<f32>(1.0e30);
    var high = vec3<f32>(-1.0e30);
    for (var k = 0u; k < 8u; k += 1u) {
        let depth = select(near_depth, far_depth, (k & 4u) != 0u);
        let nx = select(left, right, (k & 1u) != 0u);
        let ny = select(top, bottom, (k & 2u) != 0u);
        let corner = vec3<f32>(nx * depth / params.screen.z, ny * depth / params.screen.w, -depth);
        low = min(low, corner);
        high = max(high, corner);
    }
    var mask = array<u32, MASK_WORDS>();
    let count = min(params.grid.w, MASK_WORDS * 32u);
    for (var i = 0u; i < count; i += 1u) {
        let light = light_view[i];
        let offset = clamp(light.xyz, low, high) - light.xyz;
        if (dot(offset, offset) <= light.w * light.w) {
            mask[i >> 5u] |= 1u << (i & 31u);
        }
    }
    for (var w = 0u; w < MASK_WORDS; w += 1u) {
        masks[cluster * MASK_WORDS + w] = mask[w];
    }
}
