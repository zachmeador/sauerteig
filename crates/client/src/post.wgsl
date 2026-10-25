struct Params { exposure: f32, bloom: f32, encode: f32, padding: f32 };
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var bloom: texture_2d<f32>;
@group(0) @binding(3) var<uniform> params: Params;
struct Output { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> Output {
    let p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var o: Output;
    o.position = vec4<f32>(p[index], 0.0, 1.0);
    o.uv = p[index] * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    return o;
}
fn down(uv: vec2<f32>) -> vec3<f32> {
    let d = 0.5 / vec2<f32>(textureDimensions(source));
    return (textureSampleLevel(source, linear_sampler, uv + d, 0.0).rgb
        + textureSampleLevel(source, linear_sampler, uv - d, 0.0).rgb
        + textureSampleLevel(source, linear_sampler, uv + d * vec2<f32>(1.0, -1.0), 0.0).rgb
        + textureSampleLevel(source, linear_sampler, uv + d * vec2<f32>(-1.0, 1.0), 0.0).rgb) * 0.25;
}
@fragment fn fs_threshold(v: Output) -> @location(0) vec4<f32> {
    let color = down(v.uv);
    let brightness = max(color.r, max(color.g, color.b));
    // A soft knee around radiance 1 avoids a hard halo threshold.
    let knee = clamp(brightness - 0.5, 0.0, 1.0);
    let contribution = max(brightness - 1.0, knee * knee * 0.5) / max(brightness, 0.00001);
    return vec4<f32>(color * contribution, 1.0);
}
@fragment fn fs_down(v: Output) -> @location(0) vec4<f32> { return vec4<f32>(down(v.uv), 1.0); }
@fragment fn fs_up(v: Output) -> @location(0) vec4<f32> {
    let d = 1.0 / vec2<f32>(textureDimensions(source));
    var color = vec3<f32>(0.0);
    for (var y = -1; y <= 1; y += 1) {
        for (var x = -1; x <= 1; x += 1) {
            let weight = select(1.0, 2.0, x == 0) * select(1.0, 2.0, y == 0);
            color += textureSampleLevel(source, linear_sampler, v.uv + vec2<f32>(f32(x), f32(y)) * d, 0.0).rgb * weight;
        }
    }
    return vec4<f32>(color * (0.5 / 16.0), 0.0);
}
@fragment fn fs_post(v: Output) -> @location(0) vec4<f32> {
    let radiance = textureSampleLevel(source, linear_sampler, v.uv, 0.0).rgb
        + textureSampleLevel(bloom, linear_sampler, v.uv, 0.0).rgb * params.bloom;
    let x = max(radiance * params.exposure, vec3<f32>(0.0));
    // ACES fitted filmic curve: highlight roll-off with a bounded display output.
    var color = clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), vec3<f32>(0.0), vec3<f32>(1.0));
    if (params.encode > 0.5) {
        color = select(1.055 * pow(color, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055),
            color * 12.92, color <= vec3<f32>(0.0031308));
    }
    return vec4<f32>(color, 1.0);
}
