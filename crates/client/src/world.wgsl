struct Camera {
    view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    eye_time: vec4<f32>,
    sun: vec4<f32>,
    fog: vec4<f32>,
    points: array<vec4<f32>, 8>,
    point_colors: array<vec4<f32>, 8>,
    local_view_proj: array<mat4x4<f32>, 8>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var shadow_map: texture_depth_2d;
@group(1) @binding(1) var shadow_sampler: sampler_comparison;
@group(1) @binding(2) var fixture_shadows: texture_depth_2d_array;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) emission: f32,
};
struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) light_clip: vec4<f32>,
    @location(4) emission: f32,
};
@vertex fn vs_main(v: VertexInput) -> VertexOutput {
    var o: VertexOutput;
    o.clip = camera.view_proj * vec4<f32>(v.position, 1.0);
    o.position = v.position;
    o.normal = v.normal;
    o.color = v.color;
    // A small world-space offset avoids self-shadow acne without pushing
    // contact shadows centimetres away as a large depth-space bias did.
    o.light_clip = camera.light_view_proj * vec4<f32>(v.position + v.normal * 0.012, 1.0);
    o.emission = v.emission;
    return o;
}
@vertex fn vs_shadow(v: VertexInput) -> @builtin(position) vec4<f32> {
    return camera.light_view_proj * vec4<f32>(v.position, 1.0);
}
fn hash(p: vec3<f32>) -> f32 {
    var q = fract(p * 0.1031);
    q += vec3<f32>(dot(q, q.yzx + vec3<f32>(33.33)));
    return fract((q.x + q.y) * q.z);
}
fn noise(p: vec2<f32>) -> f32 {
    let cell = floor(p);
    let f = fract(p);
    let u = f * f * (vec2<f32>(3.0) - 2.0 * f);
    return mix(mix(hash(vec3<f32>(cell, 0.0)), hash(vec3<f32>(cell + vec2<f32>(1.0, 0.0), 0.0)), u.x),
        mix(hash(vec3<f32>(cell + vec2<f32>(0.0, 1.0), 0.0)), hash(vec3<f32>(cell + vec2<f32>(1.0), 0.0)), u.x), u.y);
}
// Depth changes across a sloping receiver. Comparing every PCF tap against
// the center depth stripes the surface; project its tangent plane instead.
fn depth_gradient(matrix: mat4x4<f32>, clip: vec4<f32>, normal: vec3<f32>) -> vec2<f32> {
    let axis = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(normal.y) > 0.9);
    let tangent = normalize(cross(normal, axis));
    let bitangent = cross(normal, tangent);
    let a = matrix * vec4<f32>(tangent, 0.0);
    let b = matrix * vec4<f32>(bitangent, 0.0);
    let ndc = clip.xyz / clip.w;
    let da = (a.xyz - ndc * a.w) / clip.w;
    let db = (b.xyz - ndc * b.w) / clip.w;
    let ua = da.xy * vec2<f32>(0.5, -0.5);
    let ub = db.xy * vec2<f32>(0.5, -0.5);
    let det = ua.x * ub.y - ua.y * ub.x;
    if (abs(det) < 0.00000001) { return vec2<f32>(0.0); }
    return vec2<f32>(da.z * ub.y - db.z * ua.y, ua.x * db.z - ub.x * da.z) / det;
}
fn fixture_visibility(index: i32, position: vec3<f32>, normal: vec3<f32>) -> f32 {
    let clip = camera.local_view_proj[index] * vec4<f32>(position + normal * 0.015, 1.0);
    if (clip.w <= 0.0) { return 0.0; }
    let ndc = clip.xyz / clip.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    if (any(uv <= vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) || ndc.z <= 0.0 || ndc.z >= 1.0) {
        return 0.0;
    }
    let texel = 1.0 / vec2<f32>(textureDimensions(fixture_shadows));
    let gradient = depth_gradient(camera.local_view_proj[index], clip, normal);
    // Linear filtering compares four neighboring texels at each tap.
    let bias = 0.00004 + dot(abs(gradient), texel) * 0.5;
    var sum = 0.0;
    for (var x = -1; x <= 1; x += 1) {
        for (var y = -1; y <= 1; y += 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel;
            sum += textureSampleCompareLevel(fixture_shadows, shadow_sampler,
                uv + offset, index, ndc.z + dot(gradient, offset) - bias);
        }
    }
    return sum / 9.0;
}
@fragment fn fs_main(v: VertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(v.normal);
    let sun = normalize(camera.sun.xyz);
    let light_ndc = v.light_clip.xyz / v.light_clip.w;
    let uv = light_ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    var visibility = 1.0;
    if (all(uv > vec2<f32>(0.0)) && all(uv < vec2<f32>(1.0)) && light_ndc.z > 0.0 && light_ndc.z < 1.0) {
        var sum = 0.0;
        let texel = 1.0 / vec2<f32>(textureDimensions(shadow_map));
        let gradient = depth_gradient(camera.light_view_proj, v.light_clip, n);
        let bias = 0.00003 + dot(abs(gradient), texel) * 0.5;
        for (var x = -1; x <= 1; x += 1) {
            for (var y = -1; y <= 1; y += 1) {
                let offset = vec2<f32>(f32(x), f32(y)) * texel;
                sum += textureSampleCompareLevel(shadow_map, shadow_sampler,
                    uv + offset, light_ndc.z + dot(gradient, offset) - bias);
            }
        }
        visibility = sum / 9.0;
    }
    var plane = v.position.xz;
    if (abs(n.x) > 0.7) { plane = v.position.zy; }
    if (abs(n.z) > 0.7) { plane = v.position.xy; }
    let footprint = max(length(dpdx(v.position)), length(dpdy(v.position)));
    let fine_detail = 1.0 - smoothstep(0.025, 0.12, footprint);
    let grain = mix(0.5, hash(floor(v.position * 28.0)), fine_detail);
    let grime = noise(plane * vec2<f32>(0.43, 0.24));
    var color = v.color * (0.94 + 0.04 * grain + 0.04 * grime);
    let steel = v.color.r < 0.14 && v.color.g < 0.18 && v.color.b < 0.20;
    let rust = v.color.r > v.color.g * 1.6 && v.color.r > 0.16 && v.color.g < 0.20;
    let concrete = abs(v.color.r - v.color.g) < 0.06 && v.color.r > 0.15 && v.color.b > 0.16;
    let hazard = v.color.r > 0.30 && v.color.g > 0.18 && v.color.b < 0.12;
    var specular = 0.018;
    var roughness_power = 16.0;
    if (concrete) {
        // Large cast panels and small form-tie pits, independent of voxel size.
        var panel_size = vec2<f32>(3.0, 2.0);
        if (abs(n.y) > 0.7) { panel_size = vec2<f32>(4.0); }
        let panel = fract(plane / panel_size);
        let edge = min(min(panel.x, 1.0 - panel.x), min(panel.y, 1.0 - panel.y));
        let joint = 1.0 - smoothstep(0.003, 0.010 + footprint * 0.08, edge);
        color *= 1.0 - joint * 0.20;
        let tie = length(abs(panel - vec2<f32>(0.5)) - vec2<f32>(0.36, 0.32));
        color *= 1.0 - (1.0 - smoothstep(0.009, 0.017, tie)) * 0.12 * fine_detail;
    }
    if (steel || rust) {
        let plate = fract(plane / vec2<f32>(2.0, 1.5));
        let edge = min(min(plate.x, 1.0 - plate.x), min(plate.y, 1.0 - plate.y));
        let joint = 1.0 - smoothstep(0.004, 0.014 + footprint * 0.08, edge);
        let rivet = length(abs(plate - vec2<f32>(0.5)) - vec2<f32>(0.43, 0.41));
        color *= 0.98 - joint * 0.22;
        color += vec3<f32>(0.035) * (1.0 - smoothstep(0.009, 0.018, rivet)) * fine_detail;
        if (rust) {
            let corrosion = noise(plane * 2.7);
            color *= mix(vec3<f32>(0.80, 0.74, 0.68), vec3<f32>(1.05, 0.98, 0.86), corrosion);
        }
        specular = select(0.085, 0.03, rust);
        roughness_power = 48.0;
    }
    if (hazard) {
        let stripe = step(0.5, fract((plane.x + plane.y) * 1.25));
        color = mix(vec3<f32>(0.026, 0.029, 0.030), color, stripe);
    }
    let ambient = mix(vec3<f32>(0.045, 0.055, 0.070), vec3<f32>(0.105, 0.13, 0.155), n.y * 0.5 + 0.5);
    var lit = color * (ambient + vec3<f32>(0.85, 0.88, 0.90) * max(dot(n, sun), 0.0) * visibility);
    for (var i = 0; i < 8; i += 1) {
        if (camera.points[i].w <= 0.0) { continue; }
        let delta = camera.points[i].xyz - v.position;
        let d2 = max(dot(delta, delta), 0.1);
        let to_light = delta * inverseSqrt(d2);
        // Broad downward fixture cone: 120 degrees, with a soft inner edge.
        // No unshadowed spill outside its projection or through nearby walls.
        let cone = smoothstep(0.5, 0.65, to_light.y);
        let ndotl = max(dot(n, to_light), 0.0);
        let range = camera.point_colors[i].w;
        let falloff = max(1.0 - d2 / (range * range), 0.0);
        if (cone <= 0.0 || ndotl <= 0.0 || falloff <= 0.0) { continue; }
        let attenuation = camera.points[i].w * falloff * falloff / (1.0 + d2);
        lit += color * camera.point_colors[i].rgb * attenuation * ndotl * cone * fixture_visibility(i, v.position, n);
    }
    let view = normalize(camera.eye_time.xyz - v.position);
    let half_vec = normalize(sun + view);
    lit += vec3<f32>(specular) * pow(max(dot(n, half_vec), 0.0), roughness_power) * visibility * max(dot(n, sun), 0.0);
    lit = mix(lit, v.color * 1.5, clamp(v.emission, 0.0, 1.0));
    let distance = length(camera.eye_time.xyz - v.position);
    let fog_amount = 1.0 - exp(-distance * distance * camera.fog.w);
    lit = mix(lit, camera.fog.rgb, clamp(fog_amount, 0.0, 0.85));
    // Lighting stays linear through tone mapping. An unorm surface needs the
    // display transfer here; an sRGB fallback surface performs it on write.
    var output = lit / (lit + vec3<f32>(0.82));
    if (camera.sun.w > 0.5) {
        let linear = max(output, vec3<f32>(0.0));
        output = select(1.055 * pow(linear, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055),
            linear * 12.92, linear <= vec3<f32>(0.0031308));
    }
    return vec4<f32>(output, 1.0);
}
