struct SkyOutput { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs_main(@builtin(vertex_index) index:u32) -> SkyOutput {
    var positions = array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
    var o: SkyOutput;
    o.position = vec4<f32>(positions[index], 0.0, 1.0);
    o.uv = positions[index] * 0.5 + vec2<f32>(0.5);
    return o;
}
fn cloud_hash(p: vec2<f32>) -> f32 {
    var q = fract(p * vec2<f32>(123.34, 456.21));
    q += vec2<f32>(dot(q, q + vec2<f32>(45.32)));
    return fract(q.x * q.y);
}
fn cloud_noise(p: vec2<f32>) -> f32 {
    let cell = floor(p);
    let f = fract(p);
    let u = f * f * (vec2<f32>(3.0) - 2.0 * f);
    return mix(mix(cloud_hash(cell), cloud_hash(cell + vec2<f32>(1.0, 0.0)), u.x),
        mix(cloud_hash(cell + vec2<f32>(0.0, 1.0)), cloud_hash(cell + vec2<f32>(1.0)), u.x), u.y);
}
@fragment fn fs_main(v:SkyOutput) -> @location(0) vec4<f32> {
    let horizon = vec3<f32>(0.16, 0.175, 0.18);
    let zenith = vec3<f32>(0.075, 0.09, 0.105);
    let clouds = cloud_noise(v.uv * vec2<f32>(5.0, 7.0)) * 0.65
        + cloud_noise(v.uv * vec2<f32>(11.0, 15.0)) * 0.35;
    var col = mix(horizon, zenith, smoothstep(0.15, 1.0, v.uv.y));
    col *= 0.86 + 0.25 * clouds;
    return vec4<f32>(col,1.0);
}
