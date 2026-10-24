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

@group(2) @binding(0) var albedo_map: texture_2d_array<f32>;
@group(2) @binding(1) var normal_map: texture_2d_array<f32>;
@group(2) @binding(2) var material_sampler: sampler;
struct Materials { values: array<vec4<f32>,64> };
@group(2) @binding(3) var<uniform> materials: Materials;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) emission: f32,
    @location(4) uv: vec2<f32>,
    @location(5) material: u32,
    @location(6) instance: vec4<f32>,
};
struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) light_clip: vec4<f32>,
    @location(4) emission: f32,
    @location(5) uv: vec2<f32>,
    @location(6) @interpolate(flat) material: u32,
};
fn rotate_y(p: vec3<f32>, yaw: f32) -> vec3<f32> {
    let c = cos(yaw); let s = sin(yaw);
    return vec3<f32>(c*p.x + s*p.z, p.y, -s*p.x + c*p.z);
}
@vertex fn vs_main(v: VertexInput) -> VertexOutput {
    let position = rotate_y(v.position,v.instance.w) + v.instance.xyz;
    let normal = rotate_y(v.normal,v.instance.w);
    var o: VertexOutput;
    o.clip = camera.view_proj * vec4<f32>(position, 1.0);
    o.position = position;
    o.normal = normal;
    o.uv = v.uv;
    o.material = v.material;
    o.color = v.color;
    // A small world-space offset avoids self-shadow acne without pushing
    // contact shadows centimetres away as a large depth-space bias did.
    o.light_clip = camera.light_view_proj * vec4<f32>(position + normal * 0.012, 1.0);
    o.emission = v.emission;
    return o;
}
@vertex fn vs_shadow(v: VertexInput) -> @builtin(position) vec4<f32> {
    return camera.light_view_proj * vec4<f32>(rotate_y(v.position,v.instance.w) + v.instance.xyz, 1.0);
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
    let geometric_normal = normalize(v.normal);
    var n = geometric_normal;
    var color = v.color;
    var roughness = 0.8;
    var metallic = 0.0;
    var ao = 1.0;
    var emission = v.emission;
    // Explicit derivatives remain valid across different material triangles.
    let px = dpdx(v.position); let py = dpdy(v.position);
    let tx = dpdx(v.uv); let ty = dpdy(v.uv);
    if (v.material < 64u) {
        let base = textureSampleGrad(albedo_map, material_sampler, v.uv, i32(v.material), tx, ty);
        let detail = textureSampleGrad(normal_map, material_sampler, v.uv, i32(v.material), tx, ty);
        color = base.rgb * v.color;
        ao = base.a;
        roughness = clamp(detail.a, 0.08, 1.0);
        metallic = materials.values[v.material].x;
        emission = max(emission, materials.values[v.material].y);
        let determinant = tx.x * ty.y - tx.y * ty.x;
        if (abs(determinant) > 0.00000001) {
            let tangent = (px * ty.y - py * tx.y) / determinant;
            let bitangent = (py * tx.x - px * ty.x) / determinant;
            let t = normalize(tangent - n * dot(n,tangent));
            let b = normalize(cross(n,t)) * sign(dot(cross(n,t),bitangent));
            n = normalize(mat3x3<f32>(t,b,n) * (detail.xyz * 2.0 - 1.0));
        }
    }
    let specular = mix(vec3<f32>(0.025), color * 0.25, metallic);
    let roughness_power = mix(180.0, 4.0, roughness * roughness);
    let sun = normalize(camera.sun.xyz);
    let light_ndc = v.light_clip.xyz / v.light_clip.w;
    let uv = light_ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    var visibility = 1.0;
    if (all(uv > vec2<f32>(0.0)) && all(uv < vec2<f32>(1.0)) && light_ndc.z > 0.0 && light_ndc.z < 1.0) {
        var sum = 0.0;
        let texel = 1.0 / vec2<f32>(textureDimensions(shadow_map));
        let gradient = depth_gradient(camera.light_view_proj, v.light_clip, geometric_normal);
        let bias = 0.00003 + dot(abs(gradient), texel) * 0.5;
        for (var x = -1; x <= 1; x += 1) {
            for (var y = -1; y <= 1; y += 1) {
                let offset = vec2<f32>(f32(x), f32(y)) * texel;
                sum += textureSampleCompareLevel(shadow_map, shadow_sampler,
                    uv + offset, light_ndc.z + dot(gradient, offset) - bias);
            }
        }
        // Fade the bounded camera-following shadow window before its edge.
        // Distant terrain remains sunlit instead of acquiring a moving hard seam.
        let edge = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
        visibility = mix(1.0, sum / 9.0, smoothstep(0.0, 0.08, edge));
    }
    let ambient = mix(vec3<f32>(0.045, 0.055, 0.070), vec3<f32>(0.105, 0.13, 0.155), n.y * 0.5 + 0.5);
    var lit = color * (ambient * ao + vec3<f32>(0.85, 0.88, 0.90) * max(dot(n, sun), 0.0) * visibility);
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
        lit += color * camera.point_colors[i].rgb * attenuation * ndotl * cone * fixture_visibility(i, v.position, geometric_normal);
    }
    let view = normalize(camera.eye_time.xyz - v.position);
    let half_vec = normalize(sun + view);
    lit += specular * pow(max(dot(n, half_vec), 0.0), roughness_power) * visibility * max(dot(n, sun), 0.0);
    lit = mix(lit, color * 1.8, clamp(emission, 0.0, 1.0));
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
