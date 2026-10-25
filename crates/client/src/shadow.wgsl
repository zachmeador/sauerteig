@group(0) @binding(0) var<uniform> light: LightCamera;
@vertex fn vs_main(@location(0) position: vec3<f32>, @location(6) instance: vec4<f32>) -> @builtin(position) vec4<f32> {
    let p = rotate_y(position, instance.w) + instance.xyz;
    return light.view_proj * vec4<f32>(p, 1.0);
}
// A triangle covering its viewport at the far depth, to clear one tile of a
// page that other tiles share.
@vertex fn vs_clear(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 1.0, 1.0);
}
